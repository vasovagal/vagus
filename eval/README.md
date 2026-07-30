# `vagus eval` — retrieval-quality labels

`vagus eval <labels.jsonl>` scores a **fixed current index** against vault-specific relevance judgments.
It reports P@k, R@k, MRR@k, optional nDCG@k, complete ranked paths, and reproducibility provenance.
See [ADR 0024](../design/adr/0024-retrieval-eval-harness.md).

The command never indexes automatically. Run `vagus index` first when you intend to evaluate fresh
vault contents, then keep the index and label file unchanged across baseline/candidate runs. Evaluation
always uses note-level, no-scope, **exhaustive pre-adaptive-cutoff** results so a candidate cannot look
more precise merely by returning fewer notes.

## Label format

JSONL contains one object per nonblank line:

```jsonl
{"query":"grpc server streaming headers hang","relevant":["30-Resources/gRPC/headers-hang.md","10-Projects/edge/streaming.md"]}
{"query":"which coffee grinder did I pick","relevant":[{"path":"30-Resources/Coffee/grinder.md","grade":3},{"path":"00-Inbox/grinder-notes.md","grade":1}]}
{"query":"xilofrangiate the borogoves","relevant":[]}
```

Paths are normalized **vault-relative `.md` paths** exactly as search returns them. Every mentioned
path—including grade 0—must exist with at least one retrievable chunk in the current index. Replace
the placeholders in
[`example.labels.jsonl`](./example.labels.jsonl) before running it.

`relevant` supports:

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

The report also carries mode-specific top-score means (`rrf`, `bm25`, `cosine`, or
`rerank_sigmoid`). They are diagnostics only—not calibrated probabilities and not comparable across
different modes/configurations.

## Usage

```sh
vagus index
vagus eval eval/my.labels.jsonl                         # hybrid, k=10, normal backend policy
vagus eval eval/my.labels.jsonl --exact                 # force the cosine oracle
vagus eval eval/my.labels.jsonl --mode bm25 --k 20      # lexical-only P/R/RR/nDCG @20
vagus eval eval/my.labels.jsonl --rerank --json > run.json
```

`--json` schema version 1 pins the label and corpus SHA-256s, index/model identities and counts,
binary version + executable SHA-256, result policy, score kind, explicit-exact request, and effective
exact/usearch backend.
Only compare reports whose fingerprints and evaluation config match, unless the changed field is the
variable being deliberately tested. If another process changes the index during a run, eval detects
the final fingerprint mismatch and exits without publishing a mixed-generation report.

Vector/reranked evaluation is a batch operation and currently constructs its local model(s) once per
query through the shared search API. It may be slow, but remains fully offline.
