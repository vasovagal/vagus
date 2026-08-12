# Guardrails (binding)

The canonical list of invariants for `vagus`. The root `CLAUDE.md` mirrors this in summary; if they
ever diverge, **this file wins**. Changing a guardrail requires updating (or superseding) the relevant
[ADR](./adr/) and this file in the same change.

## Data & storage

- **G1 — iCloud holds Markdown only.** The tantivy index, the SQLite `meta.db`, and the model cache
  live **outside** iCloud (`~/.local/share/vagus/`, `~/Library/Caches/vagus/`). Never place a SQLite
  DB or search index inside the iCloud vault — async multi-file sync of `.db`/`-wal`/`-shm` corrupts
  it. Every derived-output path is checked with alias-aware resolution that retains unresolved suffixes,
  so relative/missing/`..`/symlink spellings cannot bypass the boundary. `vagus init --icloud` is the
  one explicit setup path: it preflights source/target identity, overlap, occupancy, special entries,
  and traversal errors before mutation; it never moves or recursively deletes an occupied vault, and
  removes only an exactly recognized empty PARA skeleton with non-recursive `rmdir` operations.
  ([ADR 0004](./adr/0004-icloud-markdown-only.md))
- **G2 — The index is a derived cache.** The index and derived tables (`files`/`chunks`/`meta`/
  `expansion_cache`, the tantivy dir, the usearch sidecar) must be 100% rebuildable from the Markdown
  via `vagus reindex`. Markdown files are the source of truth; the DB never is. **Sole exception:**
  `ticks`/`tick_runs`/`tick_events` are local user data, not derived caches — see G25
  ([ADR 0021](./adr/0021-usage-ticks.md)).
- **G3 — Never auto-edit the user's note.** Frontmatter is optional; a frontmatter-free note must index
  correctly (title ← first `# heading` or filename). Frontmatter is written only by an explicit capture or
  user-approved filing action: `add-note` may include validated, non-reserved producer metadata in its
  initial write, while `file` enriches Vagus-owned filing fields. Index/search never edits notes;
  indexing may derive dedicated searchable chunks from valid producer JSON without mutating the file.
  ([ADR 0005](./adr/0005-assisted-filing.md), [ADR 0027](./adr/0027-producer-frontmatter-metadata.md),
  [ADR 0028](./adr/0028-searchable-producer-metadata.md))
  A bare note must also stay **filterable by `search --since` and `inbox --since`**: when `created`
  frontmatter is absent/unparseable, both filters fall back to the file's **filesystem mtime**.
  ([ADR 0017](./adr/0017-indexed-frontmatter-filters.md))
- **G25 — Ticks and presentation provenance are local user data in meta.db.** `ticks`, `tick_runs`,
  and `tick_events` never enter the vault/frontmatter and survive `clear_all`/every reindex/file
  deletion; event paths re-key with counters on `vagus file`. Default search output cannot expose
  provenance. Explicit schema-1 provenance is restricted to one exact+reranked, full-body, note-level
  RRF path; runs pin executable/pipeline/corpus/cap/context/scope/result identity, and unscored tails
  never receive fabricated rerank ranks. Selected event paths and counters commit in one transaction
  or all roll back. Query storage is separate opt-in; bodies/snippets are never stored. Reports group
  by pipeline + corpus and must label agent-selection bias. ([ADR 0021](./adr/0021-usage-ticks.md))
  Any future non-rebuildable table must be named in `clear_all`'s keep-list comment and ADR-covered.

## Index correctness

- **G4 — Pin embedding identity.** `meta` table stores `embed_model`, `embed_dims`, `tantivy_version`.
  Any mismatch ⇒ refuse incremental indexing, require `reindex`. Never mix embedding spaces. (Currently
  `google/embeddinggemma-300m` / **768** — [ADR 0006](./adr/0006-embeddings-local-no-daemon.md). Bumping
  `CHUNK_VERSION` alongside an identity change makes the one-time reindex automatic.)
- **G5 — All stores move together.** On a changed/deleted file, delete its tantivy docs
  (`delete_term(path)` → `commit()`), its SQLite vector rows (no FK/triggers), **and** its usearch
  vectors (`remove(key_for(id))`, [ADR 0019](./adr/0019-usearch-ann-backend.md)). One mtime+sha256
  hash-diff drives all three; same `chunk_id`/`vec_key` keys; `doctor` cross-checks counts (incl.
  usearch key count == embedded chunks). The f32 BLOBs are authoritative; the `.usearch` sidecar is a
  rebuildable derived cache (G2) — a missing/mismatched sidecar rebuilds from the BLOBs, no re-embed.
  A file with NULL chunk embeddings bypasses mtime/hash shortcuts and retries the full replacement;
  forced-refresh mutations must count when deciding to save usearch. The G25 user-data tables are
  intentionally **outside** this three-store hash-diff; `doctor`
  cross-checks orphaned counter and event paths informationally.
