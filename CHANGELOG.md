# Changelog

All notable, user-noticeable changes to `vagus` are recorded here. Internal refactors and test-only
changes are intentionally omitted (CLAUDE.md → Conventions).

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). The most recent tagged release is `v0.8.0`;
entries above it accumulate under **Unreleased** until the next `vX.Y.Z` tag.

## [Unreleased]

### Added

- `vagus chunk <id|path>...`: print full chunk bodies by chunk id (full 64-hex or a unique >=8-char
  hex prefix) or every chunk of a note by vault-relative path. A pure index read — no model load,
  no index refresh, no usage tick. Stable `--json` shape (one element per resolved chunk in request
  order; unresolved args yield `"missing": true`). Builtin `chunk` shadows any same-named plugin.

### Changed

- **The `/search` skill is now two-phase** (progressive disclosure): a compact
  `--json --rerank --limit 20` pass with no bodies, Opus snippet-triage to a 5–8 shortlist, then
  `vagus chunk` fetches only the shortlist's full bodies for judging — cutting worst-case session
  tokens per search by more than half. `vagus search` flags and output are unchanged (`--full`
  behaves exactly as before). Re-run `vagus skills install` after upgrading.
- **`vagus search --rerank` is substantially faster** — the cross-encoder now scores only the top
  `(limit*2).max(16)` fused candidates instead of the whole retrieval pool (~60 → ~30 at `--limit
  15`), roughly halving its forward-pass work. Retrieval, `--since`/`--source` filtering, and
  note-dedup still run at full pool depth, so note fill is unchanged; lower-ranked hits keep their
  RRF order after the reranked prefix. Only the top candidates are now eligible to be reranked to the
  front — a deliberate recall-vs-latency tradeoff. `--min-score` still reranks the whole pool, so its
  relative-to-top floor stays meaningful. The default (no `--rerank`) `--json` shape is byte-identical;
  under `--rerank` the un-scored tail hits omit the optional `rerank` field (G9a). (ADR 0015)

### Fixed

- Empty-bodied chunks no longer waste index space with a garbage embedding. A section with no prose
  (an H1 title lead whose content lives under H2s, or an ancestor heading) produced an empty-bodied
  chunk; where the heading's tokens already survive in a descendant chunk's breadcrumb, that empty
  chunk is now dropped. A **bodyless leaf heading** (a placeholder section like `## Open Questions`
  with nothing under it, and a bare `# Foo` stub) is instead kept as a heading-only chunk — its
  heading becomes the body — so the heading text stays searchable (previously it rode along only as
  an empty-bodied chunk's `heading` field; dropping such chunks outright would have removed those
  tokens from full-text search). A truly contentless note (no heading, no prose) now indexes nothing
  instead of injecting an empty vector. A one-time auto-reindex applies this on upgrade.

## [0.8.0] — 2026-07-08

### Added

- `vagus tick` and `vagus fame`: local usage counts for notes, recorded by the `/search` skill when
  it presents results; survive reindex, follow `vagus file` moves, never touch the vault (ADR 0021).
  Builtin `tick`/`fame` shadow any same-named plugins. Re-run `vagus skills install` to pick up the
  skill change.

## [0.7.0] — 2026-06-09

### Changed

- **`vagus search --limit N` now returns N distinct notes** instead of N chunks (ADR 0020). Each
  note appears once as its best-ranked chunk, with a `siblings` count of the other ranked chunks
  folded into it (omitted when zero, so single-chunk hits are unchanged). Previously a long note
  matching broadly could fill several of the top slots, so "10 hits" might span only 3–4 notes.
  Ranking itself is untouched — dedup is a post-rank stage like the `--since`/`--source` filters.

### Added

- `vagus search --chunks`: raw chunk-level hits — `--limit` counts chunks, the pre-0.7 behavior.
  Output (including `--json`) is byte-identical to v0.6.1.

## [0.6.1] — 2026-06-07

### Added

- **Embedded vector index: usearch HNSW**, statically linked into the single binary (ADR 0019). Semantic
  and hybrid search now rank via an approximate-nearest-neighbour index instead of a brute-force scan,
  giving real headroom as the vault grows. The `.usearch` sidecar lives outside iCloud and is a
  rebuildable cache of the authoritative f32 vectors — upgrading **backfills it from existing embeddings
  with no re-embed and no reindex**.
- `vagus search --exact`: force exact brute-force semantic search (100% recall) instead of the
  approximate index — the ground-truth escape hatch.
- `vagus doctor` reports the usearch index health (vector count vs embedded chunks, G5 cross-check), and
  `vagus status` shows the sidecar path + size. `vagus file --stats` gains a `vector_ms` timing.

## [0.5.0] — 2026-05-31

### Added

- Multi-agent / worktree guardrails (ADR 0018): worktree-per-agent isolation as a convention
  (`worktree.baseRef = fresh`), a `git-guard` hook that blocks direct commits/pushes to `main`, a
  `worktree-janitor` that lists/prunes merged worktrees, a soft commit-time breadcrumb nudge, and a PR
  template. Documented as guardrails G21–G24.
- This `CHANGELOG.md`, plus a `CLAUDE.md` convention to run `cargo fmt` before pushing and to record
  meaningful work here.
- `vagus search --timings`: print a per-stage wall-clock breakdown (rewrite/embed/rerank load +
  compute, fuse, total) to stderr for `--smart`/`--rerank`. A diagnostic + regression guard; stdout
  and the `--json` Hit shape are unchanged (G9a).

### Changed

- `vagus search --smart` is substantially faster — **~9.5 s → ~5 s on a cold query and ~2.3 s on a
  repeat** on a small vault, with ranking (RRF + rerank) unchanged. Four changes (ADR 0016):
  - The embedder and cross-encoder reranker now load on background threads that overlap the local
    LLM's query-expansion decode, so their cold loads (~2 s embedder + ~0.15 s reranker) no longer sit
    serially on the critical path. Not a daemon — the threads are joined within the one-shot process
    (G14).
  - On macOS the quantized rewriter now decodes on the **Apple GPU via candle's Metal backend** (~2.5×
    faster decode), falling back to CPU if Metal can't initialize. macOS-only; Linux/lean builds are
    unchanged, and the binary stays self-contained (system frameworks only — G13).
  - The deterministic query expansion is **cached** (`meta.db`, keyed on query + model identity +
    sampling params), so a repeat query skips the LLM entirely.
  - The rewriter's token ceiling is capped (512 → 192) to bound a pathological non-terminating
    generation; real output is never clipped.

## [0.4.0] — 2026-05-30

- M3 Opus `/search` skill (tier-2 reranking), `search --since`/`--source` frontmatter filters
  (ADR 0017), and `vagus file --stats` per-step timing. See git history for detail; entries before this
  release predate the changelog.

[Unreleased]: https://github.com/vasovagal/vagus/compare/v0.8.0...HEAD
[0.8.0]: https://github.com/vasovagal/vagus/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/vasovagal/vagus/compare/v0.6.1...v0.7.0
[0.6.1]: https://github.com/vasovagal/vagus/compare/v0.5.0...v0.6.1
[0.5.0]: https://github.com/vasovagal/vagus/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/vasovagal/vagus/releases/tag/v0.4.0
