# `vagus eval` — retrieval-quality labels

Use `vagus eval <labels.jsonl>` to compare retrieval against judgments for your vault. Prepare labels,
freeze the index, then run the comparisons below. Reports include P@k, R@k, MRR@k, optional nDCG@k,
ranked paths, and the identities needed to reproduce a run.

Eval never refreshes the index. Run `vagus index` before freezing it, then keep both index and labels
unchanged across baseline and candidate runs. Results are note-level and exhaustive before the
adaptive cutoff, without CWD scope or metadata filters. Fixed denominators prevent fewer returned
notes from inflating precision. [ADR 0024](../design/adr/0024-retrieval-eval-harness.md) owns the contract.

## Label format

JSONL contains one object per nonblank line:

```jsonl
{"query":"grpc server streaming headers hang","cohort":"lexical","relevant":["30-Resources/gRPC/headers-hang.md","10-Projects/edge/streaming.md"]}
{"query":"which coffee grinder did I pick","cohort":"semantic","relevant":[{"path":"30-Resources/Coffee/grinder.md","grade":3},{"path":"00-Inbox/grinder-notes.md","grade":1}]}
{"query":"xilofrangiate the borogoves","relevant":[]}
```

Paths are normalized **vault-relative `.md` paths** exactly as search returns them. Every mentioned
path—including grade 0—must exist with at least one retrievable chunk in the current index. Replace
the placeholders in
[`example.labels.jsonl`](./example.labels.jsonl) before running it.

`cohort` is optional for ordinary eval (1–64 ASCII letters/digits/`-`/`_`) and mandatory for ADR
0025's fusion gate. `relevant` supports:

- a path string, equivalent to grade 1;
- `{"path":"...","grade":0..3}` where 1–3 are increasingly relevant and 0 is judged non-relevant;
- an explicit empty array for an out-of-corpus negative probe.

Missing/unknown keys, empty or duplicate queries, duplicate/invalid/stale paths, and out-of-range
grades fail the run. This prevents typos and deleted notes from masquerading as retrieval misses.

## Metric contract

- **P@k** uses the fixed denominator `k`; an under-filled list is penalized.
- **R@k** divides by all positive qrels for that query.
- **RR@k / MRR@k** is explicitly truncated at k.
- **nDCG@k** is present only for positive lines using graded-object judgments.
- Negative-probe metrics and absent cohorts are JSON `null`, never artificial zeroes.

Top-score means (`rrf`, `bm25`, `cosine`, or `rerank_sigmoid`) are diagnostics, not probabilities;
do not compare them across modes or configurations. With `--relevance`, `score_kind` is
`embeddinggemma300m_chunk6_cosine_clamped_v1`: finite original-query cosine clamped to `[0,1]`,
a heuristic rather than confidence. A top hit without cosine, such as a BM25-only hit, yields `null`;
a cohort mean is defined only when every member has a finite top value.

Policy names advance with `CHUNK_VERSION`. Chunk-5 evidence predates searchable producer-metadata
vectors; its thresholds do not calibrate the current corpus.

## Usage

```sh
vagus index
vagus eval eval/my.labels.jsonl                         # hybrid, k=10, normal backend policy
vagus eval eval/my.labels.jsonl --exact                 # force the cosine oracle
vagus eval eval/my.labels.jsonl --exact --relevance --json > relevance.json
vagus eval eval/my.labels.jsonl --mode bm25 --k 20      # lexical-only P/R/RR/nDCG @20
vagus eval eval/my.labels.jsonl --rerank --json > run.json
vagus eval eval/my.labels.jsonl --rerank --rerank-context 1 --json > context-1.json
```

`--json` schema version 2 pins the label and corpus SHA-256s, index/model identities and counts,
binary version + executable SHA-256, result policy, cohort, fusion policy/candidate pool, score kind,
explicit-exact request, effective exact/usearch backend, and (inside `rerank_policy`) the exact
rerank-context radius + tokenizer maximum.
Compare matching fingerprints and configurations, except for the variable deliberately under test.
For relevance, preselect positive and out-of-corpus negative cohorts and validate proposed floors on
untouched holdouts. Separation in one corpus does not establish universal calibration.
`--relevance` is unsupported with `--mode bm25`.

Eval fingerprints the index before and after the run. If those fingerprints differ, it exits without
publishing a report.

## Fusion evidence gate (ADR 0025)

Freeze fully graded held-out qrels and an RRF baseline **before** tuning a candidate. The gate requires
at least 20 positive queries across at least four cohorts (three per cohort), then compares identical
exact-hybrid k=10 pre-tidy reports:

```sh
vagus eval test.labels.jsonl --mode hybrid --exact --k 10 --json > baseline.json
# produce candidate.json with the alternate fusion policy, same command/index/labels
vagus eval-gate baseline.json candidate.json --json > gate.json
```

It requires ≥+0.010 mean nDCG@10, a positive deterministic paired-bootstrap 95% lower bound, no mean
or per-query recall loss, bounded MRR/P/cohort regressions, and matching labels/corpus/index/config
provenance. Rejection exits nonzero. Passing permits only an explicit experiment; replacing default
RRF or reusing ADR 0023's score-knee needs a separate ADR and context/latency evidence. See
[ADR 0025](../design/adr/0025-evidence-gated-fusion.md) for the binding anti-overfit protocol.

Vector and reranked evaluation construct local models for each query, so initialization can dominate
runtime. Cache the required models before an offline run; missing models may download on first use.
