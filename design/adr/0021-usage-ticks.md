# ADR 0021 — Usage ticks and presentation provenance

- **Status:** Accepted (2026-07-08); **amended 2026-07-30** with versioned rank-provenance events;
  **amended 2026-08-12** to make the skill's explicit `--since` path counter-only.

## Context

The search Agent Skill records a per-note usage signal when it judges a note useful and actually
presents it. This powers `vagus fame` without editing Markdown. These counters are local user data,
not a cache derivable from the vault.

Counters say which notes are useful but not how retrieval surfaced them. Selected-result provenance
can show whether a cited note began low in fused/source ranks, finished high, or remained in the
unscored RRF tail. Such observations are **selection-biased diagnostics**, not ground truth: the log
cannot see missing answers and agent judgments vary. ADR 0024 qrels/eval remains the acceptance gate.

## Options considered

- **Frontmatter:** rejected by G3 and because it would create iCloud churn.
- **A separate sidecar DB:** rejected because it adds a store and prevents one transaction.
- **Stable IDs in frontmatter:** rejected by G3; normalized vault-relative paths are sufficient.
- **Ticking from bare search:** rejected; retrieval is not usage. Only the tier-2 judge knows what it
  presented (G19).
- **Counter-only table:** selected originally; minimal, but insufficient for rank diagnostics.
- **Events with path/query/two ranks:** rejected; capped tails have no rerank rank and ranks are
  ambiguous without complete pipeline and corpus identity.
- **Versioned run + selected-event rows in `meta.db`:** selected by the amendment.

## Decision

`meta.db` has two data classes:

1. **Derived cache:** `files`, `chunks`, `meta`, and `expansion_cache`, plus Tantivy/usearch.
2. **Local user data:** `ticks`, `tick_runs`, and `tick_events`.

Full, incremental, windowed, and chunk-version reindex preserve all local-user-data tables. File
moves re-key event paths with counters. External moves can orphan both; fame/rank reports hide
missing paths by default and doctor reports counter/event orphans. None enters iCloud.

### Counter contract

`ticks` remains keyed by normalized vault-relative Markdown path with no FK to `files`. Unknown but
valid paths warn and still record. Repeated presentation increments repeatedly; a tick is neither a
distinct query nor user approval. `vagus file` merges conflicting destination counters atomically.

### Explicit search instrumentation

Default `vagus search --json` remains the ordinary byte-compatible Hit array; internal rank fields
are never serialized there. Only this fixed note-level standard-hybrid path may emit schema-1
`{run,hits}`:

```text
--json --full --rerank --exact --tick-provenance
```

Smart, BM25/vector-only, chunk, metadata-filtered, score-floor, and relevance-floor variants are
rejected. Therefore, when the skill honors explicit user time intent with `--since`, it must omit
`--tick-provenance`, parse the ordinary Hit array, and record only cited paths through positional
counter ticks; it must never synthesize a run or rank tuple. CWD scope remains allowed but gets an
opaque SHA-256 policy identity (raw words are not stored), preventing unlike exclusion sets from
sharing a diagnostic group.

A run records executable version and SHA-256; corpus SHA-256 and indexed counts; embed/chunk/tantivy
identities; RRF/candidate-pool and exact-backend policies; reranker model and tokenizer-context policy;
requested source/fused depths, actual candidate pool/cap; limit/returned counts; result/scope policies;
and index-refresh request/outcome.
Search fingerprints corpus/index metadata before and after retrieval and refuses mixed snapshots. A
self-verifying `pipeline_id` hashes effective configuration, index counts, and actual pool/cap; corpus
identity stays separate.

Each wrapped Hit records:

- `event_id`: SHA-256 binding this path and complete rank tuple to the run/pipeline/corpus;
- `fusion_rank`: best pre-rerank note rank, folded across sibling chunks;
- source `bm25_rank` / `cosine_rank` when present;
- `rerank_rank` only if the candidate was actually scored inside the cap;
- `final_rank` after rerank, note dedup, scope, and truncation;
- explicit `rerank_scored` state.

Scored means `fusion_rank <= cap` and requires a valid rerank rank. Unscored means
`fusion_rank > cap` and forbids one. An agent may cite an unscored tail hit, but it cannot be called a
reranker rescue.

### Atomic writes and privacy

The skill copies the run plus only cited `{path,provenance}` pairs to
`vagus tick --events <JSON>`. Strict, bounded input is normalized and checked for path-bound event
IDs, duplicate paths, positive/in-range/unique ranks, pipeline identity, and cap consistency. One
SQLite transaction inserts
the run, increments all selected counters, inserts all events, and commits all or rolls back all.
Event failure can never inflate counters.

Positional paths remain supported as counter-only input. Event paths and positional paths deduplicate
within one invocation. Query text is omitted by default; storing it requires both `--query` and the
separate `--store-query` opt-in, with size bounds. Bodies, snippets, and agent explanations are never
stored. Payloads and queries are bounded before parsing/writing. `vagus ticks` groups rank summaries by
`(path,pipeline_id,corpus_sha256)` and always labels their selection bias; `--all` includes orphans
explicitly.

## Consequences

- **Positive:** useful-note popularity survives every index rebuild; default and time-filtered search
  JSON and ranking remain honest; exact pipeline/corpus groups support descriptive diagnostics only
  where the strict contract applies; scored-prefix versus unscored-tail observations are explicit;
  counter and event updates cannot diverge.
- **Negative:** `meta.db` is no longer wholly disposable. Backup/recovery must preserve three tables.
  Paths can orphan after external moves. Events add local storage and selected-path history.
- **Boundary:** provenance does not establish recall, relevance calibration, causal model quality, or
  universal thresholds. Ranking changes still require ADR 0024/0025 evidence gates.
