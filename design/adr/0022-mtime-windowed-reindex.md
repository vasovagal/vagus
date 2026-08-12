# ADR 0022 — Mtime-windowed forced reindex

- **Status:** Accepted (2026-07-29); **amended 2026-07-31** to persist forced usearch mutations and
  make incomplete embedding rows an implicit incremental repair set; **amended 2026-08-12** to use
  the shared CLI duration grammar.

## Context

The Markdown vault may be shared by two Macs through the same iCloud location, while each Mac keeps
its own derived SQLite/Tantivy/usearch index outside iCloud (G1/G2). Normal `vagus index` is cheap and
incremental, but deliberately trusts an unchanged filesystem mtime as its first fast path. A machine
may therefore need to force-refresh a recent slice after iCloud synchronization or a previously
interrupted local index run, without paying to re-embed the whole vault with `vagus reindex`.

A partial repair still has to preserve G5: every selected note must be replaced in SQLite, Tantivy,
and usearch together. It must also walk the whole vault so additions and deletions are not hidden by
the time window.

## Options considered

1. **Keep full `reindex` only.** Correct but unnecessarily expensive for a large vault and a small
   recent synchronization window.
2. **Tell users to touch files, then run `index`.** Mutates authoritative notes solely to invalidate a
   cache, creates iCloud churn, and still relies on users selecting paths correctly.
3. **`reindex --since <duration>` as a forced incremental repair** (chosen). Select existing notes by
   filesystem mtime, bypass their normal mtime/hash shortcuts, and otherwise perform a complete
   incremental reconciliation.
4. **Build a separate partial index containing only recent notes.** Rejected: it would intentionally
   make older notes disappear from search and violate the expectation that the local index represents
   the whole vault.

## Decision

Add `vagus reindex --since <duration>` with these semantics:

- Durations use the same validated `SinceDuration` CLI type as `search` and `inbox`: `h` = hours,
  `d` = days, `m` = 30-day months, and `y` = 365-day years, with `s`, `min`, `w`, and bare days also
  accepted. (`m` now means months; minutes use `min`.) Invalid values fail in clap before vault/index
  access. The cutoff is `invocation time - duration`.
- The selector is the note's **filesystem mtime**, not frontmatter `created` (the latter belongs to
  `search --since` / `inbox --since`, ADR 0017). A note is selected when `mtime >= cutoff`.
- Before mutating a derived store, the indexer walks the **entire vault** and builds a sorted snapshot
  of every Markdown path and mtime. Walk/stat errors are fatal rather than silently turning unreadable
  notes into apparent deletions.
- A selected existing note bypasses both the unchanged-mtime and identical-hash shortcuts. Its chunks,
  embeddings, Tantivy documents, and usearch vectors are replaced through the normal G5 path even if
  cached metadata claims it is unchanged. Independently of `--since`, any file with a NULL chunk
  embedding is an implicit repair selection: an interrupted run cannot permanently bless partial
  rows merely because its file mtime/hash were written first.
- Older existing notes retain normal incremental behavior. New notes are indexed even when their mtime
  is older than the window, and missing paths are deleted globally. Thus `--since` augments normal
  reconciliation; it never creates an intentionally partial local index.
- The command does **not** clear SQLite/Tantivy/usearch and never touches G25 usage/provenance user
  data. Plain `vagus reindex` without `--since` keeps its existing full-wipe/rebuild behavior.
- An incompatible identity cannot be repaired by a time window. The existing chunk-version
  auto-rebuild upgrades the run to a full reindex and reports it; a direct embedding-identity
  mismatch refuses the partial run and requires plain `vagus reindex` (G4).
- No `CHUNK_VERSION` bump is needed: note/chunk representation is unchanged.

## Consequences

- Recent iCloud changes can be force-refreshed with, for example, `vagus reindex --since 10d`, while
  older healthy embeddings are reused.
- Cost is O(all vault files) for the path+mtime snapshot and proportional to selected/actually changed
  notes for reading, chunking, and embedding. Under ADR 0019's current write path, any vector mutation
  still saves the usearch sidecar once at the end. `refreshed` files count as mutations: omitting them
  reproduced a successful 4,381-embedding/4,361-vector run whose in-memory repairs were discarded.
- Filesystem mtimes come from iCloud/filesystem metadata and can be affected by clock skew or metadata
  preservation. A conservative wider window is safe because selection only causes redundant derived
  work; it never edits Markdown.
- This is a repair/synchronization convenience, not proof that every older row is healthy. Use a full
  `vagus reindex` when the suspect period is unknown or an embedding/chunk identity changed.
- Recorded as guardrail G26.