- **G6 — tantivy update pattern.** There is no `update_document`. Per changed file: `delete_term` on
  the exact `path` term, re-`add_document` the new chunks, then a single `commit()`. Full rebuild =
  many adds + one commit.
- **G7 — Normalize vectors at insert** so cosine = dot product.
- **G20 — Chunk budget is tied to the embedder's context window.** Sections over budget are sub-split
  on paragraph boundaries (greedily packed, overlap re-prepended); fenced code blocks stay **atomic**
  (never split — an over-budget block is one chunk). The rule is fixed; the value is derived from the
  embedder (EmbeddingGemma 2048 ctx → ~900-token target, ~128 overlap; estimate `chars/3.5`, no
  tokenizer in the hot path — G11). Valid producer JSON uses the same budget in a separate chunk
  kind; even whitespace-free values are split, and metadata cannot occupy body rerank-context slots.
  Roll changes via `CHUNK_VERSION`.
  ([ADR 0013](./adr/0013-chunk-budget.md), [ADR 0028](./adr/0028-searchable-producer-metadata.md))
- **G26 — Windowed reindex is a forced incremental repair, never a partial index.** `vagus reindex
  --since <duration>` first snapshots every Markdown path + **filesystem mtime** in the vault, then
  force-refreshes selected existing notes through all three G5 stores even when mtime/hash metadata
  agrees, and its in-memory usearch mutations are persisted even if every mutation is classified
  `refreshed`. Older notes keep normal incremental behavior; new files and deletions are reconciled
  across the whole snapshot. It never clears older rows or G25 user data. Plain `reindex` remains the full rebuild;
  an incompatible G4 identity still requires a full rebuild (a chunk-version auto-reindex may upgrade
  the windowed run; a direct embedding mismatch refuses it).
  ([ADR 0022](./adr/0022-mtime-windowed-reindex.md))

## Search behavior

- **G8 — RRF k=60 is the production floor; experiments are evidence-gated.** Bare hybrid search uses
  unweighted `score = Σ 1/(60 + rank)` over BM25/cosine; equal sums use ascending opaque `chunk_id`.
  Never blend raw BM25 + cosine or fit normalization to live result scores. An alternate may reorder
  only the same candidate union and may land only as explicit opt-in after ADR 0025's fixed held-out
  `eval-gate` passes; passing does **not** authorize default replacement. Cross-encoder reranking stays
  separate. Default promotion and any G9d reuse require a new ADR plus context/latency evidence.
  Small-to-big rerank context is an input-only opt-in (`--rerank-context 0..=2`, default 0): actual
  pair-tokenizer budgeting must reserve query/special tokens and keep the matched center; it cannot
  change retrieval, the capped rerank prefix, or the unscored RRF tail. Radius 0 preserves the legacy
  512-token center-only path exactly. ([ADR 0003](./adr/0003-search-stack.md),
  [ADR 0015](./adr/0015-cross-encoder-rerank.md),
  [ADR 0025](./adr/0025-evidence-gated-fusion.md))
- **G9 — embedder prefixes.** Apply the model's prompt template, query- vs document-side, and **don't
  double-prefix** (respect what the lib already applies). EmbeddingGemma (fastembed does *not*
  auto-template it): query `task: search result | query: {text}`, document `title: none | text: {text}`
  — note documents *are* prefixed now (bge left them raw). L2-normalize after (G7).
  ([ADR 0006](./adr/0006-embeddings-local-no-daemon.md))
- **G9a — CWD-scoped exclusion.** Search elides hits whose vault path matches an "inherited"
  `.vagus/config.json` exclude word found by walking up from the CWD (code dirs only, never the
  vault); `--all` bypasses it and the `--json` Hit-array shape is unchanged.
  ([ADR 0009](./adr/0009-cwd-scoped-search.md)) The **default `--json` shape is stable**: new optional
  fields (`rerank`, `body`, `created`, `source`, `siblings`, `relevance`) are omitted unless relevant,
  while G25 rank bookkeeping is always skipped, so existing consumers keep parsing it.
