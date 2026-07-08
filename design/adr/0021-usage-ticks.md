# ADR 0021 — Usage ticks: local user data in meta.db

- **Status:** Accepted (2026-07-08)

## Context

We want a per-note usage signal — a "hall of fame" (`vagus fame`) — recorded when the `/search`
skill judges a note a top hit and actually presents it. The signal must never enter the iCloud
vault (G1) or note frontmatter (G3). meta.db is currently described as wholly derived (db.rs
module doc, G2), and ticks contradict that as written: usage counts are user data, not derivable
from the Markdown.

## Options considered

- **Frontmatter counters.** Rejected: G3's auto-edit ban, G1's vault-stays-plain-content rule, and
  iCloud churn on every search.
- **Separate `ticks.db` sidecar.** Rejected: a fourth store with its own WAL, and it loses the
  single-connection transaction for rename re-keying. The `meta`-survives-`clear_all` precedent
  shows scoped wipes work inside one DB.
- **Append-only event log (path, ticked_at, query).** Deferred: enables `--since`/decay later; a
  counter row with `first_used`/`last_used` is enough for fame v1 and matches the existing upsert
  idiom. Revisit if windowed stats are wanted.
- **Stable note-ID in frontmatter to survive renames.** Rejected: G3.
- **Ticking from bare `vagus search`.** Rejected: retrieval is not usage; only the tier-2 judge
  knows what was presented (G19).
- **Counter table `ticks` in meta.db, no FK (chosen).**

## Decision

meta.db now holds **two data classes**: (a) derived cache (`files`/`chunks`/`meta`/`expansion_cache`
— rebuildable, G2) and (b) **local user data** (`ticks` — NOT rebuildable, excluded from every wipe
path).

- **No FK to `files`**: `foreign_keys=ON` + `ON DELETE CASCADE` would cascade-wipe ticks on every
  reindex (`clear_all` deletes all `files` rows) and on `delete_file()`.
- **`clear_all` never touches `ticks`** — ticks survive `vagus reindex` and the automatic
  CHUNK_VERSION-mismatch reindex (same wipe path).
- Keyed by **vault-relative path**; `vagus file` re-keys with a merge-on-conflict in the same
  operation, **fail-soft** (a re-key failure warns and never fails the filing). Re-keying to the
  same path is a guarded no-op (re-filing a note into its current folder must not touch its ticks),
  and alias spellings of absolute paths (the vault symlink's real target, `/tmp` -> `/private/tmp`)
  are canonicalized before keying so they hit the same row as the plain spelling.
- Deletes and external renames (Finder/Obsidian = delete+add to the indexer) **orphan** rows:
  kept, hidden by fame's default JOIN, shown by `--all`, counted by `doctor`. No rename-detection
  heuristics — accepted limitation.
- **Recording is unconditional** — never gate user data on the cache (the `files` table is
  transiently empty mid-reindex); unknown paths get a stderr notice only.
- **Skill-channel-only writes** (tier 2): only the `/search` skill ticks; bare `vagus search`
  (tier 0/1) never writes — consistent with G19 channel selection.
- Stable `--json` on both commands (G9a).
- Counts measure **presentations**, not distinct queries or user approval — repeated searches
  inflate; accepted and named.
- Deferred (not in v1): `--since` windowing (needs an event log), untick/reset, manual
  `tick --move`, surfacing tick counts in search Hits.

## Consequences

- Deleting meta.db is no longer a lossless reset — doctor/docs must never suggest it.
- Ticks sit outside the G5 three-store hash-diff (user data, not derived); `doctor` reports
  orphans informationally, never auto-deletes.
- Builtin `tick`/`fame` shadow any same-named `vagus-tick`/`vagus-fame` plugins.
- Skill compliance is probabilistic — undercounting is a soft failure.
- Users must re-run `vagus skills install` after upgrading to get the ticking skill.
