# ADR 0025 — Evidence-gated fusion experiments; RRF k=60 remains the default

- **Status:** Accepted (2026-07-30). Amends the anti-experiment clause of
  [ADR 0003](./0003-search-stack.md) and G8; depends on [ADR 0024](./0024-retrieval-eval-harness.md).
  **No alternate fusion is accepted or enabled by this ADR.**

## Context

G8 correctly fixed production hybrid fusion at unweighted reciprocal-rank fusion, k=60. It avoided
naive addition of incomparable BM25 and cosine scores and prevented endless weight tuning without
labels. ADR 0024 now supplies standard metrics, complete rankings, and enough identity provenance for
paired evaluation, so an absolute ban on even an explicit experiment is no longer necessary.

A vague “wins precision and/or nDCG with no material regression” rule would be worse than the ban. It
can reward under-returning, tune and test on the same personal queries, hide a long-document miss in
an aggregate, compare different indexes/backends, or ignore ADR 0023's score-knee consumer. The gate
must therefore be executable, paired, fixed before seeing a candidate, and narrower than permission
to silently replace the default.

## Options considered

1. **Keep the absolute ban.** Safest and simplest, but prevents measured experiments even off the
   default path now that a trustworthy harness exists.
2. **Any aggregate eval win permits replacement.** Rejected: underspecified metrics, overfit, backend
   drift, query-family regressions, and G9d score coupling make this unsafe.
3. **Raw-score learning-to-rank over BM25/cosine.** Rejected as a default proposal. Raw scales are not
   comparable or stationary; fitting transforms to the live result distribution compounds overfit.
4. **A fixed paired gate for explicit experiments, with a second decision for default promotion**
   (chosen). This opens a measured path without weakening today's floor.

## Decision

### 1. Default and candidate-pool invariants

- Bare hybrid search remains **unweighted RRF k=60**:
  `score = Σ 1/(60 + rank)`. Its stable eval policy id is `rrf_k60`.
- A fusion experiment must reorder the **same BM25/cosine candidate union at the same depths**. It may
  not change embedding, query rewriting, ANN/exact selection, source retrieval, filtering, note dedup,
  or rerank in the same experiment. Retrieval/candidate-recall work needs its own ADR and evaluation.
- Never add raw BM25 and cosine. Rank weighting is preferred. A score transform must be fixed,
  bounded, corpus-independent, declared in the policy id, and not fitted to the current result set or
  score distribution. Live min-max/z-score and hidden learned normalization remain prohibited.
- Cross-encoder reranking remains a separate post-fusion stage and is disabled in the fusion gate.

### 2. Held-out qrels before candidate tuning

The held-out test label file and baseline report are frozen **before** inspecting candidate results.
The test set must have at least **20 positive, fully graded queries**, each with an ADR 0024 `cohort`,
covering at least **four cohorts with three queries each** (for example lexical, semantic paraphrase,
causal/multi-clause, and long-document/transcript). Known answers are selected before retrieval.

Weights, transforms, or learned parameters may use a different tuning file/hash only. Repeatedly
selecting candidates against the held-out results consumes that set; use a fresh test set or a
predeclared nested/cross-validation protocol. A candidate PR records the tune/test hashes, baseline
and candidate executable hashes/policy ids, exact command lines, and gate JSON.

### 3. Executable promotion gate

Generate both held-out reports against the exact same fixed index and labels:

```sh
vagus eval test.labels.jsonl --mode hybrid --exact --k 10 --json > baseline.json
# run the alternate-fusion artifact/policy against the same snapshot and command -> candidate.json
vagus eval-gate baseline.json candidate.json --json > gate.json
```

`eval-gate` rejects reports unless schema, labels hash, logical corpus/index snapshot, and every config
field except `fusion_policy`/`score_kind` match. The baseline must be `rrf_k60`; both runs must be
hybrid, explicit exact, k=10, no rerank/floor/scope/filter/index refresh, note-level, and exhaustive
pre-tidy. One binary with two explicit policy flags is allowed; executable hashes remain recorded.

All of these predeclared checks must pass:

| Check | Acceptance threshold |
|---|---:|
| Positive graded test queries | ≥20 |
| Cohort coverage | ≥4 cohorts, ≥3 queries each |
| Mean nDCG@10 (primary) | candidate − baseline ≥ **+0.010 absolute** |
| Paired nDCG bootstrap | deterministic 10,000-resample 95% lower bound **>0** |
| Mean R@10 | no loss |
| Per-query R@10 | no query loses recall |
| MRR@10 | loss ≤0.005 absolute |
| Mean P@10 | loss ≤0.005 absolute |
| Every cohort's mean nDCG@10 | loss ≤0.010 absolute |

The deterministic bootstrap seed derives from the held-out labels hash. The stable gate report lists
each check and exits nonzero on rejection. Threshold changes require an ADR/G8/G27 update, not a CLI
argument selected after seeing results.

### 4. G9d and rollout boundary

ADR 0023's adaptive cutoff is defined only over monotonically ordered standard RRF scores. An
alternate policy **must not feed different score semantics into G9d**. Evaluation is deliberately
pre-tidy. If an evidence-passing experiment later lands, it remains explicit opt-in and its result
path fails open to the full requested prefix (equivalent context semantics to `--exhaustive`) unless
a separate ADR defines and tests a compatible cutoff.

Passing this gate is necessary but **not sufficient to replace bare-search RRF**. Default promotion
requires a separate ADR with:

- the frozen gate artifacts and policy configuration;
- user-visible default and `--exhaustive` output tests;
- qrel recall/MRR plus representative body-character/token context measurements;
- latency/RSS evidence and tier-1/tier-2 compatibility;
- a rollback flag and re-evaluation triggers for model/chunk/candidate-depth changes or material corpus
  drift.

### 5. Eval schema amendment

ADR 0024 JSON advances to schema **2**. Each qrel/report may include a normalized `cohort`, and config
records `fusion_policy` plus `fusion_candidate_pool`. Cohorts are optional for exploratory `vagus
eval`, but mandatory for this gate. This synchronized amendment updates G27.

## Consequences

- G8 changes from “alternate fusion cannot even be tested” to a concrete evidence gate for explicit
  experiments. **Production behavior remains byte-for-byte RRF k=60 today.**
- The primary metric, k, absolute deltas, sample floor, cohort floor, anti-overfit protocol, backend,
  and recall constraints are no longer negotiable per candidate.
- Private qrels still cannot be independently published without exposing notes. Complete ranked paths,
  logical fingerprints, executable hashes, deterministic gate code/tests, and recorded commands make
  the maintainer's result auditable without pretending it is a universal benchmark.
- The gate is intentionally difficult to pass with a tiny cherry-picked set. “No result” is preferable
  to a false product-wide policy claim.
- No fusion implementation, search ordering, score, dependency, schema-on-disk, or network path changes
  in this ADR. The only runtime addition is offline comparison of already-generated eval JSON.