- **G9b — Frontmatter filters are a separate post-rank stage.** `search --since`/`--source` filter on
  per-chunk `created_at`/`source` denormalized into SQLite at index time (**no tantivy schema change**);
  the filter is a drop-only stage **around** fusion (mirrors `apply_scope`), **never** touching `rrf()`
  (G7/G8) and **never** reordering survivors. The current `CHUNK_VERSION` (**6**) includes these
  columns and back-fills them via a one-time auto-reindex (G4).
  ([ADR 0017](./adr/0017-indexed-frontmatter-filters.md))
- **G9c — Note-level results are a separate post-rank dedup stage.** By default `--limit` counts
  **distinct notes**: `dedupe_notes` keeps each note's best-ranked chunk (folding later chunks into its
  `siblings` count and privately retaining their best source ranks for G9d) — a drop-only,
  order-preserving stage run **after** rerank and the G9b filters,
  **before** truncation, **never** touching `rrf()` (G7/G8). `--chunks` skips the stage (raw chunk
  hits, pre-0.7 output byte-identical); filing `--suggest` stays chunk-level.
  ([ADR 0020](./adr/0020-note-level-results.md))
- **G9d — Plain hybrid note `--limit` is an adaptive context ceiling.** After all existing rank/filter/
  dedup/scope stages and legacy truncation, a guarded robust knee over the unchanged positive RRF
  scores may drop only a statistically distinct low-signal **suffix**. It never reorders/backfills,
  mutates scores, normalizes components, or touches `rrf()` (G8); a proposed knee before any note with
  a top-three BM25/cosine source chunk is vetoed, including a folded sibling champion.
  Malformed/short/smooth/champion-crossing lists and BM25/vec/rerank/smart/chunk/explicit-`--min-score`
  modes fail open. The rule consumes only standard `rrf_k60` scores; an alternate fusion must bypass
  it and return the full prefix unless a later ADR validates compatible semantics. `--exhaustive`
  restores legacy fill-up-to-`--limit` results. This stage adds no JSON fields; absent explicit G9e
  reporting, JSON remains a pure unchanged-shape Hit array.
  ([ADR 0023](./adr/0023-adaptive-context-tidy-results.md))
- **G9e — Semantic relevance is opt-in bounded cosine, never confidence.** The v1 policy is finite
  original-query EmbeddingGemma cosine clamped to `[0,1]`, named with its model/chunk identity and
  described as a heuristic — not a probability or cross-vault calibration. Reporting and a finite
  `--min-relevance 0..=1` floor never alter retrieval, RRF k=60, source ranks, ordering,
  rerank-prefix eligibility, or default human/JSON output. Filtering is post-truncation with no
  backfill and disables G9d; a positive floor drops unknown/BM25-only hits, while zero retains them.
  BM25-only and `--smart` reject relevance because they lack retained original-query cosine.
  Reranking carries the original cosine unchanged and never substitutes its sigmoid/logit; eval may
  report the named policy as a schema-2 score diagnostic without changing ranking metrics.
  ([ADR 0026](./adr/0026-bounded-semantic-relevance.md))
- **G9f — Presentation provenance is explicit, strict, and non-ranking.** Only G25's fixed
  `--tick-provenance` path may wrap JSON Hits with self-verifying run identity and real source/fusion/
  rerank/final ranks. It cannot alter retrieval, scores, ordering, caps, filtering, or default output.
  Capped-tail hits are explicitly unscored; path-bound event IDs and all ranks are validated before
  atomically writing the run, cited-note events, and counters. Diagnostics group by pipeline + corpus and are
  selection-biased, never eval evidence. ([ADR 0021](./adr/0021-usage-ticks.md))
- **G9g — Producer metadata is searchable, lifecycle frontmatter is not.** A complete leading
  frontmatter block contributes search text only for safe non-Vagus top-level keys whose one-line
  values parse as JSON. Each field becomes one or more budgeted `ProducerMetadata` chunks appended
  after content and sent through SQLite/Tantivy/embeddings/usearch/reranking. `chunks.kind` prevents
  metadata from displacing body neighbors under `--rerank-context`; note dedup remains unchanged.
  Vagus-owned `created`/`status`/`source`/`para`/`modified`/`title` never become chunk text (ADR 0017's
  exact filters still apply). Roll extraction changes via `CHUNK_VERSION`.
  ([ADR 0028](./adr/0028-searchable-producer-metadata.md))
