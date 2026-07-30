# CLAUDE.md — vagus

`vagus` is a local-first **PARA second brain**: a Rust CLI providing hybrid full-text + semantic
search over a plain-Markdown vault in iCloud, plus Agent Skills for Claude Code and pi.

**Before any architectural change, read [`design/`](./design/).** It holds the requirements, the ADRs
(what we considered and why), the tradeoff study, and the prior-art survey. When you change a decision,
**update the matching ADR** in `design/adr/`. [`design/guardrails.md`](./design/guardrails.md) is the
canonical invariant list and is **binding** — the summary below must stay in sync with it.

## Hard invariants (do not violate without an ADR change)

1. **Only plain Markdown goes in the iCloud vault.** The search index (`tantivy/`), the SQLite
   `meta.db`, and the model cache live **outside** iCloud — `~/.local/share/vagus/` and
   `~/Library/Caches/vagus/`. Enforce this with alias-aware, missing-path-safe resolution before any
   derived write. `vagus init --icloud` may create only the PARA skeleton/symlink after a complete
   fail-closed preflight; it never moves/deletes occupied notes. Never write a database or index into
   the iCloud vault: independently synced `.db`/`-wal`/`-shm` files corrupt it.
2. **The index is a derived cache, never the source of truth.** It must be fully rebuildable from the
   Markdown via `vagus reindex`. The Markdown files are authoritative. Sole exception: the `ticks`
   usage counters (ADR 0021/G25) are local user data in `meta.db` — reindex preserves them and they
   never enter the vault.
3. **Never auto-edit a note the user is writing.** Frontmatter is *optional*; a bare `vim
   ~/brain/00-Inbox/x.md` with no frontmatter must index fine (title falls back to first `# heading`
   or filename). Frontmatter is only added/enriched during an explicit, approved filing step.
4. **Pin the embedding identity.** Store `embed_model` + `embed_dims` + `tantivy_version` in the
   `meta` table. On any mismatch, refuse incremental indexing and require `vagus reindex`. Never mix
   vectors from different models/dims — it silently corrupts ranking. Current identity:
   `google/embeddinggemma-300m` / **768** (768-dim, 2048-ctx). Bump `CHUNK_VERSION` alongside any
   identity change so the one-time reindex is automatic.
5. **Keep all three stores consistent off one hash-diff.** On a changed/deleted file: delete its tantivy
   docs (`writer.delete_term(path)` → `commit()`), its SQLite vector rows (no FK/triggers), **and** its
   usearch vectors (`remove(key_for(id))` — ADR 0019). Same `chunk_id`/`vec_key` keys drive all three.
   The f32 BLOBs are authoritative; the `.usearch` sidecar is a rebuildable derived cache (missing/stale
   ⇒ rebuilt from the BLOBs, no re-embed).
6. **Set the fastembed cache dir explicitly.** fastembed defaults to `./.fastembed_cache` in the CWD —
   always override to `~/Library/Caches/vagus/models` (`with_cache_dir(...)` or
   `FASTEMBED_CACHE_DIR`). Plain `vagus doctor` is filesystem-presence-only and must never instantiate
   a possibly partial model cache; only explicit `doctor --fetch-models` may download and validate both
   ONNX models, failing nonzero if either fails.
7. **Hybrid search = RRF (k=60).** Bare hybrid search uses unweighted
   `score = Σ 1/(60 + rank)`; equal sums use stable `chunk_id`. Never blend raw BM25+cosine or fit
   transforms to live scores. Same-pool alternate fusion may land only as explicit opt-in after the
   fixed ADR 0025 `eval-gate`; passing does not authorize a new default or G9d score semantics. The
   cross-encoder reranker (`--rerank`) is a **separate post-fusion stage**
   and must not touch `rrf()`. `--rerank-context` is input-only, bounded 0–2, and defaults to the exact
   legacy 512-token center-only path; widened input must use actual pair-tokenizer budgeting, retain
   the matched center, and preserve the capped prefix plus unscored RRF tail — as is note-level dedup,
   the default where `--limit` counts distinct
   notes (`--chunks` opts out; ADR 0020/G9c). Apply the embedder's prompt template (EmbeddingGemma:
   query `task: search result | query:`, document `title: none | text:` — documents *are* prefixed
   now) and **don't double-prefix**.
