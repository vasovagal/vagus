# CLAUDE.md — vagus

`vagus` is a local-first **PARA second brain**: a Rust CLI providing hybrid full-text + semantic
search over a plain-Markdown vault in iCloud, plus Claude Code skills for capture and retrieval.

**Before any architectural change, read [`design/`](./design/).** It holds the requirements, the ADRs
(what we considered and why), the tradeoff study, and the prior-art survey. When you change a decision,
**update the matching ADR** in `design/adr/`. [`design/guardrails.md`](./design/guardrails.md) is the
canonical invariant list and is **binding** — the summary below must stay in sync with it.

## Hard invariants (do not violate without an ADR change)

1. **Only plain Markdown goes in the iCloud vault.** The search index (`tantivy/`), the SQLite
   `meta.db`, and the model cache live **outside** iCloud — `~/.local/share/vagus/` and
   `~/Library/Caches/vagus/`. Never write a database or index into the iCloud vault: iCloud syncs
   `.db`/`-wal`/`-shm` independently and will corrupt it (`database disk image is malformed`).
2. **The index is a derived cache, never the source of truth.** It must be fully rebuildable from the
   Markdown via `vagus reindex`. The Markdown files are authoritative.
3. **Never auto-edit a note the user is writing.** Frontmatter is *optional*; a bare `vim
   ~/brain/00-Inbox/x.md` with no frontmatter must index fine (title falls back to first `# heading`
   or filename). Frontmatter is only added/enriched during an explicit, approved filing step.
4. **Pin the embedding identity.** Store `embed_model` + `embed_dims` + `tantivy_version` in the
   `meta` table. On any mismatch, refuse incremental indexing and require `vagus reindex`. Never mix
   vectors from different models/dims — it silently corrupts ranking. Current identity:
   `google/embeddinggemma-300m` / **768** (768-dim, 2048-ctx). Bump `CHUNK_VERSION` alongside any
   identity change so the one-time reindex is automatic.
5. **Keep the two stores consistent off one hash-diff.** On a changed/deleted file: delete its tantivy
   docs (`writer.delete_term(path)` → `commit()`) **and** delete its SQLite vector rows (the vector
   store has no foreign keys/triggers). Same `chunk_id`/`path` keys drive both.
6. **Set the fastembed cache dir explicitly.** fastembed defaults to `./.fastembed_cache` in the CWD —
   always override to `~/Library/Caches/vagus/models` (`with_cache_dir(...)` or `FASTEMBED_CACHE_DIR`).
7. **Hybrid search = RRF (k=60).** Fuse BM25 ranks and cosine ranks with `score = Σ 1/(k + rank)`; no
   weighting/normalization. The cross-encoder reranker (`--rerank`) is a **separate post-fusion stage**
   and must not touch `rrf()`. Apply the embedder's prompt template (EmbeddingGemma: query
   `task: search result | query:`, document `title: none | text:` — documents *are* prefixed now) and
   **don't double-prefix**.
8. **Retrieval is hand-rolled** (tantivy BM25 + brute-force cosine + RRF; see
   `design/adr/0003-search-stack.md`). `frankensearch`/`qmd` are design references, **not
   dependencies** (see `design/adr/0007-lean-on-frankensearch.md`). Don't add a heavyweight
   search-engine dependency without an ADR.
9. **Local-first, offline by default.** No cloud calls and no background daemon in **any** tier.
   Generation is *tiered*, not banned (see invariant 12): the reranker is a scoring model in core;
   generative rewriting/HyDE is opt-in local (tier-1, feature-gated) or Opus in the skill (tier-2).
10. **PARA layout is fixed** (`00-Inbox / 10-Projects / 20-Areas / 30-Resources / 40-Archive`).
    Filing inbox → PARA is **assisted and user-approved, never automatic.**
