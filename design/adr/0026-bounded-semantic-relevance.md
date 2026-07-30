# ADR 0026 — Opt-in bounded semantic relevance

- **Status:** Accepted (2026-07-30); adds G9e and clarifies/amends ADRs 0003, 0012, 0015, 0023,
  and 0024.

## Context

Vagus deliberately ranks hybrid retrieval with reciprocal-rank fusion. An RRF score answers “how did
these source ranks agree?”, not “how related is this result to the query?” Its small, compressed value
is not useful as a user-facing relevance magnitude. Cross-encoder reranking has the same presentation
problem for a different reason: `sigmoid(logit)` is a monotone display transform, not a calibrated
probability, and capped-prefix reranking leaves an unscored RRF tail.

Users nevertheless need an honest, mechanically filterable indication that an answer candidate is
semantically related to the original query. The signal must not change production RRF, ranking,
rerank-prefix eligibility, default output, or the recall behavior of callers that did not request it.
It also must remain truthful when BM25 alone contributed a hit and when the cross-encoder reordered
only the capped prefix.

## Options considered

1. **Normalize RRF or BM25 against the live result list.** Rejected. Relative min/max, z-score, and
   “percent of top” transforms make the same hit change meaning as the candidate pool changes, and
   confuse source agreement with semantic similarity.
2. **Present `sigmoid(rerank_logit)` as confidence.** Rejected. The reranker is not calibrated, its
   score changes with tokenizer context radius, and no rerank score exists for the capped RRF tail.
3. **Fit a Platt sigmoid or piecewise curve over current scores.** Rejected for v1. The available
   private corpus is too small to justify probability calibration; prior constants were gathered
   before chunk version 5 and before capped-prefix/small-to-big reranking. A fitted curve would add
   apparent precision without a stable labeling and validation protocol.
4. **Expose finite original-query cosine, bounded to the unit interval** (chosen). This is a direct,
   mode-stable semantic heuristic with explicit provenance. It is not a probability or universal
   threshold, but it can be evaluated and filtered without mutating ranking.
5. **Leave only `--min-score`.** Rejected as the complete answer. That existing option is explicitly
   relative to the top displayed score and mode-dependent; it cannot provide a stable semantic field.

## Decision

Add an opt-in relevance policy named
`embeddinggemma300m_chunk5_cosine_clamped_v1`:

```text
relevance = clamp(original_query_cosine, 0, 1), if cosine is finite
relevance = unknown,                         otherwise
```

The name pins the interpretation to the current EmbeddingGemma-300M identity and chunk version 5.
Clamping makes the public field bounded and JSON-safe; it does not claim that cosine is naturally a
probability. A future model/chunk identity must either retain this policy only with fresh evidence or
introduce a newly named policy.

### Reporting contract

- `vagus search --relevance` adds finite `relevance` plus `relevance_policy` to JSON hits and shows a
  bounded percentage in human output. The policy field is present even on an unknown hit, where the
  numeric field is omitted. The CLI describes it as a semantic heuristic, never confidence or
  probability.
- Without `--relevance` or `--min-relevance`, both opt-in fields are cleared before rendering.
  Existing human output and the JSON Hit shape remain byte-compatible.
- Hybrid, vector-only, and ordinary `--rerank` search are supported. BM25-only search is rejected
  because it has no cosine. `--smart` is rejected because its typed multi-query fusion does not retain
  an original-query cosine.
- Reranking carries each hit's original-query cosine through capped-prefix reordering unchanged.
  The unscored RRF tail may still have cosine from vector retrieval. A BM25-only survivor has unknown
  relevance: human output marks its rank-relative display fallback with `~`, and JSON omits
  `relevance` for that hit. No RRF, BM25, or reranker value ever populates the numeric relevance field.
- The feature does not alter RRF k=60, candidate generation, source ranks, note deduplication,
  rerank-prefix selection, scores, ordering, or sibling bodies.

### Filtering contract

`--min-relevance <0..=1>` implies reporting and is a post-rank floor:

1. Complete the existing ranked result pipeline through its ordinary filters, deduplication, scope
   removal, and legacy `--limit` truncation; preserve the resulting order exactly.
2. Filter that finite prefix without backfill. Hits at or above the floor survive.
3. At floor `0`, unknown-relevance hits survive. At any positive floor, they are dropped rather than
   assigned a fabricated value.