8. **Retrieval fusion is hand-rolled** (tantivy BM25 + RRF; see `design/adr/0003-search-stack.md`). The
   cosine component uses exact brute force automatically below 10,000 embedded chunks and the embedded,
   statically linked **usearch HNSW** index above that; `--exact` forces the oracle in every mode—see
   `design/adr/0019-usearch-ann-backend.md`. `rrf()` and
   rerank are untouched by the backend (G7/G8). `frankensearch`/`qmd` are design references, **not
   dependencies** (see `design/adr/0007-lean-on-frankensearch.md`). Don't add another heavyweight
   search-engine dependency without an ADR.
9. **Local-first, offline by default.** No cloud calls and no background daemon in **any** tier.
   Generation is *tiered*, not banned (see invariant 12): the reranker is a scoring model in core;
   generative rewriting/HyDE is opt-in local (tier-1, feature-gated); tier-2 uses its host agent as a
   bounded body judge, with one reformulation retry only when the first pass has no useful evidence.
10. **PARA layout is fixed** (`00-Inbox / 10-Projects / 20-Areas / 30-Resources / 40-Archive`).
    Filing inbox → PARA is **assisted and user-approved, never automatic.**
11. **Stay Obsidian-compatible** (plain `.md`, optional `[[wikilinks]]`/frontmatter). Artifact note
    (verified): `ort` statically links onnxruntime, so the installed binary is self-contained (system
    dylibs only). Re-verify with `otool -L` if `ort`/platform changes; `model2vec` is the
    onnxruntime-free fallback. macOS enables candle's Metal backend for the rewriter (ADR 0016) —
    links only system frameworks (`Metal`/`Foundation`/`CoreFoundation`), still `otool`-clean.
12. **Three tiers, "no versioned runtime" identity.** vagus is a self-contained Rust *universe* (no
    Python/Node/TS; static C++ inference libs are in-character — ADR 0014). Retrieval is three-tier,
    channel-selected (ADR 0012): (0) bare `vagus search` = RRF floor; (1) `--smart`/`--rerank`/`--rewrite`
    = shell + local models, offline; (2) the bundled search skill (`/search` in Claude Code,
    `/skill:search` in pi) = Opus over 10 exact+reranked bodies at rerank-context radius 0, grade≥2
    only, max 6 presented, one fallback only if none survive. Advanced search is **in core**,
    **not** a plugin — plugins (G18) are for networked capture only.
13. **Chunk budget ↔ embedder context window** (ADR 0013/G20). Sub-split sections over ~900 tokens
    (`chars/3.5`, ~128 overlap); **fenced code stays atomic** (never split). Re-derive the budget if the
    embedder changes; roll via `CHUNK_VERSION`.
14. **Multi-agent isolation** (ADR 0018/G21–G23). Parallel/swarm work runs in its own git worktree
    (`.claude/worktrees/<name>` or org-level `.vagus-worktrees/`, branched fresh from `origin/main`) —
    never dueling agents in one checkout. **No direct commits to `main`** except releases — a version
    bump (Cargo.toml/Cargo.lock/CHANGELOG.md) and `vX.Y.Z` tag pushes are allowed; everything else goes
    via feature branch + PR (a `git-guard` hook enforces it). Prune a worktree once its branch merges
    (`scripts/worktree-janitor.sh`).
15. **Leave breadcrumbs** (ADR 0018/G24). Architectural changes update the matching ADR and keep the
    `design/README.md` ADR index, `design/guardrails.md`, and this file **in sync, same change**.
16. **Windowed reindex is forced incremental repair, not a partial index** (ADR 0022/G26). `vagus
    reindex --since <duration>` snapshots every Markdown path + filesystem mtime first, force-refreshes
    selected existing notes across SQLite/Tantivy/usearch even when cached metadata agrees, and still
    reconciles all new/deleted files. It preserves older indexed notes and ticks; plain `reindex` is
    the full rebuild, and a G4 identity mismatch still requires one (chunk-version auto-reindex may
    upgrade the windowed run; a direct embedding mismatch refuses it).
