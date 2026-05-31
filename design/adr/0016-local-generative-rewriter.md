# ADR 0016 — Local generative query rewriter (tier-1, in-core via candle)

- **Status:** Accepted + **implemented (2026-05-30)** — `src/rewrite.rs` behind the default-on
  `generate` feature; `vagus rewrite` + `vagus search --smart`. Amends G17.

## Context

Tier-1 ([ADR 0012](./0012-three-tier-retrieval.md)) makes `vagus search` smart *without Claude*. The
reranker ([ADR 0015](./0015-cross-encoder-rerank.md)) gives precision; the missing half is **query
expansion + HyDE** — generating alternate query phrasings and a hypothetical answer passage to widen
recall. HyDE *fundamentally* needs generation; expansion benefits from it. That requires a small
generative LLM running locally — "better than nothing" when Opus isn't in the loop.

This is the one piece that is a generative LLM. The earlier (two-tier) plan parked it entirely on Opus.
The author's decision: **support both** — a local rewriter for the shell *and* Opus in the skill. Under
the reframed identity ([ADR 0014](./0014-self-contained-universe.md)), a self-contained local generative
model is in-character, because "no versioned runtime" forbids Python/Node — not a static native lib.

## Decision

A **local generative rewriter, compiled into `vagus`** (the author chose in-core over a companion
binary), behind a **cargo feature** (default-on in releases; a lean build can exclude it):

- **Engine: `candle`** (pure Rust, no new system build dep — notably **no cmake**, unlike
  `llama-cpp-2`). Its `candle-transformers::quantized_qwen3` loads GGUF directly. **On macOS the Metal
  GPU backend is enabled** (`candle-core`/`-transformers` `metal` feature, declared in a
  `[target.'cfg(target_os = "macos")']` block so it feature-unions only on macOS — the Linux release
  targets and the `--no-default-features` lean lane compile unchanged). The quantized decode runs on
  the Apple GPU, falling back to CPU if Metal can't initialize (`rewrite::select_device`); every other
  platform decodes on CPU.
- **Model: qmd's fine-tuned `tobil/qmd-query-expansion-1.7B-gguf`** (or a leaner Qwen3-0.6B) — we *ape
  qmd's expansion model* here (candle runs its exact GGUF). Lazily downloaded to
  `~/Library/Caches/vagus/models` (G1/G6); ~0.4–1.3 GB depending on size/quant. ~1–4s CPU for a few
  short variants on Apple Silicon.
- **Output protocol (aped from qmd, model-agnostic):** one pass emits typed lines —
  `lex:` (→ BM25), `vec:` (→ semantic), `hyde:` (a hypothetical answer passage → `--mode vec`). The
  original query is always retained.
- **Surface:** `vagus rewrite "q"` prints the typed lines; `vagus search "q" --smart` runs the rewrite
  in-process, issues one retrieval per typed variant, fuses (RRF), and reranks (`--rerank`). Offline,
  **no daemon** (G14) — one-shot per search.
- The skill (tier-2) emits the **same** typed-line discipline with Opus instead of the local model —
  the pipelines are parallel.

## Alternatives considered

- **`llama-cpp-2`** (static C++ like onnxruntime — also self-contained, also runs qmd's GGUF *and*
  Qwen3-Reranker via rank-pooling) — viable, but adds **cmake** to dev + CI and a from-source compile.
  candle keeps the build a plain `cargo build`. Tracked as a fallback if candle's Qwen3 support
  regresses.
- **A `vagus-rewrite` companion binary** — rejected per the author's in-core choice. (It would also
  have needed a new search-time request/response contract; the capture NDJSON protocol — ADR 0011 —
  doesn't fit. Reconsider only if candle's dep weight on the core build becomes a problem.)
- **Opus-only (the old two-tier plan)** — rejected as insufficient: it leaves the shell at the RRF
  floor when Claude is absent. Opus remains tier-2 (strictly higher quality, zero disk) — we keep both.
- **Classical, non-generative expansion (RM3/PRF)** — the only *generation-free* in-core option;
  retained as a possible tier-0/1 recall aid, but it can't do HyDE. Not a substitute.

## Consequences

- **G17 is superseded** by the tiered statement (see guardrails): the core binary *may* contain a
  feature-gated, lazily-downloaded local generative rewriter (tier-1); generative work otherwise runs
  in the Opus skill (tier-2); tier-0 has none. No cloud, no daemon in any tier (G14).
- candle is a new heavyweight dependency → this ADR is its gate (G11). Re-verify the binary stays
  self-contained with `otool -L` (G13). It adds ~17 MB to the binary; the `generate` feature is
  default-on, with `--no-default-features` for a lean candle-free build (CI guards that lane). The
  macOS Metal backend links only system frameworks (`Metal`/`Foundation`/`CoreFoundation`) —
  re-verified self-contained via `otool -L` (system frameworks/dylibs only).
- **Latency (verified, release, Apple Silicon, model cached):** the `--smart` pipeline is **~5 s on a
  cold query / ~2.3 s on a repeat**, down from ~9.5 s, via three optimizations (perf pass, 2026-05-30):
  - **Overlapped model loads (Fix B):** the embedder (~2 s) and reranker (~0.15 s) ONNX models load on
    background threads that overlap the multi-second LLM decode instead of running serially after it.
    Still no daemon (G14) — the threads are joined within the one-shot process.
  - **Metal decode (Fix A, macOS):** the quantized rewriter decodes on the Apple GPU (~2.5× faster
    decode; partly offset by a ~0.6 s GPU weight-upload at load).
  - **Expansion cache (Fix C):** the deterministic (fixed-seed) expansion is cached in `meta.db`
    (`expansion_cache`) keyed on query + model identity + sampling params, so a repeat query skips the
    LLM entirely (load + decode). The `MAX_NEW_TOKENS` ceiling was also tightened 512 → 192 (Fix D) to
    bound a pathological non-terminating generation.

  This is still an **opt-in** mode — bare `vagus search` (tier-0) stays < 1 s. (Debug builds are
  ~10–15× slower; candle only optimizes under `--release`.) First use downloads ~1.28 GB to the cache
  (outside iCloud). `vagus search --timings` prints the per-stage breakdown.
- Built in **milestone 2**; the `--full`/`--min-score`/`--rerank` primitives it composes ship first.
