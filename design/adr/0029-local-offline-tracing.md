# ADR 0029 — Small opt-in search tracing: safe and research profiles

- **Status:** Accepted redesign (2026-08-23). **Supersedes the proposed privacy-projected,
  local-only shared-exporter design in PR #33**, including its schema-v1 catalogue, YAML activation,
  silent failures, rotation/retention and local-only transport restriction. This decision applies to
  Vagus only; the shared crate and Corti are unchanged.

## Context

The purpose is to diagnose happy-path real search performance: model loading versus inference,
rewrite cache versus generation, vector opening versus searching, and preparation versus scoring.
The previous adapter catalogue and aggregate re-entry accounting obscured that work and added a
second instrumentation API. Research also needs explicit query/candidate/score/model-input context.
This is observability, not another evaluation framework or a reliable evidence archive.

## Decision

Use ordinary `tracing::instrument(skip_all)` annotations and small explicit spans/events. One small
subscriber module configures standard `tracing-subscriber` JSON files and `tracing-opentelemetry` with
`opentelemetry-otlp`'s direct HTTP/protobuf exporter and standard SDK batching. All dependencies come
from the registry. There is no custom recorder, queue, retry, spool, sync/durability protocol,
artifact/replay system, middleware, corpus snapshot or new evaluation framework.

### Activation and transport

Tracing is off by default. The unconditional flags are:

- `--trace`: safe profile, default private JSONL file.
- `--trace-profile safe|research`: explicitly enables that profile.
- `--trace-file PATH`: a **new** JSONL file, enabling safe unless a profile is selected.
- `--trace-otlp`: explicitly enables direct export; requires `OTEL_EXPORTER_OTLP_ENDPOINT` or
  `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`. Uses safe unless a profile is selected. No implicit collector.
  Without `--trace-file`, OTLP-only runs create no local trace file.

CLI profile takes precedence, then any enabling CLI flag selects safe, then `VAGUS_TRACE_PROFILE`
(`off|safe|research`), then legacy exact `VASOVAGAL_TRACE=true|false`. No YAML is read. `RUST_LOG`, OTEL
endpoint/resource settings, and third-party tracing callsites cannot activate tracing or content.
**Research plus `--trace-otlp` authorizes content export** to the explicitly configured destination.
Standard OTEL endpoint/header settings are consumed only for that exporter; credentials are never
recorded. HTTP/protobuf is selected programmatically; gRPC is not provided. Export request timeout
and best-effort shutdown are capped at two seconds, overriding ambient OTEL timeout settings.

The default file is `${XDG_STATE_HOME:-$HOME/.local/state}/vasovagal/traces/vagus/<unique>.jsonl`.
Before directory creation, G1's alias-aware missing-path resolver must prove the prospective output
directory does not overlap the configured vault. Check again after creating directories. New
directories are 0700, existing output directories must be private, and files are create-new 0600
(no overwrite or symlink-following final file). Files are ordinary standard tracing JSONL: not the
old schema-v1 format. Local writes are synchronous standard file writes, without fsync, retention,
rotation, loss counters, recovery, or reliability claims; users manage sensitive files themselves.
As with other G1 checks, this is not a sandbox against concurrent hostile ancestor replacement.

Initialization/storage/export/shutdown errors are visible on stderr, reduced to non-content
messages for configuration/export failures, but do not change the application's result/status.
Standard JSON writer errors are reported by tracing-subscriber. A subscriber conflict disables
tracing. Root spans close before the SDK's bounded shutdown, including exact external-plugin exits.
There is no guarantee of delivery: crashes, full SDK queues, detached workers, and endpoint failures
may lose traces. No default network path, signal handler or background daemon is added.

### Profiles and instrumentation

Both output layers accept **only** the explicit application targets:

- `vagus::timing`: timings, counts, settings, and outcomes. All function arguments are `skip_all`;
  only explicitly selected enums/booleans/counts enter safe spans. No query, path, note metadata,
  hash/cache key, raw error, prompt, environment/host identity or plugin arguments.
- `vagus::research`: enabled only by the research profile. One event per meaningful stage holds
  query/rewrite text, exact rewrite prompt, lexical/vector/fused candidates and scores, hydrated
  paths/snippets/retained bodies, prefixed embedding query input, rerank query/documents, and rerank
  logits. Expensive JSON serialization stays inside enabled event macros. No per-token/candidate
  spans. No credentials or unrestricted third-party logging. SDK error diagnostics go only to stderr
  as a fixed warning, never into either exporter. Resources contain only the fixed service name.

Instrumentation covers config/storage; index snapshot/reconciliation/commit/persistence and refresh;
model load/inference; rewrite cache/generation; vector selection/open/rebuild/search; lexical search,
fusion/hydration; rerank document preparation/inference; postprocessing and output. Settings reflect
actual retrieval/rerank counts, model context limits, and effective vector backend. Every stage span
is a contiguous wall-time scope. JSON NEW/CLOSE timestamps give wall intervals (standard fmt close
busy/idle fields are not custom CPU accounting); OTLP spans have native start/end times and parents.
Model batch calls can create repeated inference spans during indexing; no per-note wrapper is added.

Smart prewarm threads explicitly receive the dispatcher and parent span. Original `JoinHandle`
ownership and fallback behavior are unchanged; **error/empty-result paths can detach workers**, and
shutdown need not retain their traces. Do not change search lifetimes just to improve telemetry.
Research captures exact application strings passed to fastembed, not private ONNX/tokenizer tensors
or padded/truncated pairs inside fastembed. Rewriter raw token streams are not recorded. Hydrated
bodies exist only when the original search requested them (full output or reranking); do not add DB
reads or retain data just for traces. No model downloads are added by instrumentation.

`vagus eval` has an eval parent and a per-query opaque ordinal ID, with shared search spans below it.
It does **not** expose query text in safe mode. ADR 0024/0025 reports, metrics, schemas, provenance,
and promotion gates remain authoritative and unchanged. Traces are not evaluation evidence.

The default `local-tracing` feature name is retained for compatibility despite optional networking.
Without it all trace flags remain accepted but inert: no tracing config/environment/state reads,
subscriber installation or instrumented calls. `generate` remains independent.

## Verification and consequences

`tests/local_tracing.rs` uses synthetic real SQLite/Tantivy search and a real loopback OTLP receiver
that decodes standard protobuf. It checks off/safe/research stdout/stderr/status equality, content
partitioning, JSON files and modes, OTLP parent/time intervals, no ambient resource capture, explicit
activation, invalid output/vault aliases, external-plugin status, exporter failure, bounded shutdown,
and the compiled-out lane. No model downloads or live corpus exports are needed for these tests.
Real-model timing and prewarm verification require already-cached models with isolated derived state;
synthetic lexical tests cannot prove model latency or model-backed span coverage.

The obsolete custom-schema benchmark and its performance artifact are removed, not represented as
current evidence. No overhead guarantee is claimed for synchronous JSON or research serialization.
Tracing does not alter ranking, filtering, dedup, worker ownership, stdout/search JSON, or eval gates.
Unlike the superseded proposal, explicit research traces may contain sensitive content and explicit
OTLP may transmit it. Keep private traces out of public issues/artifacts and use a trusted destination.
