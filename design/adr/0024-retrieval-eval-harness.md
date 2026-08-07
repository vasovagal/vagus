# ADR 0024 — Reproducible retrieval-quality evaluation (`vagus eval`)

- **Status:** Accepted (2026-07-30); **amended 2026-07-30** by ADR 0025 — schema 2 adds
  query cohorts and explicit fusion-policy/candidate-pool provenance — ADR 0015, which records
  tokenizer-safe rerank-context policies, and ADR 0026, which adds an opt-in named cosine heuristic
  to the existing score-diagnostic shape without changing metrics.

## Context

Vagus had no executable way to measure whether retrieval changes improve known-answer ordering or
silently trade recall for shorter output. Manual corpus audits found real issues—the HNSW miss behind
ADR 0019 and the adaptive-cutoff counterexample behind ADR 0023—but ad-hoc scripts do not define a
stable metric contract for future changes. Labels necessarily name private, vault-relative notes, so
a public golden corpus cannot represent the maintainer's actual workload.

An evaluation report can itself mislead if its denominator rewards under-filled lists, undefined
cohorts become numeric zero, approximate/exact backend selection is omitted, or a post-rank context
cutoff changes how many opportunities a system has to be wrong. Raw RRF and BM25 magnitudes are also
not query-comparable probabilities; averaging them must never be advertised as calibration.

## Options considered

1. **Bundle a toy corpus.** Rejected as the acceptance corpus: it cannot reproduce private technical
   vocabulary, long transcripts, PARA distribution, or known answers. Synthetic fixtures remain
   appropriate for metric and integration tests.
2. **Shell out once per query.** Rejected: it duplicates CLI parsing, makes structured errors harder,
   and cannot unit-test metric math directly. The harness calls the shared search API in-process.
3. **Evaluate user-visible adaptive output by default.** Rejected for ranking experiments. A method
   could appear more precise merely by returning fewer notes. Evaluation therefore fixes the
   exhaustive pre-tidy prefix; context policy is measured separately.
4. **Report a positive-vs-negative “calibration” score.** Rejected. Hybrid top score is RRF agreement,
   BM25 is corpus/query dependent, cosine is not calibrated, and sigmoid(logit) does not calibrate a
   reranker. Mode-specific top-score means can remain clearly labelled diagnostics.
5. **Vault-specific JSONL qrels plus schema-versioned provenance** (chosen). This preserves realistic
   private evaluation while making each result self-describing and mechanically comparable.

## Decision

Add `vagus eval <labels.jsonl> [--k N] [--mode hybrid|bm25|vec] [--rerank
[--rerank-context 0|1|2]] [--exact] [--relevance] [--json]`.
It reads but never refreshes the current local index; callers run `vagus index` explicitly before a
baseline. It uses note-level results with no CWD scope, frontmatter filter, score floor, or ADR 0023
adaptive cutoff. The report calls this policy `note_level_exhaustive_pre_tidy`.

### Labels and validation

Each nonblank JSONL line is exactly:

```json
{"query":"...","cohort":"semantic","relevant":["vault/path.md"]}
```

`cohort` is an optional normalized label for exploratory eval and mandatory only for ADR 0025's
promotion gate. The qrels may use `{"path":"...","grade":0..3}` entries. Bare paths have grade 1. Grades 1–3 are relevant;
grade 0 records a judged non-relevant note. An explicit empty relevant set is a negative probe.
Unknown keys, missing `relevant`, empty queries/files, duplicate queries/qrels, non-normalized or
non-Markdown paths, grades outside 0–3, and qrel paths absent from the current index or lacking a
retrievable chunk are hard errors.
This distinguishes stale labels from genuine misses.

### Metric contract

For each **positive** query and requested `1 <= k <= 1000` (bounded before search-pool arithmetic):

- `P@k = relevant results in first k / k`. The denominator is always fixed; under-returning is
  penalized rather than rewarded.
- `R@k = relevant results in first k / number of positive qrels`.
- `RR@k = 1 / first relevant rank` within k, else zero; the aggregate is explicitly `MRR@k`.
- `nDCG@k` uses `Σ(2^grade−1)/log2(rank+1)` and is defined only for positive lines using graded form.

Negative probes have `null` P/R/RR/nDCG. Empty/missing positive, negative, graded, or top-score
cohorts likewise serialize as `null`, never fabricated `0.0`. Every per-query report includes the
returned ranked note paths so metrics can be independently recomputed.

The top hit's finite mode-specific score is retained as a diagnostic (`rrf`, `bm25`, `cosine`, or
`rerank_sigmoid`). ADR 0026's opt-in `--relevance` instead records finite original-query cosine
clamped to `[0,1]` under a chunk-pinned policy name (currently
`embeddinggemma300m_chunk6_cosine_clamped_v1`; the original evidence used chunk 5); this remains an
explicitly uncalibrated heuristic. A cohort mean is defined only when that cohort is non-empty and
**every** member returned a score. Neither it nor its delta is a probability or ranking acceptance
metric; a BM25-only top hit can make the relevance diagnostic undefined.

### Provenance contract

JSON schema version **2** records:

- binary version **and executable SHA-256**, plus raw label-file SHA-256;
- corpus SHA-256 over sorted `(vault-relative path, note-content hash)` pairs;
- indexed file/chunk/embedding counts and pinned model/chunk/tantivy/vector identities;
- k, mode, rerank, explicit-exact request, **effective** vector backend, and the automatic exact cutoff;
- the exact capped-prefix rerank context radius + tokenizer maximum in `rerank_policy` (ADR 0015);
- query cohort plus stable `fusion_policy` and `fusion_candidate_pool` identifiers (ADR 0025);
- note-level/exhaustive-pre-tidy/no-refresh/no-scope policy and score kind, including ADR 0026's
  named policy when `--relevance` is explicit.

The vector trait exposes only a stable diagnostic backend name; selection and fallback still flow
through the exact same ADR 0019 factory used by search. This does not change ranking. Long runs
recompute the complete index snapshot after the final query and fail rather than publish a report if
another process changed the index mid-run.

### Verification

Metric/parser/undefined-cohort tests are model-free. A current-index integration fixture builds real
SQLite + Tantivy state, runs BM25 through `search::query`, verifies fixed-denominator P@k and MRR@k,
and pins the schema/provenance/null contract. ADR 0025 adds deterministic paired-bootstrap and
accept/reject fixtures for `vagus eval-gate`. Vector-model quality remains a local corpus run, not a
networked CI dependency.

This contract is guardrail **G27**. [ADR 0025](./0025-evidence-gated-fusion.md) uses comparable reports
as an evidence gate for explicit experiments; it does not itself change default fusion.

## Consequences

- Evaluation becomes repeatable and auditable without putting private notes or labels in the repo.
- Reports from different label/corpus/index/config fingerprints are not directly comparable; tooling
  must refuse or explicitly explain such comparisons.
- Vector/reranked batches currently pay local model construction per query through the shared API.
  This is slow but truthful and offline; session reuse is a later optimization that must not alter
  results or provenance.
- Changing metric meaning or required JSON fields requires a schema-version bump and ADR/G27 update.
- ADR 0021's cited-note rank events are agent-selection-biased presentation diagnostics with no
  missing-answer observations or qrels; they cannot replace this acceptance harness.
- No index schema, model identity, chunk version, network path, daemon, or vault write is introduced.
