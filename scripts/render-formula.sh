#!/usr/bin/env bash
# Render Formula/vagus.rb for a published release, to stdout.
# Downloads the release assets from the PUBLIC GitHub release to compute each sha256 (no token).
#
#   VERSION=0.1.0 scripts/render-formula.sh > Formula/vagus.rb
#
# Used by both the release workflow's update-formula job and by hand for the first release.
set -euo pipefail

VERSION="${VERSION:?set VERSION, e.g. VERSION=0.1.0}"
REPO="${REPO:-vasovagal/vagus}"
BASE="https://github.com/${REPO}/releases/download/v${VERSION}"

sha() { # $1 = target triple
  curl -fsSL "${BASE}/vagus-${VERSION}-$1.tar.gz" | shasum -a 256 | awk '{print $1}'
}

MAC_ARM="$(sha aarch64-apple-darwin)"
LIN_ARM="$(sha aarch64-unknown-linux-gnu)"
LIN_X86="$(sha x86_64-unknown-linux-gnu)"

cat <<RB
class Vagus < Formula
  desc "Local-first PARA second brain: hybrid search over a Markdown vault"
  homepage "https://github.com/${REPO}"
  version "${VERSION}"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "${BASE}/vagus-${VERSION}-aarch64-apple-darwin.tar.gz"
      sha256 "${MAC_ARM}"
    else
      odie "vagus ships only Apple Silicon (arm64) macOS bottles. Build from source: " \\
           "cargo install --git https://github.com/${REPO}"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "${BASE}/vagus-${VERSION}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "${LIN_ARM}"
    else
      url "${BASE}/vagus-${VERSION}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "${LIN_X86}"
    end
  end

  def install
    bin.install "vagus"
  end

  test do
    assert_match "vagus #{version}", shell_output("#{bin}/vagus --version")
  end
end
RB
