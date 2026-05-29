# ADR 0004 — iCloud holds Markdown only; index/DB/cache live outside iCloud

- **Status:** Accepted (2026-05-29)

## Context

The vault lives in iCloud so notes sync to phone and are backed up. The search index is a SQLite DB
(+ tantivy segments) and a cached model. Where should the derived state live?

## Options considered

1. Everything (vault + index + DB) in iCloud — simplest mentally.
2. **Markdown in iCloud; index/DB/model cache outside iCloud** (`~/.local/share/vagus`,
   `~/Library/Caches/vagus`).

## Decision

**Option 2.** A live SQLite DB inside an iCloud-synced folder corrupts: iCloud syncs `.db`, `-wal`,
`-shm` independently and asynchronously, ignores SQLite's locks, and may evict files to `.icloud`
placeholders — yielding `database disk image is malformed` / `SQLITE_CANTOPEN`. (Same hazard on
Dropbox/OneDrive; SQLite's own docs warn against it.) The DB is a **derived cache, fully rebuildable**
from the Markdown, so there is zero benefit to syncing it.

## Consequences

- `~/.local/share/vagus/` holds `tantivy/`, `meta.db`, `config.toml`. `~/Library/Caches/vagus/models/`
  holds the embedding model. Neither is in iCloud.
- Only plain Markdown lives in `~/brain` (→ iCloud). `doctor` asserts no index files under the vault.
- A second machine builds its **own** local index from the synced Markdown (`vagus index`) — we never
  sync the DB across devices.
- `vagus reindex` must always be able to reconstruct the index from scratch (it's a pure cache).