- **G19 — Three-tier retrieval, channel-selected.** (0) bare `vagus search` = deterministic RRF floor;
  (1) `vagus search --smart`/`--rerank`/`--rewrite` = shell + **local** models (offline, no agent);
  (2) the bundled search skill (`/search` in Claude Code, `/skill:search` in pi) = **Opus** over the
  same core, with a bounded contract: 10 exact+reranked full-body candidates, present only grade ≥2,
  max 6 nonredundant notes, never pad, and at most one modality-selected retry if none survive. The
  fixed **unfiltered** primary path emits G9f provenance and atomically records only cited notes
  without query content. Explicit user time windows go into retrieval as `--since`, stay on retry,
  omit the G9f wrapper because metadata-filtered provenance is forbidden, and record primary cited
  paths counter-only; retries remain unticked. The *channel* picks the tier — no escalation prompts or
  routine tier-2 fan-out. The skill keeps rerank-context radius 0; optional wider model input never
  expands the ten matched bodies shown to the agent.
  ([ADR 0012](./adr/0012-three-tier-retrieval.md))
- **G27 — Evaluation evidence is reproducible and cannot reward under-returning.** `vagus eval` uses
  fixed-denominator P@k, explicitly truncated MRR@k, and `null` undefined cohorts. Schema 2 pins
  labels/corpus/index/backend/config/fusion identity, cohorts, and complete rankings; runs are
  note-level exhaustive pre-tidy with no implicit refresh/scope/filter/floor. Raw scores and explicit
  G9e relevance are diagnostics, never probabilities. Reranked reports encode the context radius +
  tokenizer limit in `rerank_policy`, so unlike inputs
  cannot be compared as one configuration. Fusion claims use only ADR 0025's non-configurable paired
  gate; metric/gate changes require an ADR
  and schema/contract-version update. ([ADR 0024](./adr/0024-retrieval-eval-harness.md),
  [ADR 0025](./adr/0025-evidence-gated-fusion.md))

## Build & dependencies

- **G10 — fastembed cache dir and download consent are explicit.** Never rely on fastembed's
  `./.fastembed_cache` CWD default; set it to `~/Library/Caches/vagus/models` via `with_cache_dir` /
  `FASTEMBED_CACHE_DIR`. Plain `vagus doctor` is strictly network-incapable: it inspects the exact
  required files in local Hugging Face snapshots and never calls a download-capable model constructor,
  even for a partial cache. Only `doctor --fetch-models` may construct/download both ONNX models; it
  validates each with inference and exits nonzero if either fails. ([ADR 0006](./adr/0006-embeddings-local-no-daemon.md),
  [ADR 0015](./adr/0015-cross-encoder-rerank.md))
- **G11 — Retrieval fusion is hand-rolled** (tantivy BM25 + RRF k=60; [ADR 0003](./adr/0003-search-stack.md)).
  The cosine component uses automatic exact brute force below 10,000 embedded chunks and the embedded,
  statically linked **usearch HNSW** index at/above that cutoff
  ([ADR 0019](./adr/0019-usearch-ann-backend.md)); explicit `--exact` forces the oracle in **every**
  mode, including `--smart`. The *fusion* (`rrf()`) and rerank stages remain untouched (G7/G8).
  `frankensearch`/`qmd` remain **design references, not dependencies**
  ([ADR 0007](./adr/0007-lean-on-frankensearch.md)). Don't add another heavyweight search-engine
  dependency without an ADR; if you do, pin/vendor it (usearch is pinned `=2.25.3`, `Cargo.lock` committed).
- **G12 — Don't bump `ort` independently.** It's version-locked by fastembed (`=2.0.0-rc.12`).
- **G13 — Honest artifact (verified).** `ort` 2.0.0-rc.12 statically links `libonnxruntime.a`, so the
  installed binary is self-contained (system dylibs only; no `libonnxruntime.dylib`). If a future
  `ort`/platform ships a *shared* onnxruntime instead, the artifact becomes binary+dylib — re-verify
  with `otool -L` and update this note. `model2vec` is the onnxruntime-free fallback. The macOS Metal
  backend for the candle rewriter (ADR 0016) links only system frameworks (`Metal`, `Foundation`,
  `CoreFoundation`, `CoreML`) — re-verified self-contained via `otool -L` (still system-only). The
  **usearch** vector index ([ADR 0019](./adr/0019-usearch-ann-backend.md)) compiles its C++ via
  `cxx-build` and static-links its `numkong` SIMD lib (`rustc-link-lib=static=numkong`); with `openmp`
  OFF the binary links only OS frameworks + `libc++` — re-verified `otool`-clean (no `libomp`,
  no shared onnxruntime). Re-verify with `otool -L`/`ldd` on any usearch/platform bump.

