# Changelog

All notable, user-noticeable changes to `vagus` are recorded here. Internal refactors and test-only
changes are intentionally omitted (CLAUDE.md → Conventions).

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). The most recent tagged release is `v0.4.0`;
entries above it accumulate under **Unreleased** until the next `vX.Y.Z` tag.

## [Unreleased]

### Added

- Multi-agent / worktree guardrails (ADR 0018): worktree-per-agent isolation as a convention
  (`worktree.baseRef = fresh`), a `git-guard` hook that blocks direct commits/pushes to `main`, a
  `worktree-janitor` that lists/prunes merged worktrees, a soft commit-time breadcrumb nudge, and a PR
  template. Documented as guardrails G21–G24.
- This `CHANGELOG.md`, plus a `CLAUDE.md` convention to run `cargo fmt` before pushing and to record
  meaningful work here.

## [0.4.0] — 2026-05-30

- M3 Opus `/search` skill (tier-2 reranking), `search --since`/`--source` frontmatter filters
  (ADR 0017), and `vagus file --stats` per-step timing. See git history for detail; entries before this
  release predate the changelog.
