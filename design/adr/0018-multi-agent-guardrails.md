# ADR 0018 — Multi-agent operation & worktree guardrails

- **Status:** Accepted (2026-05-30); **amended 2026-05-31** — G22 carves out releases (version/formula
  bump commits + `vX.Y.Z` tag pushes may land directly on `main`).

## Context

vagus development is moving toward a multi-agent ("swarm") style where several Claude Code
sessions may work the repo concurrently. Nothing today guards that mode:

- No documented worktree policy — three stale, already-merged worktrees
  (`file-stats`, `m3-search-skill`, `search-since-source`) are lying around, and two agents
  in the same checkout would clobber each other's edits and thrash `target/`.
- Nothing stops a direct commit to `main`. The release pipeline (`RELEASING.md`, the `xrl/agents`
  `LAWS.md`) already assumes a green, PR-gated `main` — a tag trusts the main it was cut from — so
  direct commits to `main` quietly break that contract.
- The `design/` record (the project's "second brain") stays current only by human diligence, and it
  is **already drifting**: the README ADR index was missing `0017`.

The goal is a **lean, convention-first** set of guardrails: prevent the collisions that actually
hurt, keep the design record self-healing, and avoid heavyweight machinery that nags or blocks
ordinary solo work.

## Decision

- **Worktree isolation is a convention, not a lock.** Parallel/swarm work runs in its own git
  worktree — in-repo under `.claude/worktrees/<name>` or org-level under
  `~/code/vasovagal/.vagus-worktrees/` — branched **fresh from `origin/main`**
  (`.claude/settings.json` pins `worktree.baseRef = "fresh"`). This is reinforced by the native
  `Agent`/`Workflow` `isolation: 'worktree'` option, which the orchestrator uses for fan-out. We do
  **not** install a blocking lock hook (see rejected alternatives). (G21)
- **No direct commits to `main`, except releases.** Code/doc changes land via a feature branch + PR,
  matching the CI laws. **Releases are exempt** and may land directly on `main` (`RELEASING.md`): a
  version bump or the CI formula bump — a commit staging only `Cargo.toml`/`Cargo.lock`/`CHANGELOG.md`/
  `Formula/` — plus `vX.Y.Z` tag pushes. A `git-guard` `PreToolUse` hook (`scripts/git-guard.sh`) denies
  non-release `git commit`s while `HEAD` is `main` and denies pushing the `main` branch, but allows
  release-only commits and tag pushes, pointing the agent at a feature branch otherwise. The hook
  **fails open** (a missing `jq` or non-git cwd never blocks work). (G22)
- **Worktree hygiene.** A worktree is removed once its branch merges. `scripts/worktree-janitor.sh`
  lists worktrees whose branch is already merged into `origin/main` (run in `--list` mode by a
  `SessionStart` hook, quiet when there is nothing to report) and `--prune` removes the clean ones,
  refusing any dirty worktree. (G23)
- **Leave breadcrumbs.** Every architectural decision updates the matching ADR and moves the README
  ADR index, `guardrails.md`, and `CLAUDE.md` in the **same change**. This is nudged **softly**, not
  gated: the `git-guard` hook emits a one-line reminder when a `git commit` touches `src/**` with no
  `design/**` or `CHANGELOG.md` change staged (commit-time, not per-turn, to avoid nagging), and a
  PR template (`.github/pull_request_template.md`) carries the checklist. (G24)
- **Two `CLAUDE.md` conventions** ride along (workflow rules, not architectural invariants, so they
  live in `## Conventions`, not `guardrails.md`): run `cargo fmt` before pushing and **don't** read
  the reformatted output back (CI's `cargo fmt --check` is the backstop; inspect only if something
  breaks); and record user-noticeable work in `CHANGELOG.md` under `## [Unreleased]` (Keep a Changelog
  format) in the same change.

## Consequences

- Two agents in two worktrees get isolated checkouts and isolated `target/` dirs; two agents in the
  *same* checkout is now a documented anti-pattern rather than a silent footgun — but nothing
  mechanically prevents it (deliberately, to keep solo edits friction-free).
- `main` stays releasable by construction: the only paths onto it are a merged PR **or a release commit
  (version/formula bump, then a `vX.Y.Z` tag)**, preserving the "tag trusts green main" release law.
- The design record self-heals against the most common drift (an ADR with no index line, a guardrail
  with no CLAUDE.md mirror) via review + the soft nudge, without a CI gate that would block on
  judgment calls (is *this* change "architectural"?).
- The committed `.claude/settings.json` adds hooks that **merge** with each contributor's global
  Claude settings (they don't override), so the guards apply to anyone working the repo; runtime
  worktrees and `settings.local.json` are git-ignored.
- New, low-maintenance surface: two shell scripts, one settings file, a PR template, and a
  `CHANGELOG.md`. No Rust is touched and no CI workflow changes.

## Alternatives considered & rejected

- **Hard worktree lock hook** (a `PreToolUse` lock keyed on cwd that blocks a second concurrent
  session from mutating a held checkout). Rejected as too aggressive: it would interrupt ordinary
  solo edits and the single-user common case for a hazard that the convention + native worktree
  isolation already cover. The convention can be hardened into a lock later if dueling actually bites.
- **Per-worktree index sandbox** (a `SessionStart` hook exporting `VAGUS_DATA_DIR` to a scratch dir
  so parallel agents never corrupt the single shared index at `~/.local/share/vagus` — `Config::load`
  already honors the var, `src/config.rs`). **Deferred.** It is a real hazard (concurrent `reindex`
  breaches G1/G5's single-writer assumption), but the mitigation for now is manual: agents that index
  in parallel set `VAGUS_DATA_DIR` themselves. Revisit if swarm indexing becomes routine.
- **ADR-number / G-number collision CI check.** Two branches could both grab `0018` or reuse a
  `G2x`. **Deferred** — relies on review for now; a mechanical `scripts/check-design.sh` in `ci.yml`
  is the obvious next step if a collision ever lands.
- **CI consistency gate** (fail a PR when the ADR index / guardrails / CLAUDE.md drift) and a running
  **`design/devlog.md`** session journal. **Deferred** — kept soft. ADR `Status` lines, `Consequences`
  sections, `CHANGELOG.md`, and git history remain the dev-history record; a separate journal was
  judged redundant at current scale.
- **Consuming `CHANGELOG.md` at release time.** `release.yml` does not read a changelog today
  (`RELEASING.md` only references the reindex note). Wiring the `## [Unreleased]` section into the
  GitHub release body is a reasonable future option, out of scope here.
