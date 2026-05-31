# ADR 0017 — Indexed frontmatter filters: `search --since` / `--source`

- **Status:** Accepted (2026-05-30)

## Context

Users want to narrow hybrid search results by when a note was written
(`--since=10d`) and where it came from (`--source=slack`). Both facts live in
**optional** note frontmatter (`created`, `source`), which today is stripped
before indexing ([ADR 0013](./0013-chunk-budget.md)) and is absent from the
tantivy schema (path/chunk_id/heading/body only — [ADR 0003](./0003-search-stack.md)).

The filter must not perturb the deterministic RRF floor (G7/G8): RRF (k=60) is
unweighted and any filter has to be a separate stage *around* fusion, like the
existing `apply_scope` ([ADR 0009](./0009-cwd-scoped-search.md)). It must also
honor G3 — a bare note with no frontmatter must still index *and* still be
filterable by `--since`.

## Options considered

- **Add `created`/`source` to the tantivy schema** and filter inside the lexical
  query. Rejected: a tantivy schema change invalidates the on-disk index format
  (no compat guarantee across 0.x — [ADR 0003](./0003-search-stack.md)), touches
  the BM25 hot path, and would have to be re-applied to the cosine side anyway
  since fusion happens after both lists are produced.
- **Filter entirely in SQLite** (chosen). The vector store already round-trips
  every chunk through `meta.db`; search hydrates each hit from `db.chunk_row(id)`.
  Denormalizing two note-level columns onto each chunk lets the filter read them
  for free during hydration — no tantivy change, no second query.

## Decision

- **SQLite-only.** `chunks` gains two nullable columns — `created_at INTEGER`
  (unix epoch secs) and `source TEXT`. They are **note-level**: parsed once from
  frontmatter at index time and written onto every chunk of the note via
  `replace_chunks`. `chunk_row` reads them back so the filter decides per hit.
  The tantivy schema is unchanged.
- **G3 mtime fallback.** `created` is parsed as `%Y-%m-%dT%H:%M` (local — the
  format vagus writes). If frontmatter is absent or the value is unparseable,
  `created_at` falls back to the file's filesystem mtime (already captured in
  `files.mtime`). So a bare note is always `--since`-filterable. `source` absent
  ⇒ `NULL`.
- **Filter is a separate post-rank stage** (`apply_filters`), applied *after*
  `apply_scope` and *before* the final `take(limit)`, mirroring `apply_scope`.
  It **never touches `rrf()`** and **never reorders** survivors — it only drops.
  Keep a hit iff (`--since` unset OR `created_at >= cutoff`) AND (`--source`
  unset OR `source` equals the request, ASCII case-insensitive). A `NULL`
  `source` never matches a `--source` filter. The cutoff is computed in the CLI:
  `--since` is a relative duration parsed dependency-free (`s/m/h/d/w`, bare
  number = days), `cutoff = now − dur`.
- **CHUNK_VERSION bump → auto-reindex (G4/G20).** `CHUNK_VERSION` goes `3 → 4`.
  `index.rs` already forces a full reindex on identity mismatch and re-pins
  identity in `meta`, so existing vaults reindex **once** automatically and
  back-fill the new columns. No manual `reindex` step for users.
- **`--json` shape stable (G13).** `created`/`source` are added as OPTIONAL Hit
  fields (`skip_serializing_if = Option::is_none`), so the default Hit JSON is
  byte-identical when the flags are unused; the `/search` skill keeps parsing it.

## Consequences

- `rrf()` is untouched; the RRF floor stays deterministic. The new filter is
  structurally identical to the CWD scope filter — a drop-only stage around
  fusion.
- The index is still a derived cache (G2): the two columns are 100% rebuildable
  from the Markdown via `vagus reindex`. The Markdown remains the source of truth
  and is never auto-edited (G3).
- `created_at`/`source` are denormalized onto every chunk (note-level value
  repeated per chunk). At personal scale the duplication is negligible and keeps
  hydration a single-row read.
- Frontmatter is still hand-parsed (no YAML dependency); only top-level scalar
  `created`/`source` keys are read, with quote/whitespace trimming.
- The hydration cap (`limit + 32` candidates) is shared with `apply_scope`; a
  very aggressive filter could under-fill results, same best-effort behavior as
  scope. Acceptable at personal scale; revisit only if it bites.
