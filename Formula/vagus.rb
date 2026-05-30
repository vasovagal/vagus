class Vagus < Formula
  desc "Local-first PARA second brain: hybrid search over a Markdown vault"
  homepage "https://github.com/vasovagal/vagus"
  version "0.1.8"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.8/vagus-0.1.8-aarch64-apple-darwin.tar.gz"
      sha256 "914205a3271e2c32ffcc159ee3f5663fbd49131c7c4f36c4f5d7e609b5dd0aa9"
    else
      odie "vagus ships only Apple Silicon (arm64) macOS bottles. Build from source: " \
           "cargo install --git https://github.com/vasovagal/vagus"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.8/vagus-0.1.8-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "9629dcc7861a1f753a7757c742d37095ce69b5845ed27ee7e390f11c3a22e7ba"
    else
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.8/vagus-0.1.8-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "9558ca7d7a010a196126c09880ae1295958434cd91b3c362924095a1a23271f3"
    end
  end

  def install
    bin.install "vagus"
  end

  test do
    assert_match "vagus #{version}", shell_output("#{bin}/vagus --version")
  end
end
