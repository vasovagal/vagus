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
  `llama-cpp-2`). Its `candle-transformers::quantized_qwen3` loads GGUF directly.
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
  default-on, with `--no-default-features` for a lean candle-free build (CI guards that lane).
- **Latency (verified, release, Apple Silicon, model cached):** `vagus rewrite` ≈ 5–6 s, `vagus search
  --smart` ≈ 8 s end-to-end (expand → multi-query fuse → rerank). This is an **opt-in** mode, *not* the
  fast path — bare `vagus search` (tier-0) stays < 1 s. (Debug builds are ~10–15× slower; candle's CPU
  gemm only optimizes under `--release`.) First use downloads ~1.28 GB to the cache (outside iCloud).
- Built in **milestone 2**; the `--full`/`--min-score`/`--rerank` primitives it composes ship first.
