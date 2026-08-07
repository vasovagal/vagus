# vagus

[![ci](https://github.com/vasovagal/vagus/actions/workflows/ci.yml/badge.svg)](https://github.com/vasovagal/vagus/actions/workflows/ci.yml)

**Your second brain, as a single fast binary.** vagus gives you **hybrid full-text +
semantic search** over a folder of plain-Markdown notes — local-first, offline after first
run, no daemon, no cloud. Capture with one line in your editor; recall with one command (or
straight from a Claude Code or pi session).

## Features

- **Hybrid search that just works.** [tantivy](https://github.com/quickwit-oss/tantivy)
  BM25 keyword matching and local ONNX embeddings (Google **EmbeddingGemma-300M**, 768-dim)
  fused with **Reciprocal Rank Fusion** — exact cosine below 10,000 chunks, usearch HNSW above it,
  and an all-mode `--exact` oracle. You get exact-term *and* meaning-based hits with no tuning.
- **Context-tidy by default.** For plain hybrid note search, `--limit` is a ceiling: when the finished
  RRF list has a guarded, statistically distinct score knee, vagus omits only the low-signal suffix—
  but never across a top-three BM25/cosine source hit. `--exhaustive` restores legacy fill.
- **Honest opt-in relevance.** `--relevance` reports finite original-query cosine clamped to `[0,1]`
  as a named semantic heuristic — never confidence or probability. `--min-relevance` can apply an
  explicit post-rank floor without reordering or backfill; default output and ranking stay unchanged.
- **Measure retrieval, don't guess.** `vagus eval` emits schema-2 metrics and full corpus/index/
  backend/fusion/cohort provenance; `vagus eval-gate` applies a fixed held-out paired contract before
  any same-pool alternate fusion can even land as opt-in. RRF k=60 remains the only default.
- **Private, honest usage diagnostics.** The search skill atomically increments cited-note fame and
  records strict rank provenance for its fixed pipeline. Runs pin binary/pipeline/corpus identity,
  capped tails stay explicitly unscored, query text is off by default, and reports state their
  selection bias. Ordinary search output is unchanged.
- **Opt-in quality tiers.** Add `--rerank` for an in-core cross-encoder
  (jina-reranker-v1-turbo-en) that re-scores against full chunk bodies; difficult boundary-spanning
  queries can opt into tokenizer-safe adjacent context with `--rerank-context 1|2`. Or use `--smart` for a
  local query-expansion/HyDE rewriter (candle + Qwen, Metal-accelerated, cached). The bundled
  search skill gives the host agent 10 exact+reranked full-body candidates, then presents only useful
  (grade ≥2), nonredundant evidence — at most 6 notes, never quota padding. Every tier runs **offline**.
- **Plain Markdown in iCloud.** [PARA](https://fortelabs.com/blog/para/) layout
  (`00-Inbox / 10-Projects / 20-Areas / 30-Resources / 40-Archive`), Obsidian-compatible,
  optional `[[wikilinks]]` and frontmatter. Your notes are the source of truth; the search index is
  a throwaway cache (local usage counters/provenance are the explicit exception).
- **Zero-ceremony capture.** `vim ~/brain/00-Inbox/idea.md` — no frontmatter required — or
  the create-note skill from Claude Code or pi. Generated-note integrations may safely add namespaced
  provenance with `add-note --frontmatter-json` without taking over Vagus-owned fields.
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
brew trust --tap vasovagal/tap    # Homebrew 6+: trust this third-party tap
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

## Setup

```sh
vagus init --icloud      # recommended on macOS: iCloud Brain + friendly ~/brain symlink
vagus init               # alternatively, a local-only vault
vagus doctor             # network-free health/cache-presence check
vagus doctor --fetch-models  # explicit download + inference validation of both ONNX models
```

`--icloud` uses `~/Library/Mobile Documents/com~apple~CloudDocs/Brain` as the real vault and
symlinks the configured vault path (normally `~/brain`) to it. Only Markdown enters iCloud; each Mac
keeps its own SQLite/Tantivy/usearch index and model cache locally. Point Obsidian at either spelling
of the same folder—vagus never edits a note unless you explicitly create or file it.

Setup is idempotent and fail-closed. A missing vault or an exact empty PARA skeleton can be linked;
existing iCloud notes are preserved. If the local vault contains any note, symlink, special entry, or
unrecognized directory, init changes **neither** path and asks for a manual migration. When the iCloud
`Brain` target does not yet exist, the safe whole-directory move is:

```sh
target="$HOME/Library/Mobile Documents/com~apple~CloudDocs/Brain"
[ ! -e "$target" ] && [ ! -L "$target" ]  # both must pass; otherwise merge manually
mv -- "$HOME/brain" "$target"
vagus init --icloud
```

For a bulk import, copy Markdown into the PARA folders (or `00-Inbox/`) and run one `vagus index`;
do not loop over `add-note`, which would load the embedder once per process.

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

Hybrid retrieval is hand-rolled (BM25 + exact cosine below 10,000 embedded chunks, usearch HNSW
above it, then RRF). `--exact` forces the ground-truth scan at any size and in every mode. At personal
scale it is effectively instant; a synthetic 10k×768 exact load+search fixture measures ~26.6 ms:

| Query | Latency |
|-------|---------|
| `vagus search` (default hybrid) | **~1 s** warm; a few seconds on the first query of a session (loads the embedder) |
| `--rerank` (cross-encoder, context 0) | **+~0.7 s** for the capped 20-doc stage |
| `--rerank --rerank-context 1` | **~2.8 s** rerank stage; up to ~3.2 GiB process RSS |
| `--rerank --rerank-context 2` | **~5.7 s** rerank stage; up to ~5.6 GiB process RSS |
| `--smart` (local rewrite + rerank) | a few seconds cached, ~10 s cold; scales with vault size |

*(Measured on Apple Silicon over five exact, capped-20 rerank queries in a 4,148-chunk corpus. Wider
attention is quadratic; radius 1 is the practical first try and radius 2 needs adequate memory. The
`--smart` rewrite is cached per query, so repeats are much faster — ~5 s cold / ~2.3 s warm on a small
vault.)* No daemon and no cloud round-trip on any
path.

## Usage

```sh
vagus init --icloud         # one-time fail-closed iCloud/PARA setup (`vagus init`: local only)
vagus tutorial              # the capture → search → file PARA workflow
vagus index                 # incremental: sync the vault into the local index
vagus reindex               # full rebuild from the vault
vagus reindex --since 10d   # force-refresh recent filesystem mtimes; preserve older embeddings
vagus compact               # defragment the tantivy index (force-merge segments) — no re-embed
vagus search "<query>"      # hybrid search; adaptive low-signal tail cutoff by default
vagus search "<query>" --exhaustive  # fill up to --limit (legacy/max-recall result set)
vagus search "<query>" --exact       # force ground-truth cosine (also composes with --smart)
vagus search "<query>" --rerank --rerank-context 1  # score with adjacent in-note context
vagus search "<query>" --relevance     # show bounded semantic heuristic (not probability)
vagus search "<query>" --min-relevance 0.30  # explicit post-rank floor; no backfill
vagus search "<query>" --json --full --rerank --exact --tick-provenance  # fixed skill instrumentation
vagus eval labels.jsonl --exact --relevance --json  # inspect current-corpus relevance evidence
vagus eval labels.jsonl --exact --json  # score fixed note-level pre-tidy rankings against qrels
vagus eval labels.jsonl --exact --rerank --rerank-context 1 --json  # evaluate that input policy
vagus eval-gate baseline.json candidate.json --json  # fixed ADR 0025 fusion acceptance gate
vagus add-note "<title>"    # create an inbox note, open $EDITOR (--edit/-e), then index
vagus add-note "Generated" --frontmatter-json '{"producer":{"version":"1"}}'  # safe metadata
vagus inbox                 # list 00-Inbox items
vagus file <path> --to ...  # move into a PARA folder (--suggest [--thought-process] to get ideas)
vagus doctor                # network-free health/cache-presence check
vagus doctor --fetch-models # explicitly download + validate embedder and reranker
vagus status                # index plus local tick/event counts
vagus tick '<path>'         # counter-only usage record (normally called by the skill)
vagus fame                  # most-presented existing notes
vagus ticks                 # selection-biased rank diagnostics by pipeline + corpus
vagus vectors export --out DIR  # coherent local vector/metadata snapshot for offline analysis
vagus skills install        # install agent skills (--agent claude|pi; default: claude)
```

Search results are **one per note** by default, each shown as its best-matching chunk. `--limit 10`
means **at most** 10 distinct notes: plain hybrid search may return fewer when a guarded robust RRF
knee separates a high-signal prefix from a low-signal tail. The stage only drops a suffix; it never
changes ranking or scores, and it fails open before any top-three BM25/cosine source champion. Pass
`--exhaustive` to fill the legacy result set up to the limit, or
`--chunks` to rank individual chunks instead.

`--rerank-context N` accepts 0–2 and requires `--rerank` or `--smart`. Radius 0 is the exact historical
center-only path. Radius 1/2 gives the cross-encoder up to N adjacent chunks per side, admitting whole
neighbors only when the model's actual pair tokenizer can retain the query, special tokens, and
matched center. This changes only rerank ordering: returned `body`/`snippet`, retrieval, RRF, capped
prefix, and unscored RRF tail are unchanged. Use it selectively; wider attention is expensive and can
help or hurt an individual query.

`--relevance` exposes the hit's finite original-query EmbeddingGemma cosine clamped to `[0,1]`;
JSON also names the exact `relevance_policy`. It is an opt-in semantic heuristic tied to the current
model/chunk identity, **not** calibrated confidence. RRF and reranking still determine order. A
BM25-only survivor is marked unknown rather than assigned a
fake value. `--min-relevance 0..=1` implies reporting and filters only after truncation, with no
backfill: a positive floor drops unknowns, while zero retains them. Any relevance floor disables the
adaptive RRF tidy stage. BM25-only and `--smart` reject these flags because they do not retain an
original-query cosine. Use `vagus eval --relevance` over your own fixed positive and negative probes
before adopting a floor. `0.30` was a development starting point and one untouched plain-search
negative scored `0.300044`; it is not a universal boundary, even for this vault.

`--tick-provenance` is an explicit instrumentation contract for the bundled skill, not a ranking
mode. It requires standard note-level hybrid `--json --full --rerank --exact` with no smart mode,
metadata filter, chunks, or score/relevance floor. Instead of the normal Hit array it emits
`{run,hits}`: the run pins executable, corpus, model, fusion/source depths, exact backend,
reranker/cap/context, scope, and result-policy identities; each hit has a path-bound event ID and
states source/fusion/final ranks plus either a real capped-prefix rerank rank or an explicit unscored-tail state. It does not change scores or order.
The skill copies the run and only cited `{path,provenance}` entries to `vagus tick --events`, so run,
events, and counters commit or roll back together. `vagus ticks` groups observations by pipeline and
corpus and labels them selection-biased — they are not recall/eval evidence. Query text is not stored
unless `vagus tick --events ... --query ... --store-query` is explicitly requested; bodies and
snippets are never stored.

`vagus eval` never refreshes the index: run `vagus index` first, then hold the index and JSONL labels
fixed across A/B runs. Its P@k denominator is always k, MRR is explicitly MRR@k, undefined cohorts are
`null`, and schema-2 reports pin rankings plus label/corpus/index/backend/fusion/cohort identities;
reranked reports include the context radius and tokenizer maximum in `rerank_policy`.
Evaluation is note-level exhaustive pre-tidy so a candidate cannot win by under-returning. The
non-configurable `eval-gate` requires a held-out graded/diverse sample, paired nDCG confidence, and
recall/MRR/P/cohort nonregressions; passing permits only explicit experimentation, not a new default.
See [`eval/README.md`](eval/README.md) for labels and the complete gate contract.

For a vault shared across Macs through iCloud, `vagus reindex --since 10d` snapshots the whole vault
and force-reindexes notes whose **filesystem mtime** is within the window, even if local cached
metadata says they are unchanged, including persisted usearch repairs. Older indexed notes and all
local usage/provenance rows are preserved; new/deleted files are still reconciled globally. Ordinary
`vagus index` also retries a file if an interrupted run left any chunk embedding missing. Use plain
`vagus reindex` when the suspect period is unknown or the
embedding/chunk identity changed. (`search --since` is different: it filters results by note creation
time.)

The index/database live **outside** iCloud (`~/.local/share/vagus/`) — only your notes live in
iCloud. The search index is fully rebuildable from the Markdown (`vagus reindex`), but the database
also holds your usage counters and selected-result provenance (`vagus fame`, `vagus ticks`), which
are not rebuildable — don't delete `meta.db` as a "reset".

## More

- [`CONTRIBUTING.md`](./CONTRIBUTING.md) — build, test, feature flags, releasing, conventions.
- [`CLAUDE.md`](./CLAUDE.md) and [`design/`](./design/) — the binding invariants and the ADRs
  behind every decision.
