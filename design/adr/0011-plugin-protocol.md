# ADR 0011 — Plugin protocol: NDJSON event stream

- **Status:** Accepted (2026-05-30)

## Context

[ADR 0010](./0010-plugin-subcommands.md) dispatches `vagus <name>` to a child `vagus-<name>`. We then
need a contract for how the plugin talks back. Two models:

- **Level 0 — exec/forward (git):** core `exec`s the plugin; it owns stdio; exit code is the result.
  Dead simple, but core disappears: no uniform progress rendering, and the plugin must shell back into
  `vagus index` itself.
- **Level 1 — structured stream:** core stays parent, pipes stdout, and the plugin emits structured
  events. Core can render progress uniformly and **index results itself**.

## Decision

Adopt **Level 1 now** with a newline-delimited JSON (NDJSON) event stream.

- **stdout = machine channel** (NDJSON events). **stderr = human channel** (logs/progress/errors,
  inherited to the terminal). This split is the single most important rule.
- Events are externally tagged on `type`: `log`, `progress`, `note`, `result` (schema in
  `docs/plugin-contract.md`).
- **One schema serves batch and streaming:** emit incremental `progress`/`note` then a final
  `result` (streaming), or only the `result` (batch). The plugin chooses.
- **Indexing is core's job.** Plugins emit `note` events (vault-relative paths); after a clean exit
  core runs one incremental `index::run`. A `result.no_index = true` (e.g. `--dry-run`) skips it.
  Standalone runs (`VAGUS_PLUGIN_PROTOCOL` unset) self-index via `$VAGUS index`.
- **Graceful degradation:** a stdout line that isn't a known event is echoed verbatim, so a text-only
  plugin works without speaking the protocol.
- The wire schema lives in a tiny shared crate, **`vagus-plugin-protocol`**, depended on by both core
  and the `vagus-plugin` SDK so it cannot drift. Non-Rust plugins follow the documented JSON.
- Versioned via `VAGUS_PLUGIN_CONTRACT` (currently `1`); the schema evolves additively and the integer
  bumps only on breaking changes.

## Alternatives considered

- **Level 0 only** — rejected: we want uniform progress UI and core-side indexing, and adding the
  protocol later would be a breaking change to the dispatch path.
- **A bidirectional RPC (JSON-RPC/LSP-style) over a socket** — rejected as far too heavy for one-shot
  capture commands; a one-way stdout event stream is sufficient.
- **Mandating events (no text passthrough)** — rejected: would break trivial shell-script plugins for
  no real gain.

## Consequences

- Core spawns (not `exec`s) plugins and owns a small NDJSON parser/renderer (`src/plugin.rs`).
- Plugins get a clean obligation set; the `vagus-plugin` SDK's `Emitter` and `write_note` implement it.
- `note`-driven indexing means plugins never re-enter `vagus` in the common path.
