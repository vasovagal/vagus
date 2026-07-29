# ADR 0020 — Note-level results by default: `--limit` counts distinct notes

- **Status:** Accepted (2026-06-09); **amended 2026-07-29** — `--limit` remains note-counted but is
  now an adaptive ceiling for plain hybrid results (ADR 0023).

## Context

The unit of retrieval is a **chunk** (`chunk_id = sha256(path + "#" + ord)` — [ADR 0013](./0013-chunk-budget.md)),
and `--limit` truncates the ranked *chunk* list. A long note that matches broadly can occupy several
of the top slots, so `--limit 10` may surface only 3–4 distinct notes. The human display already
groups hits by note (`PER_FILE_CAP=3` + "+N more in this note"), but that is cosmetic: the limit and
the `--json` array stayed chunk-level, and the `/search` skill's then-`--limit 20` could deliver 20
chunks spanning far fewer notes. (ADR 0012 later reduced the skill budget to 10 distinct notes.)

In practice the user asking for "10 hits" almost always means **10 different notes** — "the file
with the best chunk", not "the 10 best chunks wherever they live". The roadmap had deferred a
"ranked per-note cap"; this ADR un-defers a simple variant of it. As with [ADR 0017](./0017-indexed-frontmatter-filters.md),
nothing may perturb the deterministic RRF floor (G7/G8).

## Options considered

- **Keep display-only grouping** (status quo). Rejected: it fixes the *rendering* but not the
  semantics — `--limit`, `--json`, and the skill still count chunks.
- **Per-note capping inside fusion / cosine-MMR.** Rejected: edits ranking itself, a G8 breach
  (same family as qmd's weighted-RRF). MMR stays deferred.
- **Post-rank dedup stage** (chosen). Exactly the `apply_scope` / `apply_filters` shape: a
  drop-only, order-preserving stage *around* fusion. `rrf()` and the reranker are untouched.

## Decision

- **Note-level is the default.** After rerank and after `apply_filters`, `dedupe_notes` keeps each
  note's best-ranked chunk and drops its later chunks; truncation to `--limit` then counts
  **distinct notes**. Stage order is load-bearing: filters run first, so a note whose best chunk
  was dropped by `--since`/`--source` is represented by its next surviving chunk. **Amendment
  (ADR 0023):** for plain hybrid note results, a later drop-only context gate may shorten that final
  list, so `--limit` is a maximum; `--exhaustive` restores the legacy fill behavior.
- **`--chunks` opts out**, restoring raw chunk-level hits (`--limit` counts chunks) —
  byte-identical to pre-0.7 output, since `siblings` is never set on that path.
- **`siblings` is an additive optional Hit field** (`skip_serializing_if`, like
  `created`/`source` — G9a): the count of additional ranked chunks from the same note folded into
  the kept hit, present only in note mode and only when > 0. It powers the "+N more in this note"
  display line and gives the `/search` skill a breadth signal.
- **Pool deepening.** Dedup compresses chunks → notes, so note mode always retrieves the deep pool
  (`(limit*4).max(30)` — the existing rerank/filter sizing). Only `--chunks` without
  rerank/filters retrieves exactly `limit`, preserving the old hot path.
- **Filing stays chunk-level.** `vagus file --suggest` passes `chunks: true`: it folds folders
  itself and weighs per-chunk scores, so its suggestions are unchanged by this ADR.

## Consequences

- Default `--json` *content* changes (one best-chunk hit per note); the *shape* is unchanged per
  G9a (field set + one additive optional field). The only known `--json` consumer is the in-repo
  `/search` skill, updated in the same change; `--chunks` is the compatibility escape hatch.
- `rrf()` and rerank are untouched (G8); dedup is structurally identical to scope/frontmatter
  filtering — adds **G9c**. ADR 0023's later suffix-only gate is likewise outside dedup/fusion and
  adds G9d.
- A note dominating the deep pool can under-fill `limit` — best-effort, same documented stance as
  ADR 0017's filter under-fill. Acceptable at personal scale.
- `PER_FILE_CAP=3` remains, now relevant only to `--chunks` human display; in note mode each group
  has one hit and the overflow line is driven by `siblings`.
