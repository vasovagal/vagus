# vagus

[![ci](https://github.com/vasovagal/vagus/actions/workflows/ci.yml/badge.svg)](https://github.com/vasovagal/vagus/actions/workflows/ci.yml)

**Your second brain, as a single fast binary.** vagus gives you **hybrid full-text +
semantic search** over a folder of plain-Markdown notes — local-first, offline after first
run, no daemon, no cloud. Capture with one line in your editor; recall with one command (or
straight from a Claude Code or pi session).

## Features

- **Hybrid search that just works.** [tantivy](https://github.com/quickwit-oss/tantivy)
  BM25 keyword matching and local ONNX embeddings (Google **EmbeddingGemma-300M**, 768-dim)
  fused with **Reciprocal Rank Fusion** — you get exact-term *and* meaning-based hits from
  one query, no tuning.
- **Context-tidy by default.** For plain hybrid note search, `--limit` is a ceiling: when the finished
  RRF list has a guarded, statistically distinct score knee, vagus omits only the low-signal suffix
  instead of padding an LLM's context with quota-filling results. `--exhaustive` restores legacy fill.
- **Opt-in quality tiers.** Add `--rerank` for an in-core cross-encoder
  (jina-reranker-v1-turbo-en) that re-scores against full chunk bodies, or `--smart` for a
  local query-expansion/HyDE rewriter (candle + Qwen, Metal-accelerated, cached). The bundled
  search skill gives the host agent 10 exact+reranked full-body candidates, then presents only useful
  (grade ≥2), nonredundant evidence — at most 6 notes, never quota padding. Every tier runs **offline**.
- **Plain Markdown in iCloud.** [PARA](https://fortelabs.com/blog/para/) layout
  (`00-Inbox / 10-Projects / 20-Areas / 30-Resources / 40-Archive`), Obsidian-compatible,
  optional `[[wikilinks]]` and frontmatter. Your notes are the source of truth; the index is
  a throwaway cache.
- **Zero-ceremony capture.** `vim ~/brain/00-Inbox/idea.md` — no frontmatter required — or
  the create-note skill from Claude Code or pi.
- **Assisted, never automatic filing.** The process-inbox skill proposes a PARA home per note; you
  approve.
- **Claude Code and pi skills built in.** Create-note, search, and process-inbox skills ship
  inside the binary — `vagus skills install --agent <claude|pi>` writes them to the selected
  agent's global skills directory.
- **Self-contained.** One ~40 MB static binary (ONNX Runtime linked in — `otool -L` shows
  only system dylibs). No Python, no Node, no background process.

## Install

### Homebrew (macOS arm64, Linux arm64/amd64)

```sh
brew tap vasovagal/tap
brew install vagus
```

(The formula lives in the shared tap `vasovagal/homebrew-tap`. After a release, the tap is updated
manually — `VERSION=X.Y.Z scripts/render-formula.sh > .../homebrew-tap/Formula/vagus.rb` and commit it
to `vasovagal/homebrew-tap`. CI never writes the tap.)

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

### Claude Code and pi skills

```sh
vagus skills install                 # Claude Code (default): ~/.claude/skills
vagus skills install --agent pi      # pi: ~/.pi/agent/skills
vagus skills list --agent pi         # show pi install status
```

The installer honors `CLAUDE_CONFIG_DIR` and `PI_CODING_AGENT_DIR`. It is idempotent: re-running
leaves identical files alone, backs up hand-edits to `SKILL.md.bak`, and skips symlinks. After an
upgrade, install again for each agent you use; in an existing pi session, run `/reload`.

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
vagus reindex --since 10d   # force-refresh recent filesystem mtimes; preserve older embeddings
vagus compact               # defragment the tantivy index (force-merge segments) — no re-embed
vagus search "<query>"      # hybrid search; adaptive low-signal tail cutoff by default
vagus search "<query>" --exhaustive  # fill up to --limit (legacy/max-recall result set)
vagus add-note "<title>"    # create an inbox note, open $EDITOR (--edit/-e), then index
vagus inbox                 # list 00-Inbox items
vagus file <path> --to ...  # move into a PARA folder (--suggest [--thought-process] to get ideas)
vagus doctor                # health check (symlink, model cache, dylib, dims, index)
vagus status                # counts, model/dims, index size
vagus skills install        # install agent skills (--agent claude|pi; default: claude)
```

Search results are **one per note** by default, each shown as its best-matching chunk. `--limit 10`
means **at most** 10 distinct notes: plain hybrid search may return fewer when a guarded robust RRF
knee separates a high-signal prefix from a low-signal tail. The stage only drops a suffix; it never
changes ranking or scores. Pass `--exhaustive` to fill the legacy result set up to the limit, or
`--chunks` to rank individual chunks instead.

For a vault shared across Macs through iCloud, `vagus reindex --since 10d` snapshots the whole vault
and force-reindexes notes whose **filesystem mtime** is within the window, even if local cached
metadata says they are unchanged. Older indexed notes and usage ticks are preserved; new/deleted files
are still reconciled globally. Use plain `vagus reindex` when the suspect period is unknown or the
embedding/chunk identity changed. (`search --since` is different: it filters results by note creation
time.)

The index/database live **outside** iCloud (`~/.local/share/vagus/`) — only your notes live in
iCloud. The search index is fully rebuildable from the Markdown (`vagus reindex`), but the database
also holds your usage ticks (`vagus fame`), which are not — don't delete `meta.db` as a "reset".

## More

- [`CONTRIBUTING.md`](./CONTRIBUTING.md) — build, test, feature flags, releasing, conventions.
- [`CLAUDE.md`](./CLAUDE.md) and [`design/`](./design/) — the binding invariants and the ADRs
  behind every decision.
