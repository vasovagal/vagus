# ADR 0029 — Privacy-projected local offline tracing

- **Status:** Proposed (2026-08-22); implements the accepted cross-project
  [Vasovagal tracing architecture v1](https://github.com/vasovagal/vasovagal-tracing/blob/eebe5bbbba597b64dabd2d1981d18ba71bab9869/docs/architecture-v1.md).

## Context

Vagus has stable user-facing output and opt-in `--timings`, but no structured way to compare command,
index, retrieval, model, and storage-stage latency across many local runs. Corti needs the same local
analysis substrate. Conventional observability stacks add collectors, endpoints, network transports,
arbitrary fields, or log payloads; each conflicts with Vagus's local-first/privacy contract and makes
query/note leakage too easy.

The shared format must remain useful after the process exits, tolerate abrupt final-line truncation,
and fail closed when disabled, misconfigured, insecure, or compiled out. It must not alter ordinary
stdout/JSON/stderr, command errors, model behavior, vault safety, or index consistency.

## Decision

Vagus integrates the public MIT-licensed `vasovagal-tracing` crate behind a default-on Cargo feature
named **`local-tracing`**. The feature owns optional `tracing`, `tracing-subscriber`, and shared-crate
dependencies. `--no-default-features` omits all three and every instrumentation call becomes a
zero-sized no-op: the binary reads no tracing environment/YAML, creates no tracing path, but still
silently accepts the unconditional global `--trace` flag.

Until the shared crate completes its independently reviewed crates.io release, the implementation PR
pins the pushed core commit `eebe5bbbba597b64dabd2d1981d18ba71bab9869` with an exact Git `rev`.
A Git/path dependency may not merge; after tokenless publication succeeds, the release-remediation
commit must use registry `version = "0.1.1"` and its reviewed lockfile.

### Activation and lifecycle

With `local-tracing` compiled, activation is resolved only by the shared crate, in this exact order:

1. `--trace` enables and short-circuits every lower source;
2. a present `VASOVAGAL_TRACE` must be exactly `true` or `false` and short-circuits YAML;
3. `${XDG_CONFIG_HOME:-$HOME/.config}/vasovagal/vagus.yaml` must be the strict schema-v1 document;
4. otherwise tracing is disabled.

Invalid environment/config, missing home, insecure/unwritable storage, repeated initialization, or a
subscriber conflict produces no application error and no trace. Initialization occurs immediately
after `Cli::parse()` and before the `eval-gate` fast path, config, vault, DB, model, or command work.
The app composes the optional exact-target layer on a bare `tracing_subscriber::Registry`; no
subscriber is installed when disabled. The root span closes on both `Ok` and `Result` error before a
two-second best-effort guard shutdown.

Activated JSONL is written only to:

```text
${XDG_STATE_HOME:-$HOME/.local/state}/vasovagal/traces/vagus/
```

The shared crate owns private `0700` directories, create-new/no-follow locked `0600` files, rotation,
retention, lossy buffering, complete-line writes, graceful summaries, validation, and partial-tail
recovery. There is no path override, endpoint, socket, collector, daemon, upload, or signal handler.

### Instrumentation and privacy

Only exact-target (`vasovagal::trace`) schema-v1 catalogue spans are emitted:

```text
vagus.command
├── vagus.config.load
├── vagus.storage.validate
├── vagus.index
│   ├── snapshot
│   ├── reconcile       # includes bounded SQLite reconciliation
│   ├── embed
│   ├── lexical_commit
│   └── vector_persist
└── vagus.search
    ├── refresh
    ├── scope
    ├── retrieve
    │   ├── bm25
    │   ├── vector
    │   └── rrf
    ├── hydrate         # bounded SQLite hydration
    ├── rewrite
    ├── rerank
    └── postprocess
```

`vagus.model.load`, `vagus.model.decode`, and `vagus.model.infer` are explicit children of the
consuming stage. Smart-search prewarm workers receive the current dispatcher and an explicit cloned
parent, enter it only inside their active closure, and are joined before shutdown. Index and smart
variant/note work reuse aggregate spans rather than creating per-note/chunk/query-variant/token/SQL
spans.

The application can provide only catalogue enums, booleans, and bounded aggregate counts. It never
passes query/variant text, note content/title/heading/snippet/frontmatter/source values, paths,
filenames, plugin argv, hashes/cache keys, stored queries, prompts, raw errors, environment contents,
host/user/cwd/executable/thread identifiers, or arbitrary `Debug`/`Display` values. Errors are reduced
to reviewed low-cardinality codes; unknowns are `other`. The shared layer independently rejects and
counts any unknown, wrongly typed, or privacy-denied field, and every emitted line validates against
the bundled immutable JSON Schema.

## Consequences

- Explicit local traces support `jq`, DuckDB, Python, Polars, and SQLite batch analysis without a
  running service or network collector.
- Default commands pay no runtime tracing/storage cost beyond compiled callsites; compiled-out builds
  omit even activation reads. Enabled writes use a bounded lossy queue so tracing does not block
  retrieval/model hot paths.
- Trace files reveal coarse command/stage timing and aggregate sizes to the local account. They are
  therefore private `0600` state with bounded retention, not diagnostics to attach wholesale to an
  issue.
- Abrupt termination may lose queued/final records and omit the summary; all preceding newline-ended
  records remain valid, and readers may allow one partial tail.
- Schema-v1 operation/attribute additions require a shared schema-v2 decision, preventing an app from
  quietly expanding the privacy surface.

## Alternatives considered

- **OTLP/OpenTelemetry plus a local collector:** rejected. Even a nominally local setup introduces
  endpoints, collector lifecycle, network-capable dependencies, and accidental export risk.
- **Vagus-specific JSON timing logs:** rejected. It duplicates Corti's need, lacks tracing-rs
  composition/parentage, and would drift from one validated privacy schema.
- **Reuse human diagnostics / `--timings`:** retained for their current purpose but rejected as the
  batch format. Their strings are user-facing, incomplete, and may contain raw errors or paths.
- **Arbitrary output paths or arbitrary span fields:** rejected. Fixed state paths and a closed
  catalogue make ownership/mode checks, retention, validation, and privacy review tractable.
