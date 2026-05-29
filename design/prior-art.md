# Prior art surveyed

Tools we examined before deciding to build. Conclusion: no Rust **all-in-one** combines
{hybrid retrieval} ∩ {MCP for Claude} ∩ {pure-Rust} ∩ {PARA}, but every *layer* has strong prior art —
so we build the second-brain layer and lean on `frankensearch` for retrieval.
(Stars/dates are as observed in May 2026.)

## Retrieval engine (the part we lift)

- **[frankensearch](https://github.com/Dicklesworthstone/frankensearch)** — MIT, Rust **library crate +
  `fsfs` CLI**. *Is* our retrieval core: Tantivy BM25 + vector cosine + **RRF k=60**, in-process
  **f16 SIMD brute-force** (optional HNSW behind `ann`), incremental/watch, JSON/agent output, model
  cache at `~/.cache/frankensearch/models` (`FRANKENSEARCH_MODEL_DIR`), pluggable backends
  (`hash` / **`model2vec` = pure-Rust, no ONNX** / `fastembed` = ONNX). Indexes `.md`.
  **Borrow/depend.** Missing: MCP, PARA, capture/filing, heading-aware chunking. Caveat: solo (~56★),
  some sub-crates may not be cleanly publishable → pin/vendor.
- **[codesearch](https://github.com/flupkede/codesearch)** — Apache-2.0, Rust. Proof the full stack
  ships: fastembed + tantivy + BM25+vector+**RRF** + arroy/LMDB + tree-sitter + an **MCP server for
  Claude Code**, fully offline. It's *code* search (tree-sitter AST chunking; markdown falls back to
  line-based). **Borrow** its `rmcp` MCP server shape; swap chunking for our heading chunker. Its
  author notes "many projects share the same baseline stack (Rust + tree-sitter + BM25 + embeddings + MCP)."

## Reference designs / building blocks

- **[qmd](https://github.com/ehc-io/qmd)** (and tobi/qmd) — the hybrid design spec: BM25 + vector + RRF
  (k=60, original-query ×2) + LLM rerank + HyDE/query-expansion, smart markdown chunking. TS/Bun + ~2 GB
  local GGUF models; not Rust-installable. **Spec to match**, not a base to fork.
- **[papers-cli](https://crates.io/crates/papers-cli)** — Rust, fastembed + ort + LanceDB + MCP, ships
  on darwin-arm64. **Proof** the embedding/ort/MCP wiring compiles; lift that wiring (ignore its papers
  domain; we use SQLite-BLOB cosine, not LanceDB).
- **fastembed-rs** (Anush008) — the embedding building block (ONNX via `ort`); bge-small default (384d),
  rerankers + sparse available. Cache dir defaults to `./.fastembed_cache` (CWD) — **must override**.
- **tantivy** (quickwit-oss) — the BM25 building block; the one piece with an honest single-binary
  story. No native vector search; no `update_document` (delete_term → add → commit).
- **EmbedAnything** (StarlightSearch) — Rust embedding/ingestion pipeline (candle + onnx backends);
  library, not a search engine.

## All-in-one note managers (miss the retrieval half)

- **[iwe](https://github.com/iwe-org/iwe)** — Apache-2.0, ~1.1k★, pure-Rust note engine with CLI + LSP +
  **MCP**, refactoring ops, `notify` file-watch. But search is **fuzzy title/path + graph traversal
  only** (no BM25, no embeddings), and it's link-graph-centric, not PARA. **Borrow** the watcher / MCP
  shape; don't fork the retrieval model.
- **[MALD](https://github.com/NAME0x0/MALD)** — Rust PKM, hybrid (HNSW + FTS5), ~40 commands, watcher,
  rusqlite bundled. But ~4★ solo, **no MCP**, semantic needs external **Ollama**. Rejected as a base.
- **memex / basic-memory / khoj / txtai** — strong in their lanes (offline embeddings, MCP write-back,
  self-hosted RAG, embeddings DB) but TS/Python and/or heavier (Postgres/Django, AGPL) than a personal
  Rust CLI wants. Patterns only.
- TUI/link-only Rust note tools (`rucola`, `mdzk`, `zk`, `settle`, …) — pure-Rust single binaries but
  fuzzy/link search only; no semantic/hybrid, no MCP.
