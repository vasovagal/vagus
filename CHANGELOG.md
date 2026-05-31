# Changelog

All notable, user-noticeable changes to `vagus` are recorded here. Internal refactors and test-only
changes are intentionally omitted (CLAUDE.md → Conventions).

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). The most recent tagged release is `v0.4.0`;
entries above it accumulate under **Unreleased** until the next `vX.Y.Z` tag.

## [Unreleased]

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
