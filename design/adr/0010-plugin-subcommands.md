# ADR 0010 — Plugins via external `vagus-<name>` subcommands

- **Status:** Accepted (2026-05-30)

## Context

vagus core is deliberately offline (G14): no cloud calls, no daemon. But useful capture sources are
inherently networked — Slack, GitHub, Readwise, calendars. Baking any of them into the `vagus` binary
would (a) violate G14, (b) drag heavyweight, churny dependencies (HTTP stacks, OAuth) into the search
core, and (c) couple every integration to vagus's release cadence.

We want a way to add networked/third-party capture **without touching core** and **without eroding
the offline guarantee**.

## Decision

Adopt the **git / `kubectl` / `cargo` / `gh`-extension pattern**: a subcommand that isn't built in is
dispatched to an external executable named `vagus-<name>` found on `$PATH`. `vagus slack pull` →
`vagus-slack pull`.

- clap gains an `External(Vec<OsString>)` catch-all variant (`allow_external_subcommands`).
- Core **spawns the plugin as a child** (it does not `exec`), so it stays in control to render
  progress and to index results afterwards (see [ADR 0011](./0011-plugin-protocol.md) for the wire
  protocol that this enables).
- `vagus plugins` discovers `vagus-*` executables on `$PATH`, dedupes (first wins), and flags any that
  are shadowed by a builtin.
- The plugin contract is a **language-agnostic spec** (`docs/plugin-contract.md`), not a mandatory
  library: a plugin can be a shell script. Rust authors may use the optional `vagus-plugin` SDK.

## Alternatives considered

- **Monolith with cloud features behind flags** — rejected: violates G14 and bloats core deps.
- **A remote plugin registry / installer (krew-style)** — rejected as overkill for a personal tool;
  Homebrew + `$PATH` is enough. Can revisit if a third-party ecosystem ever appears.
- **A required SDK crate plugins must link** — rejected: forces Rust, couples versions, and kills the
  "drop any executable on PATH" simplicity. The *contract* is the spec; the crate is optional sugar.

## Consequences

- Core stays offline; networked features ship as independently-released, independently-brewed
  `vagus-*` plugins. This is recorded as guardrail **G18**.
- vagus becomes a small **cargo workspace** to host the shared `vagus-plugin-protocol` schema crate
  and the `vagus-plugin` SDK; the `vagus` binary stays at the repo root (no source move).
- `__`-prefixed subcommands are reserved for the protocol (e.g. `__describe`).
- First consumer/dogfood: `vagus-slack`.
