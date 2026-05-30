class Vagus < Formula
  desc "Local-first PARA second brain: hybrid search over a Markdown vault"
  homepage "https://github.com/vasovagal/vagus"
  version "0.3.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.3.0/vagus-0.3.0-aarch64-apple-darwin.tar.gz"
      sha256 "1a53c75036988f637d9afb724d62993574313151aeae9b649845782ec45b1db9"
    else
      odie "vagus ships only Apple Silicon (arm64) macOS bottles. Build from source: " \
           "cargo install --git https://github.com/vasovagal/vagus"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.3.0/vagus-0.3.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "6d68e5d7a613681642484bef8a44696a94ad33b84fd1b0ca118bd387abde3d01"
    else
      url "https://github.com/vasovagal/vagus/releases/download/v0.3.0/vagus-0.3.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "73ad22f3c935a15e3a64a6ef8423163b6044291f77505cd3146711e869ef70f2"
    end
  end

  def install
    bin.install "vagus"
  end

  test do
    assert_match "vagus #{version}", shell_output("#{bin}/vagus --version")
  end
end
