class Vagus < Formula
  desc "Local-first PARA second brain: hybrid search over a Markdown vault"
  homepage "https://github.com/vasovagal/vagus"
  version "0.1.5"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.5/vagus-0.1.5-aarch64-apple-darwin.tar.gz"
      sha256 "555c98101921e32f7721753d924cf0f3785921b5c705d979ccb175ba0b327f1d"
    else
      odie "vagus ships only Apple Silicon (arm64) macOS bottles. Build from source: " \
           "cargo install --git https://github.com/vasovagal/vagus"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.5/vagus-0.1.5-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "5af5d050d6f43d029fd02d0b64593bf3c9f700a690f8def37c5200f44dfaac12"
    else
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.5/vagus-0.1.5-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "b518cc7947e052a4093bfa8bf83f389260d23a4b9256eb9fa47b28e939543f0e"
    end
  end

  def install
    bin.install "vagus"
  end

  test do
    assert_match "vagus #{version}", shell_output("#{bin}/vagus --version")
  end
end
