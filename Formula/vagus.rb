class Vagus < Formula
  desc "Local-first PARA second brain: hybrid search over a Markdown vault"
  homepage "https://github.com/vasovagal/vagus"
  version "0.5.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.5.0/vagus-0.5.0-aarch64-apple-darwin.tar.gz"
      sha256 "7b74ef6b4c3ee6431c348cb3eab1241ee276158fe90d831580e449353c151112"
    else
      odie "vagus ships only Apple Silicon (arm64) macOS bottles. Build from source: " \
           "cargo install --git https://github.com/vasovagal/vagus"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.5.0/vagus-0.5.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "101dcab2c7544139b079db2179021e0b45b5cd68608141b6bcc62793a9b5e1f8"
    else
      url "https://github.com/vasovagal/vagus/releases/download/v0.5.0/vagus-0.5.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "a3c25a5eaf6d4a223551b4ad40e3f974f0c14103894b9ee2270e34ecaeaa9ce5"
    end
  end

  def install
    bin.install "vagus"
  end

  test do
    assert_match "vagus #{version}", shell_output("#{bin}/vagus --version")
  end
end
