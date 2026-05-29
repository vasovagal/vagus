# Guardrails (binding)

The canonical list of invariants for `vagus`. The root `CLAUDE.md` mirrors this in summary; if they
ever diverge, **this file wins**. Changing a guardrail requires updating (or superseding) the relevant
[ADR](./adr/) and this file in the same change.

## Data & storage

- **G1 — iCloud holds Markdown only.** The tantivy index, the SQLite `meta.db`, and the model cache
  live **outside** iCloud (`~/.local/share/vagus/`, `~/Library/Caches/vagus/`). Never place a SQLite
  DB or search index inside the iCloud vault — async multi-file sync of `.db`/`-wal`/`-shm` corrupts
  it. ([ADR 0004](./adr/0004-icloud-markdown-only.md))
- **G2 — The index is a derived cache.** It must be 100% rebuildable from the Markdown via
  `vagus reindex`. Markdown files are the source of truth; the DB never is.
- **G3 — Never auto-edit the user's note.** Frontmatter is optional; a frontmatter-free note must index
  correctly (title ← first `# heading` or filename). Frontmatter is written/enriched only during an
  explicit, user-approved filing step. ([ADR 0005](./adr/0005-assisted-filing.md))

## Index correctness

- **G4 — Pin embedding identity.** `meta` table stores `embed_model`, `embed_dims`, `tantivy_version`.
  Any mismatch ⇒ refuse incremental indexing, require `reindex`. Never mix embedding spaces.
- **G5 — Both stores move together.** On a changed/deleted file, delete its tantivy docs
  (`delete_term(path)` → `commit()`) **and** delete its SQLite vector rows (the vector store has no
  FK/triggers). One mtime+sha256 hash-diff drives both; `doctor` cross-checks counts.
- **G6 — tantivy update pattern.** There is no `update_document`. Per changed file: `delete_term` on
  the exact `path` term, re-`add_document` the new chunks, then a single `commit()`. Full rebuild =
  many adds + one commit.
- **G7 — Normalize vectors at insert** so cosine = dot product.

## Search behavior

- **G8 — RRF k=60.** Fuse BM25 and cosine ranks with `score = Σ 1/(k + rank)`; no score normalization.
- **G9 — bge prefixes.** Prepend the retrieval instruction to *queries* only; documents are
  un-prefixed. Don't double-prefix (respect whatever the embedding lib already applies).
  ([ADR 0003](./adr/0003-search-stack.md))

## Build & dependencies

- **G10 — fastembed cache dir is explicit.** Never rely on fastembed's `./.fastembed_cache` CWD
  default; set it to `~/Library/Caches/vagus/models` via `with_cache_dir` / `FASTEMBED_CACHE_DIR`.
- **G11 — Retrieval is hand-rolled** (tantivy BM25 + brute-force cosine + RRF;
  [ADR 0003](./adr/0003-search-stack.md)). `frankensearch`/`qmd` are **design references, not
  dependencies** ([ADR 0007](./adr/0007-lean-on-frankensearch.md)). Don't add a heavyweight
  search-engine dependency without an ADR; if you ever do, pin/vendor it.
- **G12 — Don't bump `ort` independently.** It's version-locked by fastembed (`=2.0.0-rc.12`).
- **G13 — Honest artifact.** Default ONNX build = binary + `libonnxruntime.dylib` (rpath), not one
  file. Don't claim "single static binary"; the dylib-free path is the `model2vec` backend.

## Product

- **G14 — Local-first / offline by default.** No cloud calls, no background daemon in the default path.
- **G15 — PARA layout fixed.** `00-Inbox / 10-Projects / 20-Areas / 30-Resources / 40-Archive`.
  Filing is assisted + user-approved, **never automatic**.
- **G16 — Obsidian-compatible.** Plain `.md`, optional `[[wikilinks]]`/frontmatter; don't break it.
- **G17 — No LLM inside the binary.** Query-expansion/reranking via an LLM, if ever, belongs in the
  Claude skill layer, not `vagus`.
