# Releasing vagus

Releases are cut by pushing a `vX.Y.Z` tag. The `release` workflow then does *only* tag-specific work
(Law 3): build native per-arch binaries and publish a GitHub release. It does **not** re-run the test
matrix — the tag trusts the green `main` it was cut from. The Homebrew formula lives in the external
shared tap (`vasovagal/homebrew-tap`) and is updated **manually** after the release — CI never writes
the tap.

## Cut a release

1. Bump `version` in `Cargo.toml` to `X.Y.Z`; commit; let `ci` go green on `main`.
2. Tag and push:
   ```sh
   git tag vX.Y.Z && git push origin vX.Y.Z
   ```
3. `release.yml` builds `aarch64-apple-darwin`, `aarch64-unknown-linux-gnu`, and
   `x86_64-unknown-linux-gnu` on native runners (Law 1) and uploads `vagus-X.Y.Z-<target>.tar.gz` to the
   GitHub release using the built-in `GITHUB_TOKEN`. It does **not** touch the Homebrew formula.
4. After the release, manually update the tap (see below) — CI never writes the tap.

Re-run-safe (Law 19): re-running re-uploads with `--clobber`.

## Upgrade notes

When a release changes the **embedding identity** (`EMBED_MODEL`/`EMBED_DIMS`) or the **chunk format**
(`CHUNK_VERSION`), the index self-heals: the next `vagus index`/`vagus search` detects the `meta`
mismatch (G4) and force-reindexes the whole vault automatically, printing a one-line stderr notice.
That first post-upgrade run is slow (it re-embeds everything, and a new embedder downloads its model —
EmbeddingGemma-300M is ~1.23 GB to `~/Library/Caches/vagus/models`, outside iCloud). Tell users in the
release notes to run **`vagus reindex`** once at their convenience so the cost isn't paid mid-search,
then `vagus doctor` to confirm `embed identity` and consistent `files/chunks/embedded`.

## The Homebrew tap

The formula lives in the **shared manual tap** `vasovagal/homebrew-tap` — not in this repo. CI never
writes the tap (no PAT, no auto-commit). Users tap it by name:

```sh
brew tap vasovagal/tap
brew install vagus
```

After a release, update the tap by hand: render the formula from the published assets and commit it to
`vasovagal/homebrew-tap`:

```sh
VERSION=X.Y.Z scripts/render-formula.sh > .../homebrew-tap/Formula/vagus.rb
cd .../homebrew-tap && git commit -am "vagus X.Y.Z" && git push
```

## Targets

macOS **arm64**, Linux **arm64**, Linux **amd64** (native runners). Intel macOS isn't shipped —
`cargo install --git https://github.com/vasovagal/vagus`.
