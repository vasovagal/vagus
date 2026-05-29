# ADR 0002 — Language: Rust

- **Status:** Accepted (2026-05-29)

## Context

Earlier drafts assumed Python (rich embedding ecosystem, stdlib `sqlite3` can load `sqlite-vec`). The
author is Rust-fluent, prefers a single owned binary, and likes the tantivy/fastembed-rs stack.

## Options considered

1. **Python + uv** — strongest embedding ecosystem; stdlib sqlite allows extension loading on this Mac;
   no single binary.
2. **Rust** — single binary (modulo the ONNX dylib), mature crates (tantivy, fastembed-rs, rusqlite),
   author ownership; `frankensearch` (the engine we lean on) is Rust.

## Decision

**Rust.** A single CLI binary `vagus`. Aligns with the author's fluency and the chosen engine, and the
components compose cleanly without Python's runtime/venv concerns.

## Consequences

- Crate stack: `tantivy`, `fastembed`(→`ort`), `rusqlite` (bundled), `pulldown-cmark`, `clap`,
  `walkdir`, `sha2`, `dirs`, `serde`/`serde_json`. See [ADR 0003](./0003-search-stack.md).
- ONNX Runtime linking introduces a `libonnxruntime.dylib` (not a pure single file) — see
  [tradeoffs §D](../tradeoffs.md) and [ADR 0006](./0006-embeddings-local-no-daemon.md).
