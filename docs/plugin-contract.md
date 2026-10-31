# vagus plugin contract

Implement a `vagus-<name>` executable to add a networked capture command without adding network
code to Vagus core. This normative, language-independent contract covers dispatch, process channels,
environment, events, and plugin obligations. Rust authors may use the optional `vagus-plugin` SDK
and `vagus-plugin-protocol` wire-schema crate.

## Discovery & dispatch

- Put an executable named `vagus-<name>` on `$PATH`; the first matching path wins.
- `vagus <name> <args…>` starts it with the subcommand name stripped:
  `vagus slack pull --since 7d` passes `["pull", "--since", "7d"]` to `vagus-slack`.
- Built-in commands win over same-named plugins. `vagus plugins` lists plugins and marks shadowed names.
- Reserve `__`-prefixed subcommands for the protocol; `__describe` is currently defined.

## Process model

Core starts a child process, inheriting stdin and stderr and piping stdout:

| Channel | Use |
|---|---|
| stdin | Terminal input and prompts. |
| stderr | Human-readable logs, progress, and errors. |
| stdout | One NDJSON event per line. Core echoes unrecognized lines to its own stdout. |

A nonzero child exit code is propagated. A terminal `result` with `ok: false` also fails the command.
After a successful run, at least one `note` event triggers **one whole-vault incremental refresh**,
unless `result.no_index` is true. The paths are change notifications, not a selective indexing list.
An interrupted rebuild makes that automatic refresh warn and defer until explicit `vagus index`
or `vagus reindex` resumes it.

<a id="environment-set-by-core-for-every-plugin-run"></a>

## Environment (set by core for dispatched commands)

Discovery calls to `__describe` inherit the caller environment; core does not set these variables
for discovery.

| Variable | Meaning |
|---|---|
| `VAGUS` | Absolute path to the running Vagus binary, for callbacks. |
| `VAGUS_VAULT` | Absolute path to the resolved vault root; see the implementation caveat below. |
| `VAGUS_DATA_DIR` | Core data directory; informational, not plugin storage. |
| `VAGUS_CONFIG_DIR` | Core config directory; informational, not plugin storage. |
| `VAGUS_VERSION` | Core version string. |
| `VAGUS_PLUGIN_PROTOCOL` | `ndjson` when core parses events; unset for standalone use. |
| `VAGUS_PLUGIN_CONTRACT` | Decimal contract version, currently `1`. |

**Implementation caveat:** the contract requires an absolute, resolved `VAGUS_VAULT`, but current
core forwards the configured path unchanged (`~/brain` by default). Relative overrides and symlinks
are not canonicalized; plugins must not assume that core resolved them.

When run directly, print human output and index changed notes by calling `$VAGUS index`, or
`vagus index` from `$PATH` if `VAGUS` is unset. The SDK switches output modes and resolves that
binary path; **the plugin must invoke indexing itself**.

## The NDJSON event stream (stdout)

Emit one JSON object per line, tagged by `type`. Ignore unknown fields in known event types.

```jsonc
{"type":"log","level":"info|warn|error","msg":"..."}
{"type":"progress","done":3,"total":10,"msg":"fetching #proj-foo"}   // total optional ⇒ indeterminate
{"type":"note","path":"30-Resources/slack/scientist/x.md","action":"write|append|delete"}
{"type":"result","ok":true,"summary":{...},"data":{...},"no_index":false}
```

- `note.path` is relative to `VAGUS_VAULT`; omitted `action` defaults to `write`.
- Emit `progress` and `note` as work happens, then exactly one terminal `result`.
  A batch command may emit only `result`, but without a `note` event core does not refresh the index.
- `summary` and `data` are optional plugin-defined JSON; `no_index` defaults to false.
- Unknown event types and malformed event lines are echoed, not acted on.

## Plugin obligations (the rules)

1. Write only `.md` files under `$VAGUS_VAULT`, with no `..` escape. Keep state, caches, and cursors
   in the plugin's own XDG directories (`~/.local/share/vagus-<name>/`, `~/.config/vagus-<name>/`),
   never in the vault or core's directories.
2. In protocol mode, emit `note` events instead of invoking indexing yourself.
3. Own your configuration; core does not parse it.
4. Keep stdout for events and stderr for people.
5. Implement `vagus-<name> __describe`: print a one-line summary to stdout and exit 0 promptly.
   `vagus plugins` uses it for discovery.

## Versioning

Check `VAGUS_PLUGIN_CONTRACT` for compatibility. Add optional fields and event variants without
changing the version; older cores echo unknown variants without acting on them. Increment the
contract only for breaking changes. See [ADR 0011](../design/adr/0011-plugin-protocol.md).
