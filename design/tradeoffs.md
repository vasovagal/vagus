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

**Chosen:** fastembed by default ([ADR 0006](./adr/0006-embeddings-local-no-daemon.md)); `model2vec` is
the documented dylib-free escape hatch. *(The model was **upgraded 2026-05-30** from bge-small (384d) to
**EmbeddingGemma-300M (768d, 2048-ctx)** on the same fastembed/ort backend — same trade-offs as this
table's fastembed row, larger cache (~1.23 GB). See ADR 0006.)*

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

Re-evaluated 2026-06-07 and amended 2026-07-30
([ADR 0019](./adr/0019-usearch-ann-backend.md)) on **popular / technical / speed** under the hard
static-single-binary constraint, given a >500k-chunk growth trajectory. Scored
1–5 (5 best); `total` weights the static-link constraint:

| Option | Kind | Popular | Technical | Speed | Static-link | SQLite-fit | Total | Verdict |
|---|---|:-:|:-:|:-:|:-:|:-:|:-:|---|
| **usearch** | C++ HNSW (cxx-build) | 4 | 5 | 5 | 4 | 1 | 19 | **chosen** (ADR 0019) |
| tuned brute-force | pure Rust | 4 | 4 | 4 | 5 | 5 | 22 | automatic <10k; all-mode `--exact` oracle |
| sqlite-vec | C ext (`-DSQLITE_CORE`) | 5 | 4 | 3 | 5 | 5 | 22 | stable release still brute-force — no ANN |
| hnsw_rs | pure Rust HNSW | 3 | 3 | 4 | 5 | 1 | 16 | ✗ no delete API → breaks G5 |
| lancedb / lance | Rust + Arrow | 5 | 4 | 5 | 1 | 1 | 16 | ✗ needs system `protoc`/cmake; heavy |
| instant-distance | pure Rust HNSW | 3 | 2 | 4 | 5 | 1 | 15 | ✗ rebuild-only; frozen 2023 |
| arroy | Rust + LMDB | 3 | 3 | 3 | 5 | 1 | 15 | ✗ deprecated by Meilisearch (hannoy) |
| faiss-rs | C++ + BLAS | 3 | 4 | 5 | 1 | 1 | 14 | ✗ `static` still links `gomp/blas/lapack` |
| hora | pure Rust | 2 | 1 | 3 | 5 | 1 | 12 | ✗ cosine NaN-panics; abandoned |

**Chosen:** a scale-selected pair. Tuned exact scan is automatic below 10,000 embedded chunks because
it recovered a live answer HNSW omitted from 120 candidates; a 10k×768 load+search fixture is ~26.6 ms.
Embedded **usearch HNSW** takes over at/above 10k and supplies the >500k trajectory, statically linked
(cxx-build, `openmp` off → OS + `libc++` only, verified `otool`-clean). Explicit `--exact` forces the
oracle in every mode. The f32 BLOBs remain authoritative and `.usearch` remains a rebuildable cache.
`frankensearch` (brute-force-f16-SIMD + RRF) stays a design reference, not a dependency.

## F. Filing inbox → PARA

| Option | Effort | Control | Risk |
|---|---|---|---|
| **Assisted, on demand** (`/process-inbox`, user approves) | low | high | low — **chosen** |
| Automatic on capture | none | none | files move unexpectedly |
| Manual only (`mv`) | n/a | total | nothing learns; still searchable |