4. The option composes conjunctively with `--min-score`; neither changes the other's meaning.
5. Any explicit relevance floor disables ADR 0023 adaptive tidy so two independent tail policies do
   not stack. `--exhaustive` remains useful for disabling tidy but does not bypass an explicit floor.

The floor is not a default and `0.30` is not a globally calibrated constant. It is an evidence-backed
starting point for the current private corpus only.

### Evaluation contract

`vagus eval --relevance` reports the top hit's finite bounded cosine under the policy name in the
existing schema-2 `score_kind` field. Ranking metrics, complete ranked paths, exhaustive-pre-tidy
policy, and all provenance remain unchanged. Undefined top-hit relevance remains `null` under ADR
0024's existing cohort semantics. Because schema 2 already defines score kind as mode-specific
provenance and no metric meaning changes, this does not require a schema bump.

This decision is guardrail **G9e**.

## Measured evidence

All runs used corpus SHA-256
`5fd75d935c59612709be2b718ed0eadc5cece941197ebdc27559b8c4a3ece98d`
(483 indexed files, 4,148 chunks/embeddings, exact cosine). A development diagnostic file (SHA-256
`d878ddaf90576c04c06217cbf6e37bfae9889dab8d3eda574421f05b4d4cede8`) contained 15 grounded
positive queries and 15 deliberately out-of-corpus negative probes. Because some of these scores were
inspected while choosing the policy, none are called holdout evidence. Top-result ranges were:

| Search policy | Development positive | Development negative |
|---|---:|---:|
| exact, no rerank | 0.468–0.815 | 0.148–0.231 |
| exact + capped rerank, radius 0 | 0.383–0.815 | 0.111–0.265 |

A `0.30` exploratory floor was selected from that development separation. After the policy and floor
were frozen, a second never-scored query file was written and hashed before execution (SHA-256
`28b81e1d894922cf2c6de896989af71314ed0e4a8dfc25aadf8c567c9350f672`). It held five new
grounded positives spanning OIDC, PostgreSQL/dbt, AG-UI, Reveal serialization, and PKI, plus five new
out-of-corpus probes. Its top-result ranges and fixed-floor outcomes were:

| Search policy | Holdout positive | Holdout negative | Positive tops ≥0.30 | Negative tops dropped |
|---|---:|---:|---:|---:|
| exact, no rerank | 0.558–0.777 | 0.177–0.300044 | 5/5 | 4/5 |
| exact + capped rerank, radius 0 | 0.566–0.662 | 0.165–0.271 | 5/5 | 5/5 |

The one plain-search miss is important rather than rounded away: an espresso-burr negative scored
`0.300043613`, just above the frozen floor. Thus `0.30` is a useful current-corpus starting point, not
a decision boundary. All five holdout answer paths remained in the top ten and survived the floor in
both policies. Across the 15 development positives, the labeled answer's cosine was 0.463–0.816; the
floor also retained all five primary known answers in the existing lexical/semantic/causal/transcript
benchmark under exact search with no rerank and with rerank context radii 0, 1, and 2. Wider reranking
demonstrated the honest unknown case: a BM25-only hit can move to rank 1, so top relevance may be
undefined even though the ranked result is retained without a positive floor. Empty qrels here are
negative probes, not proof of universal irrelevance.

Default-output compatibility was checked against the pristine predecessor binary: five exact+reranked
JSON qrels and one plain human query were byte-identical without the new flags. Search/eval unit tests
pin finite/clamped semantics, opt-in serialization, positive-vs-zero handling of unknowns, no-backfill
post-truncation filtering, capped-prefix/tail relevance preservation, and unsupported-mode errors.

This evidence supports an opt-in diagnostic and explicit floor, not a default cutoff or probability
calibration. The sample is private and small, corpus drift can move cosine distributions, and other
vaults should inspect their own labeled positives/negatives with `vagus eval --relevance`.

## Consequences

- Users and tools can inspect a stable, bounded semantic heuristic without scraping human text.
- A positive floor can remove an all-noise result set while preserving ranked order, but may also
  remove a useful lexical-only hit; that fail-closed behavior is explicit and opt-in.
- Reranker context changes ordering but not the semantic field attached to each hit. Top-score cohort
  diagnostics may therefore change or become undefined as a different hit reaches rank 1.
- No index schema, embedding, chunk format, model download, network path, daemon, or vault write is
  introduced.
- Default ranking and output remain unchanged; production fusion is still unweighted RRF k=60.
