# Roadmap — vagus as a self-contained retrieval universe

This is the durable "where we're going" map. It lays out the **three-tier retrieval** direction, says
**where each capability lives**, and tracks **what's shipped vs. next**. For the *why* behind each
decision, follow the linked ADR. Binding invariants live in [`guardrails.md`](./guardrails.md).

## The shape

vagus is **a self-contained Rust universe with no versioned runtime** ([ADR 0014](./adr/0014-self-contained-universe.md)) —
no Python/Node/TS to manage; statically-linked native inference libs (onnxruntime today; candle/ggml
where justified) are in-character. Retrieval comes in **three tiers**, selected by *channel*
([ADR 0012](./adr/0012-three-tier-retrieval.md)):

```
tier 0  (floor)        vagus search "q"            BM25 + cosine + RRF(k=60)            — deterministic, <1s
tier 1  (shell+local)  vagus search "q" --smart    + local rewrite/HyDE + rerank       — offline, no Claude
tier 2  (skill+Opus)   /search                     Opus expansion+HyDE+full-body judge  — SOTA, on top of the CLI
```

Tiers 1 and 2 are **parallel**: same retrieval + rerank core, same typed `lex:/vec:/hyde:` discipline —
they differ only in *who generates the rewrite* (a local model vs. Opus). RRF is never modified (G8);
reranking is a separate post-fusion stage. qmd's weighted-RRF / top-rank bonus / position-blend are
**rejected** as G8 breaches.

## Where each capability lives

| Capability | Home | Engine / model | Status |
|---|---|---|---|
| BM25 + cosine + RRF (tier 0) | core `vagus` | tantivy + brute-force cosine | shipped |
| Embedder | core `vagus` | fastembed/ort — **EmbeddingGemma-300M** (768-dim, 2048 ctx) | shipped (ADR 0006) |
| Token-budgeted chunking + code atomicity | core `vagus` | dep-free (`chars/3.5`) | shipped (ADR 0013) |
| Cross-encoder reranker (`--rerank`) | core `vagus` | fastembed/ort — **jina-reranker-v1-turbo-en** | shipped (ADR 0015) |
| `--full` / `--min-score` (skill enablers) | core `vagus` | — | shipped |
| Local generative rewriter/HyDE (`--smart`/`--rewrite`, tier 1) | core `vagus` (feature-gated) | **candle** — qmd's `qmd-query-expansion-1.7B` GGUF | **next (milestone 2)** (ADR 0016) |
| Opus expansion + HyDE + full-body judge (tier 2) | `/search` skill | Opus | **next (milestone 3)** |
| Networked capture (Slack, GitHub, …) | `vagus-<name>` plugins | per-plugin | shipped mechanism (ADR 0010/0011) |

**Why advanced search is *not* a plugin:** the plugin protocol is capture-shaped (one-way
note→index, stdin inherited) and the reranker/rewriter are neither networked nor a foreign runtime —
so they belong in core. Plugins (G18) stay scoped to networked capture.

**Aping qmd — per component:** embedder = ape the *model* (EmbeddingGemma, runs on our ort stack);
rewriter = ape the *model* (its fine-tuned GGUF, via candle) + the typed-output *protocol*; reranker =
**deviate** (jina cross-encoder, because qmd's Qwen3-Reranker is a decoder that can't ride fastembed).

## Milestones

- **M0 — design overhaul** *(this round)*: this roadmap, ADRs 0012–0016, the [identity reframe](./adr/0014-self-contained-universe.md),
  guardrail edits (G4/G7/G8/G9/G17/G19/G20). ✅
- **M1 — strong core** *(this round)*: EmbeddingGemma-300M + token-budgeted chunking (one reindex);
  in-core `--rerank`; `--full` / `--min-score`. ✅ (verified end-to-end)
- **M2 — tier-1 local generation** *(next)*: in-core candle rewriter; `vagus rewrite` + `vagus search
  --smart`; typed `lex:/vec:/hyde:` routing + multi-query fuse + rerank. ([ADR 0016](./adr/0016-local-generative-rewriter.md))
- **M3 — tier-2 SOTA skill** *(next)*: rewrite `skills/search/SKILL.md` as the Opus pipeline over
  `vagus search --json --full --rerank --min-score` (expansion + HyDE + full-body 0–3 judge + cite).

## Deferred / not building

- RRF weighting / top-rank bonus / position-blend (breach G8 — revisit only with an ADR + G8 edit).
- True cosine-MMR / ranked per-note cap (`PER_FILE_CAP=3` already curbs display dominance).
- A real tokenizer in the chunk hot path (the `chars/3.5` heuristic suffices — G11).
- llama-cpp-2 engine (adds cmake) — fallback only if candle's Qwen3 support regresses.
- Quantized-Gemma via custom-ONNX (a footprint lever for later).
- ANN vector backend (brute-force cosine is sub-few-ms at personal scale).
