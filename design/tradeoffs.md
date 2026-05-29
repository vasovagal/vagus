# Tradeoff study

Distilled from the research that preceded v1. Detailed decisions are in the [ADRs](./adr/); this file
holds the comparison tables.

## A. Engine: build vs adopt

| Option | Pros | Cons | Verdict |
|---|---|---|---|
| **Build fresh (Rust)** | Full control, exact PARA/iCloud behavior, permissive deps, own the code | Most code | **Chosen** — retrieval hand-rolled (small); frankensearch/qmd as references ([ADR 0007](./adr/0007-lean-on-frankensearch.md)) |
| Adopt `qmd` (TS) | SOTA hybrid (BM25+vec+RRF+rerank+HyDE), MCP for Claude | Node/Bun runtime + ~2 GB models; not Rust; Rust variant not on crates.io | Reference design, not adopted |
| Adopt `basic-memory` (Py) | Mature, MCP-native, hybrid FTS+vector, write-back | AGPL; imposes a knowledge-graph model; PARA not built-in | Patterns only |
| Adopt `memex` (TS) | Plain-md + offline search + MCP, single DB | Smaller/newer; semantic-only; own conventions | Reference |
| Adopt `iwe` (Rust) | Pure-Rust, MCP, full note engine | Search is fuzzy/graph only — no BM25/embeddings; link-graph not PARA | Borrow watcher/MCP shape |
| Adopt `MALD` (Rust) | Hybrid PKM, rusqlite bundled | ~4★ solo, no MCP, needs external Ollama | Rejected (bus factor + daemon) |

## B. Language: Rust vs Python

| | Rust | Python |
|---|---|---|
| Author fit | **High** (Rust-fluent, wants ownership) | ok |
| sqlite ext loading | n/a (rusqlite bundled) | stdlib `sqlite3` allows it here |
| Embedding ecosystem | fastembed-rs / candle / model2vec-rs | richest |
| Single binary | yes (modulo ONNX dylib) | no |
| **Verdict** | **Chosen** | — |

## C. Embedding backend

| Backend | Offline | Daemon | Footprint | Quality | Single binary |
|---|---|---|---|---|---|
| **fastembed (ONNX, bge-small 384d)** | ✅ after first run | no | model ~130 MB + onnxruntime dylib | good | **no** (needs `libonnxruntime.dylib`) |
| Ollama (nomic-embed-text 768d) | ✅ | **yes** (daemon) | larger | better | no |
| Cloud (Voyage / OpenAI) | ❌ | no | none | best | n/a (text leaves device) |
| candle (bge-small safetensors) | ✅ | no | model only | same as fastembed | **yes** (pure Rust, hand-rolled tokenize) |
| model2vec (potion, static) | ✅ | no | ~8 MB, instant | ~11 MTEB pts worse | **yes** (pure Rust) |

**Chosen:** fastembed (bge-small) by default ([ADR 0006](./adr/0006-embeddings-local-no-daemon.md));
`model2vec` is the documented dylib-free escape hatch.

## D. The ONNX "single binary" reality (verified on this build)

- **Verified (ort 2.0.0-rc.12, darwin-arm64):** `download-binaries` fetches a **static**
  `libonnxruntime.a` (cached under `~/Library/Caches/ort.pyke.io/…`) and **statically links** it. The
  installed `vagus` references only system dylibs (`otool -L`: libc++, Foundation, Security,
  CoreFoundation, CoreML, libSystem, …) and bundles ~34k onnxruntime symbols — i.e. **self-contained**,
  no `libonnxruntime.dylib` to ship.
- The earlier secondary-source assumption of "binary + dylib via rpath" did **not** hold here — the
  prebuilt is a static archive on this platform/version. (macOS still can't be 100% static — system
  dylibs are always dynamic, QA1118 — but that's normal.)
- Links the system **CoreML.framework** (present on every Mac) for the optional CoreML EP; the CPU EP
  is the default and is sufficient for bge-small.
- Pure-Rust `model2vec`/`candle` remain options to drop onnxruntime entirely, but aren't needed for a
  self-contained binary here.
- `ort`/`ort-sys` are version-locked at `=2.0.0-rc.12` by `fastembed` — don't bump independently.

## E. Vector store

| Option | Simplicity | Single binary | Fit at personal scale (<100k × 384d) |
|---|---|---|---|
| **Brute-force cosine in RAM** (vectors as SQLite BLOBs) | highest | yes | sub-few-ms full scan; **chosen** |
| sqlite-vec (rusqlite loadext) | medium | extension to load | fine, but extra moving part |
| ANN: hnsw_rs / instant-distance / usearch | lower | mostly yes | unnecessary until corpus is huge |

`frankensearch` already implements the brute-force-f16-SIMD + RRF path; if we depend on it we configure
rather than write this.

## F. Filing inbox → PARA

| Option | Effort | Control | Risk |
|---|---|---|---|
| **Assisted, on demand** (`/process-inbox`, user approves) | low | high | low — **chosen** |
| Automatic on capture | none | none | files move unexpectedly |
| Manual only (`mv`) | n/a | total | nothing learns; still searchable |
