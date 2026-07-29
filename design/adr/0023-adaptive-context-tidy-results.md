# ADR 0023 — Adaptive context-tidy result ceiling

- **Status:** Accepted (2026-07-29)

## Context

`vagus search --limit 10` historically treated ten as a quota whenever retrieval could fill it. That
is convenient for exhaustive browsing, but wasteful for an LLM consumer: a clear high-signal head can
be followed by a long, low-signal tail whose full chunk bodies consume context only to be rejected.
The default tier-0 path cannot use an LLM to judge relevance, and G8 forbids changing or weighting RRF.

The motivating query was:

```text
Hunter was downtrodden at the end
```

The first note directly paraphrased the answer. The legacy ten-note `--json --full` payload contained
17,103 body characters / 3,119 whitespace words / an estimated 4,276 tokens (`ceil(chars/4)`). Four
independent relevance judges agreed rank 1 alone was sufficient, agreed ranks 4 and 6 were useful
corroboration, disputed rank 7, and agreed ranks 8–10 were not useful.

The goal is therefore conservative tail control, not an oracle that tries to identify the one perfect
answer: `--limit` should be a ceiling when the existing RRF scores contain a statistically distinct
head/tail boundary, while recall-critical callers retain the old behavior explicitly.

## Options considered

1. **Always return `--limit`** (status quo). Highest recall, but no mechanical sympathy for downstream
   context budgets.
2. **Make the cross-encoder reranker mandatory.** Rejected: adds model/download/latency to tier 0,
   changes the three-tier contract, and still needs a calibrated cutoff.
3. **Default relative score floor (`--min-score`).** Rejected: hybrid RRF scores are intentionally
   compressed and rank-based; a fixed percentage behaves inconsistently across modes.
4. **Raw BM25/cosine strong-head outlier.** In an 18-query development matrix it reduced aggregate
   body characters 46.29%, but was too aggressive for targeted long-document recall: a dominant top
   note can coexist with a requested transcript or secondary answer lower in the list.
5. **Guarded robust RRF-knee prefix** (chosen). It reduced aggregate body characters 30.12% in the
   same matrix, activated only 6/18 times, preserved rank/order/scores, and failed open on smooth or
   unsupported result lists.

## Decision

For **plain tier-0 hybrid note results only**, make `--limit` an adaptive ceiling:

1. Build today's ranked, filtered, note-deduplicated, scope-filtered list and truncate to `--limit`
   exactly as before.
2. Consider tidying only when the list filled `--limit`, has at least six hits, `--chunks` is off,
   `--rerank`/`--smart` are off, no explicit `--min-score` was supplied, and `--exhaustive` is off.
   BM25-only, vector-only, reranked, smart, chunk, under-filled, and explicit-floor searches retain
   their previous behavior.
3. Let positive, finite, non-increasing RRF scores be `s1..sn`; compute adjacent log gaps
   `g_i = ln(s_i / s_{i+1})`.
4. Compute `m = median(g)` and `MAD = median(|g-m|)`. The prominence threshold is:

   ```text
   T = max(ln(10/9), m + 3 * 1.4826 * MAD)
   ```

   Thus a cutoff needs at least a 10% adjacent score-ratio drop and must also be a robust three-sigma
   outlier relative to the list's own gaps.
5. Among boundaries leaving at least three hits on both sides, choose the largest gap (latest boundary
   on an exact tie, favoring recall). Truncate only when that gap is strictly greater than `T`.
6. Invalid, missing, non-positive, non-monotone, short, smooth, or threshold-free inputs **fail open**
   and retain the full list.
7. The stage may only drop a suffix. It never reorders, backfills, mutates a score, normalizes a
   component, or calls/modifies `rrf()` (G8). `--json` remains the same pure Hit array; an omission
   notice goes to stderr. Human output gets the same notice inline.
8. `--exhaustive` bypasses only this adaptive stage and restores the legacy fill-up-to-`--limit`
   count/order/Hit objects. It composes with `--exact` for maximum-recall retrieval.

This is guardrail **G9d**.

## Measured evidence

Method: compare the exact same fixed index with legacy/exhaustive versus adaptive output. Count only
full body strings, not JSON syntax/headings/snippets: characters = `sum(body.chars().count())`, words =
`sum(body.split_whitespace().count())`, estimated tokens = `ceil(total_chars/4)`. Token numbers are an
explicit tokenizer-independent estimate, not a claim about a particular hosted model tokenizer.

### Motivating query

| Metric | Legacy/exhaustive | Adaptive | Reduction |
|---|---:|---:|---:|
| Results | 10 | 7 | 30.00% |
| Body characters | 17,103 | 10,476 | 38.75% |
| Whitespace words | 3,119 | 2,003 | 35.78% |
| Estimated tokens | 4,276 | 2,619 | 38.75% |

The cutoff retains the direct answer, every consensus-supporting hit, and the disputed rank 7; only
ranks 8–10 are removed.

### Development matrix

An 18-query mixed set (direct facts, semantic questions, broad entity searches, long-transcript
recall, architecture lookups, and exact-ish keyword queries) contained 192,691 legacy body characters.
The guarded RRF knee activated for 6/18 queries and returned 134,651 characters: **30.12% aggregate
reduction**. Smooth distributions failed open. The more aggressive raw-score outlier strategy returned
103,486 characters (**46.29% reduction**) but was rejected for its recall behavior.

The Hunter score/size fixture is pinned in unit tests, alongside smooth/endpoint/invalid/scale
invariance cases. Four independent post-change subagents then reran both CLI paths, recomputed every
count, verified adaptive output was the exact exhaustive prefix, and unanimously graded dropped ranks
8/9/10 as `1/0/1` (tangential/trash/tangential): **zero dropped hits at the useful ≥2 threshold**.
They also independently recomputed the 18-query aggregate from raw rows with no disagreement.

## Consequences

- Default plain hybrid note search may return fewer than `--limit`; the limit is now a maximum, not a
  promise. This amends ADR 0012's old “byte-identical tier-0” consequence and ADR 0020's limit wording.
- Context savings cost only O(limit) arithmetic and no model/network/dependency. Retrieval latency and
  model footprint are unchanged.
- The rule is intentionally conservative and often abstains. It does not detect an all-junk list and
  cannot remove weak hits interleaved inside a retained prefix.
- Corpus/index changes can move a score knee. Behavior is deterministic for a fixed result list, and
  `--exhaustive` is the stable recall/debug escape hatch.
- No index schema, embedding identity, chunk format, or `CHUNK_VERSION` change is required.
