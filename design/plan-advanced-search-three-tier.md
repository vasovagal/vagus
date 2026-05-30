# Plan — Three-tier retrieval ("qmd-class" search as a self-contained Rust universe)

> **Status:** M0 (design) + M1 (strong core) **shipped & verified** (2026-05-30); M2 (local rewriter)
> and M3 (Opus skill) are next. Supersedes
> [`plan-two-tier-search-and-embedding-upgrade.md`](./plan-two-tier-search-and-embedding-upgrade.md).
> The durable map is [`roadmap.md`](./roadmap.md); the *why* is in ADRs 0012–0016.

## Context

`vagus` retrieval was a basic hybrid floor (BM25 + cosine + RRF k=60, bge-small/384/512-ctx, no
rerank/expansion). Goal: reach `tobi/qmd`-class quality with **two self-sufficient tiers** — the shell
smart on its own with local models, the `/search` skill SOTA with Opus — under a **"no versioned
runtime"** identity ([ADR 0014](./adr/0014-self-contained-universe.md)). This turned the old *two-tier*
plan into a **three-tier** contract ([ADR 0012](./adr/0012-three-tier-retrieval.md)) and un-parked the
local rewriter. Engine/model feasibility was verified by research workflows (see ADR 0015/0016).

## Decisions (locked)

1. **Three tiers, channel-selected:** 0 = pure RRF floor; 1 = shell + local models
   (`--smart`/`--rerank`/`--rewrite`); 2 = `/search` skill + Opus. Parallel pipelines, shared core.
2. **Reranker in core**, `jina-reranker-v1-turbo-en` via fastembed/ort — rides the existing stack
   ([ADR 0015](./adr/0015-cross-encoder-rerank.md)). Deviates from qmd's decoder reranker by design.
3. **Local rewriter in core** (feature-gated), candle running qmd's fine-tuned Qwen3-1.7B GGUF +
   typed `lex:/vec:/hyde:` protocol ([ADR 0016](./adr/0016-local-generative-rewriter.md)) — milestone 2.
4. **EmbeddingGemma-300M** (768-dim, 2048-ctx) + token-budgeted chunking
   ([ADR 0006](./adr/0006-embeddings-local-no-daemon.md), [ADR 0013](./adr/0013-chunk-budget.md)).
5. RRF untouched (G8); reranking is a separate post-fusion stage; qmd's RRF extras rejected.

## Shipped this round (M0 + M1)

- **Docs:** this plan, [`roadmap.md`](./roadmap.md), ADRs 0012–0016, ADR 0003/0006 amendments,
  guardrails G4/G7/G8/G9/G17/G19/G20, `requirements.md` identity reframe, `CLAUDE.md` mirror.
- **`src/config.rs`:** `EMBED_MODEL=google/embeddinggemma-300m`, `EMBED_DIMS=768`, `CHUNK_VERSION=3`.
- **`src/embed.rs`:** `EmbeddingGemma300M` via `TextInitOptions` + `with_max_length(2048)`; query prefix
  `task: search result | query:`, document prefix `title: none | text:`; L2-normalized.
- **`src/chunk.rs`:** typed prose/code segments; sub-split sections > ~900 tokens (`chars/3.5`) with
  ~128 overlap; **fenced code kept atomic**.
- **`src/rerank.rs` (new) + `src/search.rs` + `src/main.rs`:** `--rerank` re-scores the deeper fused
  pool on full bodies (sigmoid display score, raw logit in `Hit.rerank`), truncates to `--limit`;
  `--full` adds `Hit.body`; `--min-score` relative-to-top floor. Default `--json` byte-identical (G9a).
- **`src/index.rs`:** one-line stderr notice when the format-change auto-reindex fires.
- **`RELEASING.md`:** upgrade note (auto-reindex; run `vagus reindex` once; ~1.23GB model cache).

Verified end-to-end on a temp vault: `doctor` shows `google/embeddinggemma-300m / 768`; long note →
multiple chunks; oversize code stays in one chunk; `--rerank` reorders by cross-encoder; `--full`/`
--min-score` work; default JSON shape unchanged; semantic ordering sensible.

## Next (M2, M3)

- **M2 — local rewriter:** `src/rewrite.rs` (feature-gated candle), `vagus rewrite`, `vagus search
  --smart` (typed-variant routing → multi-query fuse → rerank). ([ADR 0016](./adr/0016-local-generative-rewriter.md))
- **M3 — Opus skill:** rewrite `skills/search/SKILL.md` over `vagus search --json --full --rerank
  --min-score` (expansion + conditional HyDE + full-body 0–3 judge + quality floor + cite
  `path > heading`); add `Read(~/brain/**)` + the iCloud-canonical path to `allowed-tools`.
