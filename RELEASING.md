# Releasing vagus

Releases are cut by pushing a `vX.Y.Z` tag. The `release` workflow then does *only* tag-specific work
(Law 3): build native per-arch binaries, publish a GitHub release, and bump the Homebrew formula.
It does **not** re-run the test matrix — the tag trusts the green `main` it was cut from.

## Cut a release

1. Bump `version` in `Cargo.toml` to `X.Y.Z`; commit; let `ci` go green on `main`.
2. Tag and push:
   ```sh
   git tag vX.Y.Z && git push origin vX.Y.Z
   ```
3. `release.yml` builds `aarch64-apple-darwin`, `aarch64-unknown-linux-gnu`, and
   `x86_64-unknown-linux-gnu` on native runners (Law 1), uploads `vagus-X.Y.Z-<target>.tar.gz` to the
   GitHub release, then regenerates `Formula/vagus.rb` **in this repo** and commits it to `main`
   (with `[skip ci]`) using the built-in `GITHUB_TOKEN` — no PAT, no second repo.

Re-run-safe (Law 19): re-running re-uploads with `--clobber` and re-commits the formula only on change.

## Upgrade notes

When a release changes the **embedding identity** (`EMBED_MODEL`/`EMBED_DIMS`) or the **chunk format**
(`CHUNK_VERSION`), the index self-heals: the next `vagus index`/`vagus search` detects the `meta`
mismatch (G4) and force-reindexes the whole vault automatically, printing a one-line stderr notice.
That first post-upgrade run is slow (it re-embeds everything, and a new embedder downloads its model —
EmbeddingGemma-300M is ~1.23 GB to `~/Library/Caches/vagus/models`, outside iCloud). Tell users in the
release notes to run **`vagus reindex`** once at their convenience so the cost isn't paid mid-search,
then `vagus doctor` to confirm `embed identity` and consistent `files/chunks/embedded`.

## The Homebrew tap

The formula lives **in this repo** at `Formula/vagus.rb` — no separate `homebrew-*` repo and no PAT
(the release commits it with the built-in `GITHUB_TOKEN`). Users tap it by URL:

```sh
brew tap vasovagal/vagus https://github.com/vasovagal/vagus.git
brew install vagus
```

To regenerate the formula by hand (e.g. to backfill a release):

```sh
VERSION=X.Y.Z scripts/render-formula.sh > Formula/vagus.rb
git commit -am "vagus X.Y.Z" && git push
```

## Targets

macOS **arm64**, Linux **arm64**, Linux **amd64** (native runners). Intel macOS isn't shipped —
`cargo install --git https://github.com/vasovagal/vagus`.
