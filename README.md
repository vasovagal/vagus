# vagus

[![ci](https://github.com/vasovagal/vagus/actions/workflows/ci.yml/badge.svg)](https://github.com/vasovagal/vagus/actions/workflows/ci.yml)

**Your second brain, as a single fast binary.** vagus gives you **hybrid full-text +
semantic search** over a folder of plain-Markdown notes — local-first, offline after first
run, no daemon, no cloud. Capture with one line in your editor; recall with one command (or
straight from a Claude Code session).

## Features

- **Hybrid search that just works.** [tantivy](https://github.com/quickwit-oss/tantivy)
  BM25 keyword matching and local ONNX embeddings (Google **EmbeddingGemma-300M**, 768-dim)
  fused with **Reciprocal Rank Fusion** — you get exact-term *and* meaning-based hits from
  one query, no tuning.
- **Opt-in quality tiers.** Add `--rerank` for an in-core cross-encoder
  (jina-reranker-v1-turbo-en) that re-scores against full chunk bodies, or `--smart` for a
  local query-expansion/HyDE rewriter (candle + Qwen, Metal-accelerated, cached). The
  `/search` Claude Code skill adds an Opus judging pass on top. Every tier runs **offline**.
- **Plain Markdown in iCloud.** [PARA](https://fortelabs.com/blog/para/) layout
  (`00-Inbox / 10-Projects / 20-Areas / 30-Resources / 40-Archive`), Obsidian-compatible,
  optional `[[wikilinks]]` and frontmatter. Your notes are the source of truth; the index is
  a throwaway cache.
- **Zero-ceremony capture.** `vim ~/brain/00-Inbox/idea.md` — no frontmatter required — or
  `/create-note` from Claude Code.
- **Assisted, never automatic filing.** `/process-inbox` proposes a PARA home per note; you
  approve.
- **Claude Code skills built in.** `/create-note`, `/search`, and `/process-inbox` ship
  inside the binary — `vagus skills install` writes them to `~/.claude/skills/`.
- **Self-contained.** One ~40 MB static binary (ONNX Runtime linked in — `otool -L` shows
  only system dylibs). No Python, no Node, no background process.

## Install

### Homebrew (macOS arm64, Linux arm64/amd64)

```sh
brew tap vasovagal/vagus https://github.com/vasovagal/vagus.git
brew install vagus
```

(The formula lives in this repo at `Formula/vagus.rb`, so the tap points straight at it — no
separate `homebrew-*` repo, no token.)

### From source

```sh
cargo install --git https://github.com/vasovagal/vagus
# …or, inside a clone:
cargo install --path .
```

**First-run footprint.** The build links a static ONNX Runtime (no dylib to ship). On first
search vagus downloads the embedding model (**EmbeddingGemma-300M, ~1.23 GB**) to
`~/Library/Caches/vagus/models`, outside iCloud. The optional tiers fetch their models lazily
the first time you use them — `--rerank` ~150 MB, `--smart` ~1.2 GB — so a plain install only
pays for the embedder.

### Claude Code skills

```sh
vagus skills install        # write the bundled skills (idempotent; safe to re-run)
vagus skills list           # show the bundled skills + install status
```

`brew upgrade vagus && vagus skills install` keeps them current; re-running leaves identical
files alone, backs up hand-edits to `SKILL.md.bak`, and skips symlinks.

## Speed

Hybrid retrieval is hand-rolled (BM25 + brute-force cosine + RRF over SQLite vectors) — at
personal scale it's effectively instant:

| Query | Latency |
|-------|---------|
| `vagus search` (default hybrid) | **~1 s** warm; a few seconds on the first query of a session (loads the embedder) |
| `--rerank` (cross-encoder) | **+~2 s** |
| `--smart` (local rewrite + rerank) | a few seconds cached, ~10 s cold; scales with vault size |

*(Measured on Apple Silicon. The `--smart` rewrite is cached per query, so repeats are much
faster — ~5 s cold / ~2.3 s warm on a small vault.)* No daemon and no cloud round-trip on any
path.

## Usage

```sh
vagus tutorial              # the capture → search → file PARA workflow
vagus index                 # incremental: sync the vault into the local index
vagus reindex               # full rebuild from the vault
vagus compact               # defragment the tantivy index (force-merge segments) — no re-embed
vagus search "<query>"      # hybrid search (--mode hybrid|bm25|vec, --rerank, --smart, --json)
vagus add-note "<title>"    # create an inbox note, open $EDITOR (--edit/-e), then index
vagus inbox                 # list 00-Inbox items
vagus file <path> --to ...  # move into a PARA folder (--suggest [--thought-process] to get ideas)
vagus doctor                # health check (symlink, model cache, dylib, dims, index)
vagus status                # counts, model/dims, index size
vagus skills install        # install the Claude Code skills into ~/.claude/skills
```

The index/database live **outside** iCloud (`~/.local/share/vagus/`) and are fully
rebuildable from the Markdown — only your notes live in iCloud.

## More

- [`CONTRIBUTING.md`](./CONTRIBUTING.md) — build, test, feature flags, releasing, conventions.
- [`CLAUDE.md`](./CLAUDE.md) and [`design/`](./design/) — the binding invariants and the ADRs
  behind every decision.
