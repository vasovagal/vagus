# Contributing to vagus

Thanks for hacking on vagus. This file covers building, testing, and the conventions a
change is expected to follow. **Architecture and the hard invariants live elsewhere — read
[`CLAUDE.md`](./CLAUDE.md) and [`design/`](./design/) before any architectural change.**
`design/` holds the requirements, the ADRs (what we considered and why), the tradeoff study,
and the prior-art survey; [`design/guardrails.md`](./design/guardrails.md) is the canonical,
binding invariant list.

## Build / test / run

```sh
cargo build              # first build fetches a prebuilt ONNX Runtime (network, one-time)
cargo test
cargo clippy --all-targets
cargo fmt                # run before every push (CI runs cargo fmt --check)
./target/debug/vagus --version   # run dev builds from target/; do not shadow the brew binary
vagus doctor             # network-free installed-binary health/cache check
vagus status             # counts, model/dims, index size
```

First **build** downloads a prebuilt ONNX Runtime (a static `libonnxruntime.a`) and links
it in — the installed artifact is a self-contained binary (system dylibs only; verify with
`otool -L`). First **run** downloads the embedding model (EmbeddingGemma-300M, ~1.23 GB) to
`~/Library/Caches/vagus/models`, outside iCloud. `vagus doctor --fetch-models` is the explicit
prefetch/validation path; plain doctor never downloads.

Never point a dev build at the installed binary's derived index when identities may differ. Use an
isolated data directory (the model cache can remain shared):

```sh
VAGUS_DATA_DIR=/tmp/vagus-dev ./target/debug/vagus index
```

### Feature flags

The `generate` feature pulls in the tier-1 local rewriter (candle + Qwen GGUF) used by
`vagus search --smart` / `vagus rewrite`. For a leaner build without it:

```sh
cargo build --no-default-features    # add back features as needed
```

## Releasing

Push a `vX.Y.Z` tag — see [`RELEASING.md`](./RELEASING.md) for the full procedure. The
CI/release pipeline follows the laws in the `xrl/agents` `LAWS.md`: split-by-event
(`ci.yml` on PR/main, `release.yml` on tags — no test re-run), native-per-arch build matrix
(no emulation), centralized pinned-SHA caching, re-run-safe release.

## Conventions

- Match the surrounding Rust style; keep modules small and single-purpose (`index`, `chunk`,
  `embed`, `search`, `notes`, `db`, `config`, `cli`).
- All data-producing commands support a stable `--json` shape so the bundled agent skills
  parse rather than scrape — preserve it.
- Commit `Cargo.lock` (this is a binary crate).
- **Run `cargo fmt` before pushing** — never burn a CI cycle on formatting.
- **Meaningful work goes in [`CHANGELOG.md`](./CHANGELOG.md).** User-noticeable changes get
  an entry under `## [Unreleased]` in the same change (Keep a Changelog:
  Added/Changed/Fixed/Removed). Internal refactors / test-only changes don't need one.

## Multi-agent / worktree work

Parallel or swarm work runs in its own git worktree (`.claude/worktrees/<name>` or an
org-level `*-worktrees/` sibling, branched fresh from `origin/main`) — never two agents in
one checkout. **No direct commits to `main`** except releases; everything else goes via a
feature branch + PR (a `git-guard` hook enforces this). See guardrails G21–G24 and
`design/adr/0018-*`. Prune a worktree once its branch merges
(`scripts/worktree-janitor.sh`).

This is a personal repo under the **`vasovagal`** GitHub org (not `scientist-hq`).
