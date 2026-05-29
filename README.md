# vagus

A local-first **PARA second brain**: a small Rust CLI that gives you **hybrid full-text + semantic
search** over a folder of plain-Markdown notes, with **Claude Code skills** for capture and retrieval.

- **Vault** — plain Markdown in iCloud (`~/brain` → iCloud Drive), [PARA](https://fortelabs.com/blog/para/)
  layout (`00-Inbox / 10-Projects / 20-Areas / 30-Resources / 40-Archive`). Obsidian-compatible.
- **Search** — [tantivy](https://github.com/quickwit-oss/tantivy) BM25 + local ONNX embeddings
  (bge-small) fused with **Reciprocal Rank Fusion**. Fully offline after first run.
- **Capture** — `vim ~/brain/00-Inbox/idea.md` (zero ceremony, no frontmatter required) or
  `/create-note` from a Claude Code session.
- **Filing** — `/process-inbox`: Claude proposes a PARA home per inbox note; you approve.

## Design & guardrails

This project is **guardrails-first**. Before changing anything architectural, read:

- [`CLAUDE.md`](./CLAUDE.md) — the hard invariants every session must respect.
- [`design/`](./design/) — requirements, ADRs (what we considered & why), tradeoff study, prior-art survey.

## Install

```sh
cargo install --path .
```

First **build** downloads a prebuilt ONNX Runtime (a static `libonnxruntime.a`) and links it in;
first **run** downloads the embedding model (~130 MB) to `~/Library/Caches/vagus/models`.
**Verified:** with `ort` 2.0.0-rc.12 on Apple Silicon the installed artifact is a **self-contained
binary** — `otool -L` shows only system dylibs, no `libonnxruntime.dylib` to ship (see
[`design/tradeoffs.md`](./design/tradeoffs.md)).

## Usage

```sh
vagus index                 # incremental: sync the vault into the local index
vagus reindex               # full rebuild from the vault
vagus search "<query>"      # hybrid search (--mode hybrid|bm25|vec, --json, --limit N)
vagus add-note "<title>"    # create an inbox note and index it
vagus inbox                 # list 00-Inbox items
vagus file <path> --to ...  # move a note into a PARA folder, enrich frontmatter, reindex
vagus doctor                # health check (symlink, model cache, dylib, dims, index)
vagus status                # counts, model/dims, index size
```

The index/database live **outside** iCloud (`~/.local/share/vagus/`) and are fully rebuildable from
the Markdown — only your notes live in iCloud.
