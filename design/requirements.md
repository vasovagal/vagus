# Requirements

## Problem / intent

A personal "second brain": capture thoughts and references with near-zero friction, organize them with
the [PARA](./methodology-para.md) method, and **find them again** later by keyword *or* by meaning —
all local, private, and durable as plain text. Driven from the terminal and from Claude Code.

## Functional requirements

- **F1 — PARA vault of plain Markdown.** Folders `00-Inbox / 10-Projects / 20-Areas / 30-Resources /
  40-Archive` (+ optional `50-Meta`). One `.md` file per note; whole-folder `mv` for lifecycle moves.
- **F2 — Frictionless capture.** `vim ~/brain/00-Inbox/idea.md`, type, save — *no required frontmatter*.
  Also `/create-note` to capture from a Claude Code session.
- **F3 — Hybrid search.** Full-text (BM25) **and** semantic (embeddings) retrieval over the vault,
  fused into one ranked result list, exposed both as a CLI (`--json`) and to Claude.
- **F4 — Incremental indexing.** Re-index only changed files (mtime + content hash); detect deletions;
  `reindex` rebuilds from scratch.
- **F5 — Assisted filing.** `/process-inbox`: for each inbox note, Claude proposes a PARA destination +
  title + tags; on user approval, the note is moved and its frontmatter enriched.
- **F6 — Claude Code skills.** `/create-note`, `/search`, `/process-inbox` shelling out to the `vagus` CLI.
- **F7 — Obsidian compatibility.** The vault opens in Obsidian unchanged (plain `.md`, optional
  `[[wikilinks]]` and YAML frontmatter); editable on mobile via iCloud.

## Non-functional requirements

- **N1 — Local-first & private.** Works fully offline after first run; no note text leaves the machine
  by default.
- **N2 — No background daemon** in the default path; indexing is on-demand (a watcher is opt-in, later).
- **N3 — Durable & recoverable.** Markdown is the source of truth; the index is a rebuildable cache.
  iCloud holds *only* Markdown (see [ADR 0004](./adr/0004-icloud-markdown-only.md)).
- **N4 — Fast enough.** Search returns in well under a second on a personal-scale vault
  (tens of thousands of chunks); brute-force cosine is acceptable at this scale.
- **N5 — Owned in Rust.** Single CLI binary `vagus`; the author maintains the code.
- **N6 — Small surface.** ~500–800 LOC of our own glue over mature crates; no novel algorithms.

## Scope (v1)

Indexing + hybrid search + capture + assisted filing + the three skills, on one Mac (Apple Silicon).

## Non-goals

- **Not** a cloud/SaaS service; no server, no account.
- **Not** a multi-device *write* store for the index — the index is per-machine and rebuilt locally;
  only the Markdown syncs (via iCloud).
- **Not** an Obsidian replacement — Obsidian remains an optional GUI over the same files.
- **Not** a single statically-linked binary by default (the ONNX path ships a `libonnxruntime.dylib`;
  see [tradeoffs](./tradeoffs.md)).
- **No** automatic filing/moving of notes without explicit user approval.
- **No** LLM calls inside the `vagus` binary (query expansion / reranking with an LLM, if ever, lives
  in the Claude skill layer).
