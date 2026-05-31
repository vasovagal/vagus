class Vagus < Formula
  desc "Local-first PARA second brain: hybrid search over a Markdown vault"
  homepage "https://github.com/vasovagal/vagus"
  version "0.4.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.4.0/vagus-0.4.0-aarch64-apple-darwin.tar.gz"
      sha256 "c191a9f3a1890069656ade88e87e26721cd4825799b41a9d7dc635c0b0ff2118"
    else
      odie "vagus ships only Apple Silicon (arm64) macOS bottles. Build from source: " \
           "cargo install --git https://github.com/vasovagal/vagus"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.4.0/vagus-0.4.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "bf89d58272eb7e8c38586a6d0ca41ef9a9cc4de3377421d951d760f67ddd557f"
    else
      url "https://github.com/vasovagal/vagus/releases/download/v0.4.0/vagus-0.4.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "64043a8342961f78b9a5385ff361586cd56733ed754c1cdde023314f909dc162"
    end
  end

  def install
    bin.install "vagus"
  end

  test do
    assert_match "vagus #{version}", shell_output("#{bin}/vagus --version")
  end
end