## Product

- **G14 — Local-first / offline by default.** No cloud calls, no background daemon in the default path.
- **G15 — PARA layout fixed.** `00-Inbox / 10-Projects / 20-Areas / 30-Resources / 40-Archive`.
  Filing is assisted + user-approved, **never automatic**.
- **G16 — Obsidian-compatible.** Plain `.md`, optional `[[wikilinks]]`/frontmatter; don't break it.
- **G17 — Generation is tiered; the core has no *generative* default.** (Supersedes the original
  "no LLM in the binary.") Deterministic **scoring** models — the embedder and the cross-encoder
  reranker — ride the in-binary `ort` stack and are fine in core (they are not generative). **Generative**
  rewriting/HyDE is tiered: **tier-0** has none; **tier-1** may compile a local generative model into
  `vagus` but only **feature-gated + lazily-downloaded + opt-in** (`--smart`/`--rewrite`), never in the
  default path ([ADR 0016](./adr/0016-local-generative-rewriter.md)); **tier-2** uses the host agent
  primarily for bounded full-body judgment and permits one query reformulation only after zero useful
  hits (Claude Code or pi). **No cloud calls and no daemon in any tier** (G14).
  ([ADR 0012](./adr/0012-three-tier-retrieval.md),
  [ADR 0015](./adr/0015-cross-encoder-rerank.md))
- **G18 — Networked features ship as plugins, not in core.** Anything that makes cloud/network calls
  or pulls third-party dependencies (Slack, GitHub, etc.) is an external `vagus-<name>` plugin
  dispatched off `$PATH`, speaking the NDJSON contract — never compiled into the `vagus` binary. This
  is what *keeps* G14 true as integrations grow. ([ADR 0010](./adr/0010-plugin-subcommands.md),
  [ADR 0011](./adr/0011-plugin-protocol.md), `docs/plugin-contract.md`) Plugins are for **networked
  capture**, *not* search-time transforms: the reranker/rewriter live in core (G17), because the NDJSON
  contract is one-way note→index and they are neither networked nor a foreign runtime.

## Concurrency & agents

- **G21 — Worktree isolation for parallel work.** Multiple agents never share one checkout.
  Swarm/parallel tasks run in their own git worktree (`.claude/worktrees/<name>` in-repo, or org-level
  `~/code/vasovagal/.vagus-worktrees/`), branched **fresh from `origin/main`** (`worktree.baseRef =
  "fresh"`). Convention, reinforced by the `Agent`/`Workflow` `isolation: 'worktree'` option — **not** a
  blocking lock. ([ADR 0018](./adr/0018-multi-agent-guardrails.md))
- **G22 — No direct commits to `main`, except releases.** Code/doc changes land via a feature branch +
  PR (matches the CI laws / `RELEASING.md`: a tag trusts the green `main` it was cut from). **Releases
  are exempt** and may land directly on `main`: a version bump or the CI formula bump — a commit staging
  only `Cargo.toml`/`Cargo.lock`/`CHANGELOG.md`/`Formula/` — plus **`vX.Y.Z` tag pushes**. A `git-guard`
  `PreToolUse` hook (`scripts/git-guard.sh`) denies non-release commits on `main` and pushes of the
  `main` branch, while allowing release-only commits and tag pushes; it **fails open** so a missing
  `jq`/non-git cwd never blocks work. ([ADR 0018](./adr/0018-multi-agent-guardrails.md))
- **G23 — Worktree hygiene.** Remove a worktree once its branch merges. `scripts/worktree-janitor.sh`
  lists worktrees whose branch is merged into `origin/main` (a `SessionStart` notice surfaces them) and
  `--prune` removes the clean ones, refusing any dirty worktree.
  ([ADR 0018](./adr/0018-multi-agent-guardrails.md))
- **G24 — Leave breadcrumbs.** Every architectural decision updates the matching ADR and moves the
  README ADR index, `guardrails.md`, and `CLAUDE.md` in the **same change**. Nudged softly (a commit-time
  reminder when `src/**` changes without a `design/**` or `CHANGELOG.md` change staged, plus the PR
  template checklist), **not** gated. ([ADR 0018](./adr/0018-multi-agent-guardrails.md))
