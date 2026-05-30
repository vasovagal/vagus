# ADR 0009 — CWD-scoped search exclusion ("inherited" `.vagus` config)

- **Status:** Accepted (2026-05-29)

## Context

The author works across two employers (viasat, scientist) in separate code trees (`~/code/viasat`,
`~/code/assaydepot`, `~/code/scientist`). When searching the shared second brain from inside one tree,
hits belonging to the other employer are noise. The vault already encodes the employer as a path
component (`10-Projects/scientist/…`, `30-Resources/viasat-internal/…`), and `Hit.path` is
vault-relative — so the employer is recoverable from the path alone, with no schema or index change.

## Decision

Scope results to the current working directory via "inherited" config files that live in the code
trees, never in the vault:

- **Walk UP** from the CWD collecting `.vagus/config.json` (flat `.vagus.json` fallback) and **union**
  their `exclude` word lists into one "inherited config". A `"root": true` entry **seals** a directory
  from its ancestors; the walk also stops at `$HOME` and the filesystem root.
- **Match = case-insensitive SUBSTRING** of an excluded word against the vault-relative path.
  (`scientist` hides `.../scientist/...`; `viasat` also hides `viasat-internal`.)
- **Behavior = "remove + notice":** filter the already-ranked top-`limit` results, drop the matches,
  and show the remainder (which may be fewer than `limit`), followed by
  `— N hit(s) elided by inherited config (--all to show)`. This is **not** backfill — chosen for
  simplicity and transparency. `--all` bypasses scoping entirely.
- **Notice destination:** stdout (human, dimmed) and **stderr under `--json`**, so the `--json`
  Hit-array contract is unchanged.
- **New module `src/scope.rs`**, wired into `search::query` / `search::run`. Filing
  (`notes.rs --suggest`) stays unscoped by passing `Scope::none()`.
- **Zero new dependencies** (reuses serde / serde_json / dirs).

## Consequences

- Honors **RRF k=60** ([guardrail G8](../guardrails.md), [ADR 0003](./0003-search-stack.md)): only the
  already-truncated top-`limit` list is filtered; `rrf()` is untouched (only its truncation length is
  what we trim from).
- Honors the stable **`--json` shape** (the CLAUDE.md / skills convention): the Hit array is unchanged;
  only stderr carries the notice.
- Config files live **outside** the iCloud vault ([guardrail G1](../guardrails.md): the vault holds
  Markdown only).
- The bundled `/search` skill picks this up **transparently** because it already runs `vagus search`
  from the user's CWD.

## Alternatives considered & rejected

- **Gitignore-style globs** — needs a `globset` dependency; overkill for substring word matching.
- **A frontmatter `employer:` field** — needs a chunks-table schema change and a forced reindex.
- **`.vagus.toml`** — needs a `toml` dependency; JSON is already in the tree (serde_json).
- **Over-fetch BACKFILL to always fill `--limit`** — more complex; the author preferred remove + notice.
