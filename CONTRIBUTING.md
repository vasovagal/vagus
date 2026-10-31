# Contributing to vagus

Build and check a change with the commands below, then submit it on a feature branch.
Read [`CLAUDE.md`](./CLAUDE.md) and the binding
[`design/guardrails.md`](./design/guardrails.md) first. For an architectural change, also read
and update the relevant [design decision](./design/README.md#adr-index).

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

The first build downloads and statically links ONNX Runtime. Verify system-only linkage with
`otool -L` on macOS or `ldd` on Linux after changing native dependencies or platforms.
The first model-backed command downloads its missing models; on macOS, the default cache is
`~/Library/Caches/vagus/models`, outside iCloud. Plain `vagus doctor` checks file presence without
loading or downloading models; `vagus doctor --fetch-models` downloads and validates both ONNX models.

Run dev binaries from `target/`; do not install them over the Homebrew copy. If a dev build may use
a different chunk or embedding identity, isolate its derived index. Models can remain shared:

```sh
VAGUS_DATA_DIR=/tmp/vagus-dev ./target/debug/vagus index
```

### Feature flags

The default `generate` feature includes the local candle/Qwen rewriter used by
`vagus search --smart` and `vagus rewrite`. Build without it with:

```sh
cargo build --no-default-features    # add back features as needed
```

### CI checks

[CI](.github/workflows/ci.yml) runs on pull requests and `main` pushes. It checks formatting,
Clippy with warnings denied, locked tests, and the lean build without default features.
The commands above are local development checks; plain `cargo clippy --all-targets` does not deny warnings.

## Releasing

Follow [the release procedure](RELEASING.md#cut-a-release) after CI passes on `main`.
Tag builds use native runners and do not repeat the PR tests. The release is not complete until
the [manual Homebrew tap update](RELEASING.md#the-homebrew-tap) is done.

## Conventions

- Match the surrounding Rust style and keep modules focused.
- Preserve command JSON contracts so agent skills can parse output.
- Commit `Cargo.lock`; run `cargo fmt` before pushing.
- Record user-visible changes under `## [Unreleased]` in [`CHANGELOG.md`](CHANGELOG.md), using
  Added/Changed/Fixed/Removed. Internal refactors and test-only changes need no entry.

## Multi-agent / worktree work

Use one worktree per parallel agent, branched fresh from `origin/main`. Put it under
`.claude/worktrees/<name>` or an organization-level `*-worktrees/` sibling. Code and docs go through
a feature branch and PR; only releases may commit directly to `main`.
[ADR 0018](design/adr/0018-multi-agent-guardrails.md#decision) defines the release exception and
`git-guard` hook. After a branch merges, prune its clean worktree with `scripts/worktree-janitor.sh`.

This repository belongs to the **`vasovagal`** organization, not `scientist-hq`.
