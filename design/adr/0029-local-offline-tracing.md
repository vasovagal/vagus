# ADR 0029 — Privacy-projected local offline tracing

- **Status:** Proposed (2026-08-22); implements the accepted cross-project
  [Vasovagal tracing architecture v1](https://github.com/vasovagal/vasovagal-tracing/blob/7afe13e46df63a3767d518ede7b733349dc09b14/docs/architecture-v1.md).

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

The independently reviewed core PR squash-landed as
`7afe13e46df63a3767d518ede7b733349dc09b14`, but no tag or crates.io release exists yet. This draft
integration therefore pins that exact immutable Git `rev`. A Git/path dependency may not merge; once
the reviewed registry release is verifiably available, release remediation must use its crates.io
semver dependency and reviewed lockfile.

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
subscriber is installed when disabled. Before installation, Vagus side-effect-free resolves the
shared contract's fixed prospective trace directory and its configured vault with the existing
missing-path/symlink-aware G1 resolver. Any equality, containment, or symlink-alias overlap declines
the layer before state creation. (The landed core v0.1 API deliberately defers storage to
`finish(true)` and exposes no prospective-path accessor, so this check mirrors its immutable fixed
location rather than opening storage.) The root span closes on both ordinary errors and typed
external-plugin exits before a two-second best-effort guard shutdown; only then does the process
propagate the child's exact nonzero status.

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
parent, enter it only inside their active closure, and live in a join-on-scope owner that drains both
workers on every success, `?`, fallback, or unwind before shutdown. Scope filtering re-enters its
aggregate span, while the postprocess span remains alive and accounts filters, floors, adaptive tidy,
and relevance projection. Index and smart variant/note work reuse aggregate spans rather than
creating per-note/chunk/query-variant/token/SQL spans.

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

### 2026-08-22 reproducible performance evidence

`scripts/benchmark-local-tracing.py` created a deterministic 128-note/768-section Markdown corpus,
indexed it from the existing offline model cache, then alternated 15 traced/untraced pairs for BM25
`--no-index` search and unchanged incremental indexing. On Apple arm64 with the Rust 1.96 debug test
binary, median search was 18.479 ms untraced / 25.105 ms traced (+6.625 ms), and index was 31.439 ms /
36.586 ms (+5.148 ms). Both satisfy the accepted “5% or 10 ms, whichever is larger” gate. Across 32
graceful sessions, the largest trace was 11,744 bytes; every session had a summary, no queue drops,
writer/rejection/privacy counters, or synthetic query text. The machine-readable release evidence is
[`design/evidence/local-tracing-performance.json`](../evidence/local-tracing-performance.json).

## Alternatives considered

- **OTLP/OpenTelemetry plus a local collector:** rejected. Even a nominally local setup introduces
  endpoints, collector lifecycle, network-capable dependencies, and accidental export risk.
- **Vagus-specific JSON timing logs:** rejected. It duplicates Corti's need, lacks tracing-rs
  composition/parentage, and would drift from one validated privacy schema.
- **Reuse human diagnostics / `--timings`:** retained for their current purpose but rejected as the
  batch format. Their strings are user-facing, incomplete, and may contain raw errors or paths.
- **Arbitrary output paths or arbitrary span fields:** rejected. Fixed state paths and a closed
  catalogue make ownership/mode checks, retention, validation, and privacy review tractable.
