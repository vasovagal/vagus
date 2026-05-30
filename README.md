# vagus

[![ci](https://github.com/vasovagal/vagus/actions/workflows/ci.yml/badge.svg)](https://github.com/vasovagal/vagus/actions/workflows/ci.yml)

A local-first **PARA second brain**: a small Rust CLI that gives you **hybrid full-text + semantic
search** over a folder of plain-Markdown notes, with **Claude Code skills** for capture and retrieval.

- **Vault** — plain Markdown in iCloud (`~/brain` → iCloud Drive), [PARA](https://fortelabs.com/blog/para/)
  layout (`00-Inbox / 10-Projects / 20-Areas / 30-Resources / 40-Archive`). Obsidian-compatible.
- **Search** — [tantivy](https://github.com/quickwit-oss/tantivy) BM25 + local ONNX embeddings
  (EmbeddingGemma-300M) fused with **Reciprocal Rank Fusion**, with an optional in-core cross-encoder
  reranker (`--rerank`). Fully offline after first run.
- **Capture** — `vim ~/brain/00-Inbox/idea.md` (zero ceremony, no frontmatter required) or
  `/create-note` from a Claude Code session.
- **Filing** — `/process-inbox`: Claude proposes a PARA home per inbox note; you approve.

## Design & guardrails

This project is **guardrails-first**. Before changing anything architectural, read:

- [`CLAUDE.md`](./CLAUDE.md) — the hard invariants every session must respect.
- [`design/`](./design/) — requirements, ADRs (what we considered & why), tradeoff study, prior-art survey.

## Install

### Homebrew (macOS arm64, Linux arm64/amd64)

```sh
brew tap vasovagal/vagus https://github.com/vasovagal/vagus.git
brew install vagus
```

(The formula lives in this repo at `Formula/vagus.rb`, so the tap points straight at it — no separate
`homebrew-*` repo and no token.)

### From source

```sh
cargo install --git https://github.com/vasovagal/vagus
# …or, inside a clone:
cargo install --path .
```

First **build** downloads a prebuilt ONNX Runtime (a static `libonnxruntime.a`) and links it in;
first **run** downloads the embedding model (~130 MB) to `~/Library/Caches/vagus/models`.
**Verified:** with `ort` 2.0.0-rc.12 on Apple Silicon the installed artifact is a **self-contained
binary** — `otool -L` shows only system dylibs, no `libonnxruntime.dylib` to ship (see
[`design/tradeoffs.md`](./design/tradeoffs.md)).

### Claude Code skills

The `/create-note`, `/search`, and `/process-inbox` skills are **embedded in the binary** — install
them into `~/.claude/skills/` with:

```sh
vagus skills install        # write the bundled skills (idempotent; safe to re-run)
vagus skills list           # show the bundled skills + install status
```

`brew upgrade vagus && vagus skills install` keeps them current. Re-running is safe: it leaves
identical files alone, backs up any you've hand-edited to `SKILL.md.bak`, and skips symlinks.

## Usage

```sh
vagus tutorial              # the capture → search → file PARA workflow
vagus index                 # incremental: sync the vault into the local index
vagus reindex               # full rebuild from the vault
vagus compact               # defragment the tantivy index (force-merge segments) — no re-embed
vagus search "<query>"      # hybrid search (--mode hybrid|bm25|vec, --json; auto-refreshes the index)
vagus add-note "<title>"    # create an inbox note, open $EDITOR (--edit/-e), then index
vagus inbox                 # list 00-Inbox items
vagus file <path> --to ...  # move into a PARA folder (--suggest [--thought-process] to get ideas)
vagus doctor                # health check (symlink, model cache, dylib, dims, index)
vagus status                # counts, model/dims, index size
vagus skills install        # install the Claude Code skills into ~/.claude/skills
```

The index/database live **outside** iCloud (`~/.local/share/vagus/`) and are fully rebuildable from
the Markdown — only your notes live in iCloud.
