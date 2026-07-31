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
tier 0  (floor)        vagus search "q"            BM25 + cosine + RRF(k=60)            — deterministic, interactive
tier 1  (shell+local)  vagus search "q" --smart    + local rewrite/HyDE + rerank       — offline, no agent
tier 2  (skill+Opus)   search Agent Skill           bounded exact+rerank+body judge      — concise, on top of CLI
```

Tiers 1 and 2 share the same retrieval + rerank core but use different context budgets. Tier 1 owns
the typed `lex:/vec:/hyde:` rewrite; tier 2 normally judges one bounded full-body set and permits one
modality-selected fallback only when nothing useful survives. Production RRF is never modified and
reranking stays separate. Same-pool alternate fusion is allowed only as an explicit ADR 0025
fixed-gate experiment; no alternate is currently shipped or default.

## Where each capability lives

| Capability | Home | Engine / model | Status |
|---|---|---|---|
| BM25 + cosine + RRF (tier 0) | core `vagus` | exact <10k; **usearch HNSW** ≥10k; all-mode `--exact` oracle | shipped (ADRs 0003/0019) |
| Embedder | core `vagus` | fastembed/ort — **EmbeddingGemma-300M** (768-dim, 2048 ctx) | shipped (ADR 0006) |
| Token-budgeted chunking + code atomicity | core `vagus` | dep-free (`chars/3.5`) | shipped (ADR 0013) |
| Mtime-windowed forced reindex (`reindex --since`) | core `vagus` | full-vault snapshot + persisted G5 replacement + incomplete-row retry | shipped (ADR 0022) |
| Fail-closed vault onboarding + network-free doctor | core `vagus` | alias-aware paths; explicit model-fetch consent | shipped (ADRs 0004/0006/0015) |
| Cross-encoder reranker (`--rerank`, optional `--rerank-context 1|2`) | core `vagus` | fastembed/ort — **jina-reranker-v1-turbo-en**; exact pair-token budgeting | shipped (ADR 0015) |
| `--full` / `--min-score` (skill enablers) | core `vagus` | — | shipped |
| Frontmatter filters (`--since` / `--source`) | core `vagus` | SQLite post-rank stage (no tantivy change) | shipped (ADR 0017) |
| Note-level results by default (+ `--chunks` opt-out) | core `vagus` | post-rank dedup stage (RRF untouched) | shipped (ADR 0020) |
| Adaptive context-tidy result ceiling (+ `--exhaustive`) | core `vagus` | robust RRF knee + source-champion veto (RRF untouched) | shipped (ADR 0023) |
| Opt-in semantic relevance (`--relevance` / explicit floor) | core `vagus` | named finite original-query cosine heuristic; post-truncation/no backfill | shipped (ADR 0026) |
| Reproducible retrieval evaluation + fusion gate | core `vagus` | `vagus eval` schema 2 + named relevance diagnostic + fixed paired `eval-gate` | shipped (ADRs 0024–0026) |
| Atomic cited-note presentation provenance | core + tier-2 skill | strict run/event schema, binary+pipeline+corpus identity, capped-tail truth | shipped (ADR 0021 amendment) |
| Local generative rewriter/HyDE (`vagus rewrite`, `search --smart`, tier 1) | core `vagus` (feature-gated `generate`) | **candle** — qmd's `qmd-query-expansion-1.7B` GGUF | shipped (ADR 0016) |
| Bounded exact+reranked full-body judge (tier 2) | search Agent Skill (Claude Code / pi) | Opus (10 candidates; grade≥2; max 6) | **shipped (milestone 3)** |
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
  in-core `--rerank` with bounded tokenizer-safe small-to-big context; `--full` / `--min-score`. ✅
  (verified end-to-end; context radius 0 remains the default)
- **M2 — tier-1 local generation** *(shipped)*: in-core candle rewriter behind the default-on
  `generate` feature; `vagus rewrite` + `vagus search --smart`; typed `lex:/vec:/hyde:` routing +
  multi-query fuse + rerank; lazily downloads qmd's 1.7B GGUF (~1.28GB). ([ADR 0016](./adr/0016-local-generative-rewriter.md))
- **M3 — tier-2 bounded skill** *(shipped; tightened 2026-07-30)*: `skills/search/SKILL.md` runs
  the fixed `vagus search --json --full --rerank --exact --limit 10 --tick-provenance` path at
  rerank-context radius 0, judges full bodies 0–3, presents only nonredundant grade≥2 evidence (max 6,
  never pads), and permits one query-shape-selected fallback only when none survive. The primary run
  carries self-verifying pipeline/corpus identity and strict rank/cap states; one atomic tick records
  only cited paths, with query content off. Fallbacks are counter-only. Judging stays in the skill
  (G17); RRF is never re-derived (G8). The same Agent Skill installs for Claude Code and pi.

## Deferred / not building

- Default RRF replacement. Same-pool explicit experiments now have ADR 0025's fixed gate, but none
  is accepted; promotion needs a new ADR with context/latency/default+`--exhaustive` evidence.
- True cosine-MMR (still deferred). The **ranked per-note cap shipped** as the note-level default —
  a post-rank dedup stage, [ADR 0020](./adr/0020-note-level-results.md); `PER_FILE_CAP=3` remains
  for `--chunks` display.
- A real tokenizer in the chunk hot path (the `chars/3.5` heuristic suffices — G11).
- llama-cpp-2 engine (adds cmake) — fallback only if candle's Qwen3 support regresses.
- Quantized-Gemma via custom-ONNX (a footprint lever for later).
- ~~ANN vector backend~~ — **shipped** as embedded usearch HNSW ([ADR 0019](./adr/0019-usearch-ann-backend.md)),
  adopted ahead of the >500k-chunk trajectory. Exact brute force is automatic below 10k because it
  recovered a corpus answer HNSW missed; `--exact` forces the same oracle at every scale/mode.
