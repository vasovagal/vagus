class Vagus < Formula
  desc "Local-first PARA second brain: hybrid search over a Markdown vault"
  homepage "https://github.com/vasovagal/vagus"
  version "0.2.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.2.0/vagus-0.2.0-aarch64-apple-darwin.tar.gz"
      sha256 "8a416554530bde2c9c68b26d8e5c6a6d3efcb587f2fb584cb14bb576c1afcddb"
    else
      odie "vagus ships only Apple Silicon (arm64) macOS bottles. Build from source: " \
           "cargo install --git https://github.com/vasovagal/vagus"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.2.0/vagus-0.2.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "b6c98fdb498119b90d7cf2056ed91e5f6828cebeed8f3404cb495456719c693a"
    else
      url "https://github.com/vasovagal/vagus/releases/download/v0.2.0/vagus-0.2.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "538b4bd77599af7de5c1511d5d86190c6c6109c6856fda3c39faf1869677d9a5"
    end
  end

  def install
    bin.install "vagus"
  end

  test do
    assert_match "vagus #{version}", shell_output("#{bin}/vagus --version")
  end
end