11. **Stay Obsidian-compatible** (plain `.md`, optional `[[wikilinks]]`/frontmatter). Artifact note
    (verified): `ort` statically links onnxruntime, so the installed binary is self-contained (system
    dylibs only). Re-verify with `otool -L` if `ort`/platform changes; `model2vec` is the
    onnxruntime-free fallback.
12. **Three tiers, "no versioned runtime" identity.** vagus is a self-contained Rust *universe* (no
    Python/Node/TS; static C++ inference libs are in-character — ADR 0014). Retrieval is three-tier,
    channel-selected (ADR 0012): (0) bare `vagus search` = RRF floor; (1) `--smart`/`--rerank`/`--rewrite`
    = shell + local models, offline; (2) the `/search` skill = Opus. Advanced search is **in core**,
    **not** a plugin — plugins (G18) are for networked capture only.
13. **Chunk budget ↔ embedder context window** (ADR 0013/G20). Sub-split sections over ~900 tokens
    (`chars/3.5`, ~128 overlap); **fenced code stays atomic** (never split). Re-derive the budget if the
    embedder changes; roll via `CHUNK_VERSION`.
14. **Multi-agent isolation** (ADR 0018/G21–G23). Parallel/swarm work runs in its own git worktree
    (`.claude/worktrees/<name>` or org-level `.vagus-worktrees/`, branched fresh from `origin/main`) —
    never dueling agents in one checkout. **No direct commits to `main`** (feature branch + PR; a
    `git-guard` hook enforces it). Prune a worktree once its branch merges (`scripts/worktree-janitor.sh`).
15. **Leave breadcrumbs** (ADR 0018/G24). Architectural changes update the matching ADR and keep the
    `design/README.md` ADR index, `design/guardrails.md`, and this file **in sync, same change**.

## Layout

```
~/code/vasovagal/vagus/     # this repo (org dir ~/code/vasovagal/)
  src/                      # the vagus crate
  design/                   # requirements, ADRs, tradeoffs, prior-art, guardrails  <- READ FIRST
~/brain -> ~/Library/Mobile Documents/com~apple~CloudDocs/Brain   # the vault (markdown only, in iCloud)
~/.local/share/vagus/       # index: tantivy/ + meta.db + config.toml   (OUTSIDE iCloud)
~/Library/Caches/vagus/models/   # cached ONNX models: embedder + optional reranker  (OUTSIDE iCloud)
~/.claude/skills/{create-note,search,process-inbox}/   # the three skills (shell out to `vagus`)
```

## Build / test / run

```sh
cargo build              # first build fetches prebuilt ONNX Runtime (network, one-time)
cargo test
cargo clippy --all-targets
cargo install --path .   # installs `vagus` on PATH
vagus doctor             # verify symlink, model cache, dylib, dims, index health
vagus status
```

## Releasing

Push a `vX.Y.Z` tag; see [`RELEASING.md`](./RELEASING.md). The CI/release pipeline follows the laws in
`xrl/agents` `LAWS.md`: split-by-event (`ci.yml` on PR/main, `release.yml` on tags — no test re-run),
native-per-arch matrix (no emulation), centralized pinned-SHA caching, re-run-safe release.

## Conventions

- Match the surrounding Rust style; keep modules small and single-purpose (`index`, `chunk`, `embed`,
  `search`, `notes`, `db`, `config`, `cli`).
- All data-producing commands support a stable `--json` shape so the skills parse rather than scrape.
- Commit `Cargo.lock` (this is a binary crate).
- **Run `cargo fmt` before pushing** — never burn a CI cycle on formatting (`ci.yml` runs
  `cargo fmt --check`). Run it and move on: **don't** read the reformatted output back into context —
  it's almost always fine. Only inspect formatting if something downstream actually breaks.
- **Meaningful work goes in `CHANGELOG.md`.** User-noticeable changes get an entry under `## [Unreleased]`
  in the same change (Keep a Changelog: Added/Changed/Fixed/Removed). Internal refactors / test-only
  changes don't need one.
- Personal repo under the **`vasovagal`** GitHub org — **not** `scientist-hq` (that's work).
