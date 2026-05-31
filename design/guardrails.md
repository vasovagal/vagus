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
  explicit, user-approved filing step. ([ADR 0005](./adr/0005-assisted-filing.md)) A bare note must
  also stay **filterable by `search --since`**: when `created` frontmatter is absent/unparseable, the
  filter falls back to the file's **filesystem mtime**. ([ADR 0017](./adr/0017-indexed-frontmatter-filters.md))

## Index correctness

- **G4 — Pin embedding identity.** `meta` table stores `embed_model`, `embed_dims`, `tantivy_version`.
  Any mismatch ⇒ refuse incremental indexing, require `reindex`. Never mix embedding spaces. (Currently
  `google/embeddinggemma-300m` / **768** — [ADR 0006](./adr/0006-embeddings-local-no-daemon.md). Bumping
  `CHUNK_VERSION` alongside an identity change makes the one-time reindex automatic.)
- **G5 — Both stores move together.** On a changed/deleted file, delete its tantivy docs
  (`delete_term(path)` → `commit()`) **and** delete its SQLite vector rows (the vector store has no
  FK/triggers). One mtime+sha256 hash-diff drives both; `doctor` cross-checks counts.
- **G6 — tantivy update pattern.** There is no `update_document`. Per changed file: `delete_term` on
  the exact `path` term, re-`add_document` the new chunks, then a single `commit()`. Full rebuild =
  many adds + one commit.
- **G7 — Normalize vectors at insert** so cosine = dot product.
- **G20 — Chunk budget is tied to the embedder's context window.** Sections over budget are sub-split
  on paragraph boundaries (greedily packed, overlap re-prepended); fenced code blocks stay **atomic**
  (never split — an over-budget block is one chunk). The rule is fixed; the value is derived from the
  embedder (EmbeddingGemma 2048 ctx → ~900-token target, ~128 overlap; estimate `chars/3.5`, no
  tokenizer in the hot path — G11). Roll changes via `CHUNK_VERSION`.
  ([ADR 0013](./adr/0013-chunk-budget.md))

## Search behavior

- **G8 — RRF k=60.** Fuse BM25 and cosine ranks with `score = Σ 1/(k + rank)`; no score normalization,
  **no per-list weighting**. Cross-encoder **reranking is a separate post-fusion stage** (`--rerank`,
  G19) and must **not** modify `rrf()`. qmd's weighted-RRF / top-rank bonus / position-blend are
  **rejected** (they'd breach this). ([ADR 0003](./adr/0003-search-stack.md), [ADR 0015](./adr/0015-cross-encoder-rerank.md))
- **G9 — embedder prefixes.** Apply the model's prompt template, query- vs document-side, and **don't
  double-prefix** (respect what the lib already applies). EmbeddingGemma (fastembed does *not*
  auto-template it): query `task: search result | query: {text}`, document `title: none | text: {text}`
  — note documents *are* prefixed now (bge left them raw). L2-normalize after (G7).
  ([ADR 0006](./adr/0006-embeddings-local-no-daemon.md))
- **G9a — CWD-scoped exclusion.** Search elides hits whose vault path matches an "inherited"
  `.vagus/config.json` exclude word found by walking up from the CWD (code dirs only, never the
  vault); `--all` bypasses it and the `--json` Hit-array shape is unchanged.
  ([ADR 0009](./adr/0009-cwd-scoped-search.md)) The **default `--json` shape is stable**: new optional
  fields (`rerank`, `body`, `created`, `source`) are omitted unless relevant, so the skill keeps parsing it.
- **G9b — Frontmatter filters are a separate post-rank stage.** `search --since`/`--source` filter on
  per-chunk `created_at`/`source` denormalized into SQLite at index time (**no tantivy schema change**);
  the filter is a drop-only stage **around** fusion (mirrors `apply_scope`), **never** touching `rrf()`
  (G7/G8) and **never** reordering survivors. Bumping `CHUNK_VERSION` (now **4**) back-fills the columns
  via a one-time auto-reindex (G4). ([ADR 0017](./adr/0017-indexed-frontmatter-filters.md))
- **G19 — Three-tier retrieval, channel-selected.** (0) bare `vagus search` = deterministic RRF floor;
  (1) `vagus search --smart`/`--rerank`/`--rewrite` = shell + **local** models (offline, no Claude);
  (2) the `/search` skill = **Opus** expansion/HyDE/judge over the CLI. The *channel* picks the tier —
  no smartness flags beyond these, no escalation prompts. Tiers 1 and 2 reuse the same retrieval +
  rerank core and the same typed `lex:/vec:/hyde:` discipline; they differ only in *who generates*.
  ([ADR 0012](./adr/0012-three-tier-retrieval.md))

## Build & dependencies

- **G10 — fastembed cache dir is explicit.** Never rely on fastembed's `./.fastembed_cache` CWD
  default; set it to `~/Library/Caches/vagus/models` via `with_cache_dir` / `FASTEMBED_CACHE_DIR`.
- **G11 — Retrieval is hand-rolled** (tantivy BM25 + brute-force cosine + RRF;
  [ADR 0003](./adr/0003-search-stack.md)). `frankensearch`/`qmd` are **design references, not
  dependencies** ([ADR 0007](./adr/0007-lean-on-frankensearch.md)). Don't add a heavyweight
  search-engine dependency without an ADR; if you ever do, pin/vendor it.
- **G12 — Don't bump `ort` independently.** It's version-locked by fastembed (`=2.0.0-rc.12`).
- **G13 — Honest artifact (verified).** `ort` 2.0.0-rc.12 statically links `libonnxruntime.a`, so the
  installed binary is self-contained (system dylibs only; no `libonnxruntime.dylib`). If a future
  `ort`/platform ships a *shared* onnxruntime instead, the artifact becomes binary+dylib — re-verify
  with `otool -L` and update this note. `model2vec` is the onnxruntime-free fallback. The macOS Metal
  backend for the candle rewriter (ADR 0016) links only system frameworks (`Metal`, `Foundation`,
  `CoreFoundation`, `CoreML`) — re-verified self-contained via `otool -L` (still system-only).

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
  default path ([ADR 0016](./adr/0016-local-generative-rewriter.md)); **tier-2** runs in the Opus
  `/search` skill. **No cloud calls and no daemon in any tier** (G14). ([ADR 0012](./adr/0012-three-tier-retrieval.md),
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
