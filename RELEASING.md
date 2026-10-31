# Releasing vagus

Releases are cut by pushing a `vX.Y.Z` tag. The `release` workflow then does *only* tag-specific work
(Law 3): build native per-arch binaries and publish a GitHub release. It does **not** re-run the test
matrix — the tag trusts the green `main` it was cut from. After every native asset uploads, the final job
updates `vasovagal.github.io` through a Vagus-specific deploy key and commits `vagus bumped to X.Y.Z`.
The Homebrew formula lives in the external shared tap (`vasovagal/homebrew-tap`) and is updated
automatically after public asset validation by an independent job.

## Cut a release

1. Bump `version` in `Cargo.toml` to `X.Y.Z`; commit; let `ci` go green on `main`.
2. Tag and push:
   ```sh
   git tag vX.Y.Z && git push origin vX.Y.Z
   ```
3. `release.yml` builds `aarch64-apple-darwin`, `aarch64-unknown-linux-gnu`, and
   `x86_64-unknown-linux-gnu` on native runners (Law 1) and uploads `vagus-X.Y.Z-<target>.tar.gz` to the
   GitHub release using the built-in `GITHUB_TOKEN`.
4. After all three assets are present, the workflow validates the public release and updates the landing
   page with the repository-scoped `LANDING_PAGE_DEPLOY_KEY`. Workflow reruns are idempotent, and the site
   rejects an older tag rather than downgrading its displayed version.
5. Verify the Homebrew job updated the tap (see below); rerun the workflow if it failed.

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

The formula lives in the **shared tap** `vasovagal/homebrew-tap` — not in this repo. Users tap it by name:

```sh
brew tap vasovagal/tap
brew install vagus
```

The release workflow validates the public three-asset contract, renders the formula, then mints a
short-lived GitHub App installation token scoped to **only** `homebrew-tap` with Contents write. It
commits only `Formula/vagus.rb`, retries concurrent non-fast-forward pushes, and refuses downgrade or
same-version drift. The source release uses `GITHUB_TOKEN` only for its own assets. Provision the org App
installed only on `vasovagal/homebrew-tap`; set source repository variable
`HOMEBREW_PUBLISHER_CLIENT_ID` and secret `HOMEBREW_PUBLISHER_PRIVATE_KEY` (no PAT). If publication
fails, rerun the workflow; for manual recovery render with `VERSION=X.Y.Z scripts/render-formula.sh`
and commit only the rendered formula to the tap.

## Targets

macOS **arm64**, Linux **arm64**, Linux **amd64** (native runners). Intel macOS isn't shipped —
`cargo install --git https://github.com/vasovagal/vagus`.