17. **Plain hybrid note search treats `--limit` as a context ceiling** (ADR 0023/G9d). A guarded
    robust knee over unchanged RRF scores may drop only a statistically distinct low-signal suffix
    after ranking/filtering/dedup/scope; it never reorders, backfills, normalizes, or touches `rrf()`.
    A proposed knee before a note with any top-three BM25/cosine source chunk is vetoed (folded sibling
    ranks survive dedup); unsupported/smooth inputs fail open. `--exhaustive` restores legacy results;
    JSON keeps the same pure Hit-array shape.
18. **Eval evidence must be self-describing and honest** (ADRs 0024/0025, G27). `vagus eval` uses
    fixed-denominator P@k, MRR@k, `null` undefined cohorts, full rankings, and schema-2 label/corpus/
    index/backend/fusion/cohort provenance. It is note-level exhaustive pre-tidy without implicit
    refresh/scope/filter/floor. Fusion claims must pass the non-configurable paired `vagus eval-gate`;
    raw scores remain diagnostics, never calibrated probabilities.

## Layout

These are the **maintainer machine's** paths, not an installation template. Users install from the
brew tap and choose their own home/vault paths; follow the README when helping them onboard.

```
~/code/vasovagal/vagus/     # this repo (org dir ~/code/vasovagal/)
  src/                      # the vagus crate
  design/                   # requirements, ADRs, tradeoffs, prior-art, guardrails  <- READ FIRST
~/brain -> ~/Library/Mobile Documents/com~apple~CloudDocs/Brain   # the vault (markdown only, in iCloud)
~/.local/share/vagus/       # index: tantivy/ + meta.db + config.toml   (OUTSIDE iCloud)
~/Library/Caches/vagus/models/   # cached ONNX models: embedder + optional reranker  (OUTSIDE iCloud)
~/.claude/skills/{create-note,search,process-inbox}/   # Claude Code skill installs
~/.pi/agent/skills/{create-note,search,process-inbox}/  # pi skill installs (both shell out to `vagus`)
```

## Build / test / run

```sh
cargo build              # first build fetches prebuilt ONNX Runtime (network, one-time)
cargo test
cargo clippy --all-targets
./target/debug/vagus …   # run dev builds from target/, never install them
vagus doctor             # verify symlink, model cache, dylib, dims, index health
vagus status
```

The installed `vagus` on PATH comes from the **Homebrew tap** (`brew tap vasovagal/tap && brew
install vagus`), upgraded once per release (see Releasing). Do **not** `cargo install --path .` —
`~/.cargo/bin` precedes `/opt/homebrew/bin` on PATH, so a cargo-installed copy silently shadows the
brew one and drifts. Dev builds run from `target/`.

**Dev builds must not share the installed binary's derived index when their chunk/embed identity may
be different.** A chunk-version mismatch can auto-reindex while a direct embedding mismatch refuses
incremental work under G4; either way, alternating identities wastes work and risks confusing repair.
Use an isolated derived-data directory for dev indexing/search (models are safe to share):

```sh
VAGUS_DATA_DIR=/tmp/vagus-dev ./target/debug/vagus index
```

## Releasing

Push a `vX.Y.Z` tag; see [`RELEASING.md`](./RELEASING.md). The CI/release pipeline follows the laws in
`xrl/agents` `LAWS.md`: split-by-event (`ci.yml` on PR/main, `release.yml` on tags — no test re-run),
native-per-arch matrix (no emulation), centralized pinned-SHA caching, re-run-safe release.

**Every release propagates to the tap, same cycle.** A release is not done until
`vasovagal/homebrew-tap/Formula/vagus.rb` serves the new version: wait for `release.yml` to publish
the GitHub release, then render and push the formula (`VERSION=X.Y.Z scripts/render-formula.sh`,
commit "vagus X.Y.Z" to the tap). Manual by design — CI never writes the tap — so the tap bump is
part of cutting the release, never a follow-up left for later. Finish by upgrading the local
install from the tap: `brew update && brew upgrade vagus` (the machine's `vagus` is brew-installed,
not cargo-installed — see Build / test / run).

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
