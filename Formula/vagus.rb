class Vagus < Formula
  desc "Local-first PARA second brain: hybrid search over a Markdown vault"
  homepage "https://github.com/vasovagal/vagus"
  version "0.1.7"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.7/vagus-0.1.7-aarch64-apple-darwin.tar.gz"
      sha256 "aca9afcbfe81b7627f5d6a5bf83aadf53a2be7fd117dae8e657a4030dc015d25"
    else
      odie "vagus ships only Apple Silicon (arm64) macOS bottles. Build from source: " \
           "cargo install --git https://github.com/vasovagal/vagus"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.7/vagus-0.1.7-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "adb2bcb89eb283393b09c8535618ec0bc39e3702f64a68142e0cf5510fdb725a"
    else
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.7/vagus-0.1.7-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "d06d0e7d056bb82f371d651280b689ea398e4eca40dd69d73151b16d1f1b4242"
    end
  end

  def install
    bin.install "vagus"
  end

  test do
    assert_match "vagus #{version}", shell_output("#{bin}/vagus --version")
  end
end
