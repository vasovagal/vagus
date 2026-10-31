# Roadmap — vagus as a self-contained retrieval universe

Find the current retrieval tiers, capability owners, completed milestones, and deferred work below.
The ADRs record the decisions; [`guardrails.md`](./guardrails.md) holds the binding invariants.
“Implemented” describes the current source tree, not a promise about an installed release.
Opus is the intended tier-2 model in ADR 0012; the skill uses the host-selected model and does not
enforce that choice.

## The shape

Vagus runs without a Python, Node, or TypeScript runtime; native inference libraries are statically
linked ([ADR 0014](./adr/0014-self-contained-universe.md)). The calling channel selects one of
three retrieval tiers ([ADR 0012](./adr/0012-three-tier-retrieval.md)):

```
tier 0  (floor)        vagus search "q"            BM25 + cosine + RRF(k=60)            — deterministic, interactive
tier 1  (shell+local)  vagus search "q" --smart    + local rewrite/HyDE + rerank       — offline, no agent
tier 2  (skill+Opus)   search Agent Skill           bounded exact+rerank+body judge      — concise, on top of CLI
```

Tiers 1 and 2 share the same retrieval + rerank core but use different context budgets. Tier 1 owns
the typed `lex:/vec:/hyde:` rewrite; tier 2 normally judges one bounded full-body set and permits one
modality-selected fallback only when nothing useful survives. Production RRF is never modified and
reranking stays separate. Same-pool alternate fusion is allowed only as an explicit ADR 0025
fixed-gate experiment; no alternate is implemented or default.

## Where each capability lives

| Capability | Home | Engine / model | Status |
|---|---|---|---|
| BM25 + cosine + RRF (tier 0) | core `vagus` | exact <10k; **usearch HNSW** ≥10k; all-mode `--exact` oracle | implemented (ADRs 0003/0019) |
| Embedder | core `vagus` | fastembed/ort — **EmbeddingGemma-300M** (768-dim, 2048 ctx) | implemented (ADR 0006) |
| Token-budgeted chunking + code atomicity | core `vagus` | dep-free (`chars/3.5`) | implemented (ADR 0013) |
| Mtime-windowed forced reindex (`reindex --since`) | core `vagus` | full-vault snapshot + persisted G5 replacement + incomplete-row retry | implemented (ADR 0022) |
| Fail-closed vault onboarding + network-free doctor | core `vagus` | alias-aware paths; explicit model-fetch consent | implemented (ADRs 0004/0006/0015) |
| Cross-encoder reranker (`--rerank`, optional `--rerank-context 1\|2`) | core `vagus` | fastembed/ort — **jina-reranker-v1-turbo-en**; exact pair-token budgeting | implemented (ADR 0015) |
| `--full` / `--min-score` (skill enablers) | core `vagus` | — | implemented |
| Note-time filters (`search`/`inbox --since`; search `--source`) | core `vagus` | shared duration/timestamp rules; SQLite post-rank search stage | implemented (ADR 0017) |
| Note-level results by default (+ `--chunks` opt-out) | core `vagus` | post-rank dedup stage (RRF untouched) | implemented (ADR 0020) |
| Adaptive context-tidy result ceiling (+ `--exhaustive`) | core `vagus` | robust RRF knee + source-champion veto (RRF untouched) | implemented (ADR 0023) |
| Opt-in semantic relevance (`--relevance` / explicit floor) | core `vagus` | named finite original-query cosine heuristic; post-truncation/no backfill | implemented (ADR 0026) |
| Reproducible retrieval evaluation + fusion gate | core `vagus` | `vagus eval` schema 2 + named relevance diagnostic + fixed paired `eval-gate` | implemented (ADRs 0024–0026) |
| Atomic cited-note presentation provenance | core + tier-2 skill | strict run/event schema, binary+pipeline+corpus identity, capped-tail truth | implemented (ADR 0021 amendment) |
| Local generative rewriter/HyDE (`vagus rewrite`, `search --smart`, tier 1) | core `vagus` (feature-gated `generate`) | **candle** — qmd's `qmd-query-expansion-1.7B` GGUF | implemented (ADR 0016) |
| Bounded exact+reranked full-body judge (tier 2) | search Agent Skill (Claude Code / pi) | Opus (10 candidates; grade≥2; max 6) | implemented (milestone 3) |
| Networked capture (Slack, GitHub, …) | `vagus-<name>` plugins | per-plugin | implemented mechanism (ADR 0010/0011) |

**Why advanced search is *not* a plugin:** the plugin protocol is capture-shaped (one-way
note→index, stdin inherited) and the reranker/rewriter are neither networked nor a foreign runtime —
so they belong in core. Plugins (G18) stay scoped to networked capture.

The model choices follow [ADR 0015](adr/0015-cross-encoder-rerank.md) and
[ADR 0016](adr/0016-local-generative-rewriter.md): EmbeddingGemma on ort, qmd's query-expansion GGUF
and typed output on candle, and a jina cross-encoder for fastembed's reranker interface.

## Milestones

- **M0 — design:** ADRs 0012–0016 establish the three tiers, runtime boundary, and model contracts.
- **M1 — retrieval:** EmbeddingGemma, token-budgeted chunks, `--full`, `--min-score`, and the
  tokenizer-safe reranker are implemented. Rerank-context radius 0 remains the default.
- **M2 — local generation:** the default-on `generate` feature provides `vagus rewrite` and
  `search --smart`, typed `lex:/vec:/hyde:` routing, multi-query fusion, and reranking. The qmd 1.7B
  GGUF downloads on first use; see [ADR 0016](adr/0016-local-generative-rewriter.md).
- **M3 — agent search:** the [search skill](../skills/vagus-search/SKILL.md) judges ten exact,
  reranked bodies at radius 0, presents at most six nonredundant grade ≥2 results, and retries once
  only if none qualify. It never recomputes RRF. Unfiltered primary citations record provenance and
  counters without query text; explicit `--since` windows use ordinary JSON and counter-only ticks.
  Retries are unticked. Claude Code and pi use the same skill; process-inbox shares the duration grammar.

## Deferred / not building

- Default RRF replacement. Same-pool explicit experiments now have ADR 0025's fixed gate, but none
  is accepted; promotion needs a new ADR with context/latency/default+`--exhaustive` evidence.
- True cosine-MMR (still deferred). The **ranked per-note cap is implemented** as the note-level default —
  a post-rank dedup stage, [ADR 0020](./adr/0020-note-level-results.md); `PER_FILE_CAP=3` remains
  for `--chunks` display.
- A real tokenizer in the chunk hot path (the `chars/3.5` heuristic suffices — G11).
- llama-cpp-2 engine (adds cmake) — fallback only if candle's Qwen3 support regresses.
- Quantized-Gemma via custom-ONNX (a footprint lever for later).
