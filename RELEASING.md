# Releasing vagus

Release from a green `main`, publish the native binaries, then update the shared Homebrew tap.
A `vX.Y.Z` tag starts [the release workflow](.github/workflows/release.yml); it builds and publishes
assets without repeating the PR tests. The site update is automated; the tap update is manual.

## Cut a release

1. Set `version` in `Cargo.toml` to `X.Y.Z`, update the package entry in `Cargo.lock` with Cargo,
   and record user-visible changes in `CHANGELOG.md`. Commit and wait for `ci` to pass on `main`.
2. Tag and push:
   ```sh
   git tag vX.Y.Z && git push origin vX.Y.Z
   ```
3. Wait for all three native builds: `aarch64-apple-darwin`, `aarch64-unknown-linux-gnu`, and
   `x86_64-unknown-linux-gnu`. Each uploads `vagus-X.Y.Z-<target>.tar.gz` to the GitHub release
   using `GITHUB_TOKEN`. Reruns replace assets with `--clobber`.
4. Confirm the final site job succeeds. It validates the public release's exact three-asset set,
   then updates `vasovagal.github.io` using the repository-scoped `LANDING_PAGE_DEPLOY_KEY`, with
   commit `vagus bumped to X.Y.Z`. Repeated updates are idempotent; older versions leave the site unchanged.
5. Complete [the manual tap update](#the-homebrew-tap) in the same release cycle.

## Upgrade notes

Changing the embedding model, dimensions, or document recipe requires a full reindex. Bump
`CHUNK_VERSION` with such changes: a chunk-version mismatch triggers a one-time full rebuild on
index refresh, with a notice on stderr. An embedding-identity mismatch **without** that chunk bump
refuses incremental work and asks for `vagus reindex`; it does not reuse incompatible vectors.

Tell users to run `vagus reindex` after such an upgrade, then `vagus doctor` to check the embedding
identity and `files/chunks/embedded` counts. Re-embedding can be slow, and a new model must download
into the model cache outside iCloud. An interrupted rebuild resumes only through explicit
`vagus index` or `vagus reindex`; automatic search/capture refreshes warn and defer it.
See [the indexing contract](design/guardrails.md#index-correctness).

## The Homebrew tap

CI never writes the shared [`vasovagal/homebrew-tap`](https://github.com/vasovagal/homebrew-tap).
Users install from it with:

```sh
brew tap vasovagal/tap
brew install vagus
```

After the assets publish, render the formula and commit it in your tap checkout. Replace
`.../homebrew-tap` with that checkout's path:

```sh
VERSION=X.Y.Z scripts/render-formula.sh > .../homebrew-tap/Formula/vagus.rb
cd .../homebrew-tap && git commit -am "vagus X.Y.Z" && git push
```

Then upgrade the maintainer's Homebrew install with `brew update && brew upgrade vagus`.

## Targets

Release assets cover macOS **arm64**, Linux **arm64**, and Linux **amd64**, built on native runners.
Intel macOS has no release asset; a [source build](CONTRIBUTING.md#build--test--run) is outside
that release matrix.
