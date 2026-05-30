# vagus plugin contract

`vagus` supports **plugins** the way `git`, `kubectl`, `cargo`, and `gh` do: a subcommand that isn't
built in is dispatched to an external `vagus-<name>` executable found on `$PATH`. `vagus slack pull …`
runs `vagus-slack pull …`. This keeps **core offline** (guardrail G14/G18) — all network/third-party
code lives in plugins — and lets plugins ship and version independently.

This document is the **normative contract**. It is language-agnostic: a plugin can be a shell script,
a Python program, or a Rust binary. Rust authors get an optional SDK (`vagus-plugin`) and a shared
wire-schema crate (`vagus-plugin-protocol`) that implement everything below; nothing requires them.

## Discovery & dispatch

- A plugin is any executable on `$PATH` named `vagus-<name>` (e.g. `vagus-slack`).
- `vagus <name> <args…>` spawns `vagus-<name>` with **the subcommand name stripped**:
  `vagus slack pull --since 7d` → `vagus-slack` receives `["pull", "--since", "7d"]`.
- **Builtins win.** If `<name>` is a builtin subcommand, the builtin runs; a same-named plugin is
  shadowed. `vagus plugins` lists discovered plugins and flags shadowed ones.
- `__`-prefixed subcommands are **reserved** for the protocol (currently `__describe`).
- First match on `$PATH` wins when a name appears in multiple dirs.

## Process model

Core spawns the plugin as a **child** (it does not `exec`/replace itself) with:

- **stdin** — inherited (prompts/interactivity reach the terminal).
- **stderr** — inherited. This is the **human channel**: logs, progress, errors.
- **stdout** — piped. This is the **machine channel**: the NDJSON event stream (below).
- environment — the variables below are set.

Core reads stdout line by line. A line that parses as a known event is acted on; **any other line is
echoed verbatim to core's stdout**, so a trivial plugin that just prints text still works.

On a clean exit (code 0), core runs **one incremental index pass** over every `note` path the plugin
emitted (unless the terminal `result` set `no_index`). A non-zero exit code is propagated by core.

## Environment (set by core for every plugin run)

| Variable                | Meaning                                                                 |
|-------------------------|-------------------------------------------------------------------------|
| `VAGUS`                 | Absolute path to the `vagus` binary (for callbacks like `$VAGUS index`).|
| `VAGUS_VAULT`           | Absolute path to the resolved vault root (the `~/brain` target).        |
| `VAGUS_DATA_DIR`        | vagus data dir (informational).                                         |
| `VAGUS_CONFIG_DIR`      | vagus config dir (informational).                                       |
| `VAGUS_VERSION`         | Core's version string.                                                  |
| `VAGUS_PLUGIN_PROTOCOL` | `ndjson` when core is parsing events. Unset ⇒ standalone/direct run.    |
| `VAGUS_PLUGIN_CONTRACT` | Decimal contract version core supports (currently `1`).                 |

**Standalone runs.** When a plugin is executed directly (not via `vagus`), `VAGUS_PLUGIN_PROTOCOL`
is unset. The plugin should then print human output and **self-index** by calling `$VAGUS index`
itself. The `vagus-plugin` SDK handles both modes transparently.

## The NDJSON event stream (stdout)

One JSON object per line, externally tagged on `type`. Unknown future fields must be ignored.

```jsonc
{"type":"log","level":"info|warn|error","msg":"..."}
{"type":"progress","done":3,"total":10,"msg":"fetching #proj-foo"}   // total optional ⇒ indeterminate
{"type":"note","path":"30-Resources/slack/scientist/x.md","action":"write|append|delete"}
{"type":"result","ok":true,"summary":{...},"data":{...},"no_index":false}
```

- **`note.path` is relative to `VAGUS_VAULT`.** Core indexes these after the run.
- **Streaming vs batch:** emit `progress`/`note` as work happens then a final `result` (streaming), or
  emit only the final `result` (batch). Same schema either way.
- Exactly one terminal `result` should be emitted.

## Plugin obligations (the rules)

1. **Markdown only, in the vault.** Write only `.md` files, only under `$VAGUS_VAULT`, never with a
   `..` escape (encodes G1/G16). State, caches, and cursors go in the plugin's own XDG dirs
   (`~/.local/share/vagus-<name>/`, `~/.config/vagus-<name>/`) — **never** in the vault.
2. **Don't index yourself in protocol mode.** Emit `note` events; core indexes once at the end.
3. **Own your config.** Core never parses plugin config.
4. **Respect the channels.** stdout = events only; humans read stderr.
5. **Implement `__describe`.** `vagus-<name> __describe` prints a one-line summary to stdout and exits
   0. Used by `vagus plugins`.

## Versioning

`VAGUS_PLUGIN_CONTRACT` lets a plugin check compatibility. The schema evolves **additively** (new
optional fields, new event variants that older cores ignore); the integer bumps only on a breaking
change. See ADR 0011.
