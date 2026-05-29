class Vagus < Formula
  desc "Local-first PARA second brain: hybrid search over a Markdown vault"
  homepage "https://github.com/vasovagal/vagus"
  version "0.1.2"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.2/vagus-0.1.2-aarch64-apple-darwin.tar.gz"
      sha256 "cda6ba7228e72d2b355f5976cb16fff677185d25cc8b7a0d43e3db459fdad6bc"
    else
      odie "vagus ships only Apple Silicon (arm64) macOS bottles. Build from source: " \
           "cargo install --git https://github.com/vasovagal/vagus"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.2/vagus-0.1.2-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "25e880a7cf63891e1281323a7538795f4beea6d5a09f73b05645efccb2534c34"
    else
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.2/vagus-0.1.2-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "f6de9608d7c8b2a7cccb080c29c5d93dc5f6e3ba43aa0e8a80da4c0d107cce42"
    end
  end

  def install
    bin.install "vagus"
  end

  test do
    assert_match "vagus #{version}", shell_output("#{bin}/vagus --version")
  end
end
