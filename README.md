# vagus

[![ci](https://github.com/vasovagal/vagus/actions/workflows/ci.yml/badge.svg)](https://github.com/vasovagal/vagus/actions/workflows/ci.yml)

Search a folder of Markdown notes with keyword and semantic retrieval, or capture and file notes
from the CLI, Claude Code, or pi. Start with [installation](#install), [set up a vault](#setup), then
[use the CLI](#usage).

Vagus runs retrieval and its optional models locally, without a daemon. Models download on first
use. Agent skills give matching chunk bodies to the host agent, which may read a full note;
that agent's model and network settings apply.

## Features

- **Hybrid search:** [tantivy](https://github.com/quickwit-oss/tantivy) BM25 keyword search and
  768-dimensional EmbeddingGemma-300M vectors, combined with Reciprocal Rank Fusion (RRF).
  [Control result depth, exact search, and reranking](#choose-search-results).
- **Markdown vault:** optional frontmatter and `[[wikilinks]]`, compatible with Obsidian and the
  [PARA](https://fortelabs.com/blog/para/) folders listed in [Setup](#setup). Indexing and search
  never edit notes.
- **Capture and approved filing:** write a Markdown file, use `vagus add-note`, or invoke the
  [bundled skills](#claude-code-and-pi-skills). The filing skill asks before each move.
- **Explicit diagnostics:** inspect [semantic relevance](#set-a-relevance-floor),
  [cited-note usage](#record-presented-notes), or [retrieval evaluation](#evaluate-retrieval).
  None is a confidence score.
- **One executable:** local inference is linked into the binary; Vagus needs no Python or Node
  runtime. See [model downloads](#from-source) and [measured costs](#speed).

## Install

### Homebrew (macOS arm64, Linux arm64/amd64)

```sh
brew tap vasovagal/tap
brew trust --tap vasovagal/tap    # Homebrew 6+: trust this third-party tap
brew install vagus
```

Use `brew update && brew upgrade vagus` for upgrades. Maintainers update the shared tap manually
as part of [releasing](RELEASING.md); CI does not write it.

### From source

For development, build a clone and run it from `target/`. Do not `cargo install --path .`: a copy in
`~/.cargo/bin` can shadow the Homebrew installation. Keep development index data separate from the
installed binary's data when their chunk or embedding identities differ.

```sh
git clone https://github.com/vasovagal/vagus
cd vagus
cargo build --locked
VAGUS_DATA_DIR=/tmp/vagus-dev ./target/debug/vagus --help
```

Set `VAGUS_DATA_DIR=/tmp/vagus-dev` on development indexing and search commands too. Models can be
shared. See [build requirements and feature flags](CONTRIBUTING.md#build--test--run).

**Model downloads.** First use of embeddings downloads EmbeddingGemma-300M (about 1.2 GB).
`--rerank` fetches its approximately 150 MB model on first use; `--smart` also fetches a local
rewriter of about 1.3 GB. The macOS cache is `~/Library/Caches/vagus/models`; Linux uses its platform
cache directory, normally `~/.cache/vagus/models`. Both stay outside the vault. `VAGUS_CACHE_DIR`
overrides the path, then `FASTEMBED_CACHE_DIR` if the first variable is unset.

## Setup

```sh
vagus init --icloud      # recommended on macOS: iCloud Brain + friendly ~/brain symlink
vagus init               # alternatively, a local-only vault
vagus doctor             # network-free health/cache-presence check
vagus doctor --fetch-models  # explicit download + inference validation of both ONNX models
```

The vault uses `00-Inbox / 10-Projects / 20-Areas / 30-Resources / 40-Archive`.
`--icloud` uses `~/Library/Mobile Documents/com~apple~CloudDocs/Brain` as the real vault and
symlinks the configured vault path (normally `~/brain`) to it. Only Markdown enters iCloud; each Mac
keeps its own SQLite/Tantivy/usearch index and model cache locally. Point Obsidian at either spelling
of the same folder. Frontmatter is optional: `vim ~/brain/00-Inbox/idea.md` is sufficient.

Setup is idempotent and fail-closed. A missing vault or an exact empty PARA skeleton can be linked;
existing iCloud notes are preserved. If the local vault contains any note, symlink, special entry, or
unrecognized directory, init changes **neither** path and asks for a manual migration. The whole-directory
example below requires a real, non-symlink source directory and an absent iCloud `Brain` target. Inspect
the source first; handle symlinks or special entries manually. `vault` uses `VAGUS_VAULT` when set,
otherwise the default `~/brain`:

```sh
vault="${VAGUS_VAULT:-$HOME/brain}"
target="$HOME/Library/Mobile Documents/com~apple~CloudDocs/Brain"
if [ -d "$vault" ] && [ ! -L "$vault" ] && [ ! -e "$target" ] && [ ! -L "$target" ]; then
  mv -- "$vault" "$target" && vagus init --icloud
else
  echo "Use a real source directory and an absent Brain target; reconcile other cases manually." >&2
fi
```

For a bulk import, copy Markdown into the PARA folders (or `00-Inbox/`) and run one `vagus index`;
do not loop over `add-note`, which would load the embedder once per process.

### Claude Code and pi skills

```sh
vagus skills install                 # Claude Code (default): ~/.claude/skills
vagus skills install --agent pi      # pi: ~/.pi/agent/skills
vagus skills list --agent pi         # show pi install status
```

The skill names here describe current source. Release **v0.14.1** still bundles `create-note`,
`search`, and `process-inbox`; inspect `vagus skills list` for your installed binary. The namespaced
skills and automatic old-name migration are not in that release.

Use `vagus-create-note` to capture, `vagus-search` to retrieve, and explicitly invoke
`vagus-process-inbox` to propose filing moves. Generic note requests default to Vagus; explicit
repo files, release notes, or another notes app override that routing. A retrieval question does
not authorize capture. Search and inbox skills pass requested time windows to `--since`.

After upgrading Vagus, install again for each agent. See [installation and backup behavior](skills/README.md#install)
and [old-name migration](skills/README.md#upgrading-the-old-names). The installer honors
`CLAUDE_CONFIG_DIR` and `PI_CODING_AGENT_DIR`, leaves identical files alone, backs up hand edits to
`SKILL.md.bak`, and skips symlinks unless `--force` is used. In an existing pi session, run `/reload`.
Skills are embedded in the binary; no repository clone is needed.

## Speed

Use `--timings` with search to inspect your own costs. Recorded Apple Silicon measurements give
scale estimates, not latency guarantees:

| Operation | Recorded cost |
|---|---|
| One-shot hybrid search | About 1 s on a 4,023-chunk corpus; model loading dominates |
| Exact vector load + search | 26.6 ms for a synthetic 10,000 × 768 fixture; excludes the embedder |
| Capped 20-candidate rerank, context 0 | About 0.7 s for the rerank stage |
| Rerank, context 1 | 2.8 s median; up to 3.2 GiB process RSS |
| Rerank, context 2 | 5.7 s median; up to 5.6 GiB process RSS |

The [vector measurements](design/adr/0019-usearch-ann-backend.md#2026-07-30-corpus-and-cutoff-evidence)
and [rerank measurements](design/adr/0015-cross-encoder-rerank.md#2026-07-30-small-to-big-evidence)
use different fixtures. The rerank comparison covers five exact, capped-20 queries on 4,148 chunks.
Wider context costs more memory and can help or hurt a query; start at radius 1 if needed.
`--smart` adds local query rewriting and caches expansions by query. Its cold and warm costs depend
on the vault, hardware, and cached models.

## Usage

Start with `vagus tutorial` for capture → search → file. The command list is followed by
[result selection](#choose-search-results), [time filters](#filter-by-time-and-metadata),
[diagnostics](#set-a-relevance-floor), and [index repair](#repair-the-index). Replace angle-bracket
operands with your values; the list is a command reference, not a script to run from top to bottom.

```sh
vagus init --icloud         # one-time fail-closed iCloud/PARA setup (`vagus init`: local only)
vagus tutorial              # the capture → search → file PARA workflow
vagus index                 # incremental: sync the vault into the local index
vagus reindex               # full rebuild from the vault
vagus reindex --since 5d    # force-refresh recent filesystem mtimes; preserve older embeddings
vagus compact               # defragment the tantivy index (force-merge segments) — no re-embed
vagus search "<query>"      # hybrid search; adaptive low-signal tail cutoff by default
vagus search "<query>" --since 3m  # keep notes created in the last three months
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
vagus add-note "<title>"    # create, open $VISUAL/$EDITOR when interactive, then index (--edit/-e)
vagus add-note "Generated" --frontmatter-json '{"producer":{"model":"parakeet"}}'  # safe metadata
vagus search "parakeet"    # producer metadata is indexed lexically + semantically
vagus inbox                 # list 00-Inbox items
vagus inbox --since 10h     # list inbox notes created in the last ten hours
vagus file "<path>" --to "<folder>"  # move into PARA (--suggest [--thought-process] for ideas)
vagus doctor                # network-free health/cache-presence check
vagus doctor --fetch-models # explicitly download + validate embedder and reranker
vagus status                # index plus local tick/event counts
vagus tick '<path>'         # counter-only usage record (normally called by the skill)
vagus fame                  # most-presented existing notes
vagus ticks                 # selection-biased rank diagnostics by pipeline + corpus
vagus vectors export --out DIR  # coherent local vector/metadata snapshot for offline analysis
vagus skills install        # install agent skills (--agent claude|pi; default: claude)
```

### Choose search results

Default hybrid search combines BM25 and cosine ranks with unweighted RRF (`k=60`). Exact cosine is
automatic below 10,000 embedded chunks; usearch HNSW is used at that threshold and above when its
sidecar is available. `--exact` forces the full scan in every mode.

Results contain one best-matching chunk per note. `--limit 10` means at most 10 distinct notes;
`--chunks` returns individual chunks instead. Plain hybrid note search may omit a low-signal suffix
when a guarded RRF score knee separates it from the leading results. This never changes ranks or
scores, and never cuts before a top-three BM25/cosine source hit. `--exhaustive` disables that cutoff.

`--rerank` scores a capped candidate prefix with the local `jina-reranker-v1-turbo-en` cross-encoder.
`--rerank-context N` accepts 0–2 and requires `--rerank` or `--smart`:

- **0 (default):** the center-only, 512-token path.
- **1 or 2:** up to N adjacent content chunks per side, admitting whole neighbors only when the
  model's pair tokenizer can retain the query, special tokens, and matched center.

Context changes only rerank ordering. Returned `body`/`snippet`, retrieval, RRF, the capped prefix,
and the unscored RRF tail stay unchanged. `--smart` adds local query expansion/HyDE before reranking;
its Qwen rewriter uses candle and enables Metal on macOS when available. A build without the
`generate` feature falls back to reranking.

The [search skill](skills/vagus-search/SKILL.md) gives its host agent up to 10 exact, reranked full-body
candidates at context 0. The agent presents at most six nonredundant notes graded useful or direct
(2–3), with one retry only if none qualify. Agent judgment is distinct from local CLI retrieval.

### Filter by time and metadata

`reindex`, `search`, and `inbox` share the validated `--since` grammar: `h` hours, `d` days,
`m` 30-day months, `y` 365-day years, `s` seconds, `min` minutes, `w` weeks, or bare days.
Invalid operands fail during CLI parsing before the index opens.

- `search` and `inbox` use `created` frontmatter, falling back to filesystem mtime when absent or
  unparseable. Search filtering leaves RRF ranking unchanged.
- `reindex` selects filesystem mtimes for [forced repair](#repair-the-index).

`add-note --frontmatter-json` accepts validated producer metadata without overwriting Vagus-owned
fields. Producer JSON becomes dedicated `Frontmatter > <namespace>` chunks, searchable by BM25
and embeddings. Lifecycle fields (`created`, `status`, `source`, `para`, `modified`, `title`) stay
out of chunk text; `--since` and `--source` keep their filter behavior. Upgrading an older index to
chunk version 6 triggers a one-time full re-embed.

### Set a relevance floor

`--relevance` reports finite original-query EmbeddingGemma cosine clamped to `[0,1]`; JSON names
its `relevance_policy`. This is a model- and chunk-specific heuristic, not calibrated confidence.
RRF and reranking still set order. A BM25-only survivor has unknown relevance.

`--min-relevance 0..=1` implies reporting and filters after truncation without backfill. A positive
floor drops unknowns; zero retains them. Any relevance floor disables adaptive RRF cutoff.
BM25-only and `--smart` reject both flags because they do not retain original-query cosine.
Evaluate fixed positive and negative probes with `vagus eval --relevance` before choosing a floor.
The example `0.30` is not a universal boundary; the [recorded evaluation](design/adr/0026-bounded-semantic-relevance.md#measured-evidence)
includes a negative that scored above it.

### Record presented notes

`--tick-provenance` instruments the bundled skill; it does not change scores or ordering. It requires
note-level hybrid `--json --full --rerank --exact`, a limit of 1–1,000, and no smart mode, metadata
filter, chunks, `--relevance`, or score/relevance floor. It replaces the ordinary Hit array with `{run,hits}`.
The run pins executable, corpus, model, fusion/source depths, exact backend, reranker/cap/context,
scope, and result policy. Each hit has a path-bound event ID, source/fusion/final ranks, and either
a capped-prefix rerank rank or an explicit unscored-tail state.

The skill passes the run and only cited `{path,provenance}` pairs to `vagus tick --events`.
Run, events, and counters commit or roll back together. A time-filtered primary search uses
counter-only ticks; fallback searches are not ticked.

`vagus fame` lists the most-ticked notes. `vagus ticks` groups observations by pipeline and corpus;
these selection-biased reports are not recall or evaluation evidence. Query storage requires
explicit `vagus tick --events ... --query ... --store-query`; bodies and snippets are never stored
in these records. This restriction does not prevent the host agent from receiving search results.

### Evaluate retrieval

Run `vagus index`, then hold the index and JSONL labels fixed across A/B runs: `vagus eval` never
refreshes the index. It evaluates exhaustive note-level rankings before adaptive cutoff. P@k always
uses k as the denominator; MRR is MRR@k, and undefined cohorts are `null`. Schema-2 reports pin
rankings and label/corpus/index/backend/fusion/cohort identities; reranked reports include context
radius and tokenizer maximum in `rerank_policy`.

The fixed `eval-gate` requires a held-out graded/diverse sample, paired nDCG confidence, and
recall/MRR/P/cohort nonregressions. Passing permits opt-in experiments, not a new default.
See [labels and gate requirements](eval/README.md).

### Repair the index

Use `vagus reindex --since 10d` to repair a known recent period, including after iCloud sync between
Macs. It snapshots the whole vault, force-refreshes notes in the filesystem-mtime window even when
cached metadata matches, and persists usearch repairs. Older indexed notes and all local usage
records survive; new and deleted files are still reconciled across the whole vault.
`vagus index` also retries files with missing chunk embeddings after an interrupted run.
Use plain `vagus reindex` when the suspect period is unknown or the embedding/chunk identity changed.

Keep the index/database outside iCloud, by default in `~/.local/share/vagus/`. Markdown can rebuild
the search index, but **do not delete `meta.db` as a reset**: it also holds usage counters and
selected-result provenance that cannot be rebuilt from notes. Reindexing preserves those records.

## More

- [`CONTRIBUTING.md`](./CONTRIBUTING.md) — build, test, feature flags, releasing, conventions.
- [`CLAUDE.md`](./CLAUDE.md) and [`design/`](./design/) — the binding invariants and the ADRs
  behind every decision.
