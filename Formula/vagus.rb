class Vagus < Formula
  desc "Local-first PARA second brain: hybrid search over a Markdown vault"
  homepage "https://github.com/vasovagal/vagus"
  version "0.1.6"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.6/vagus-0.1.6-aarch64-apple-darwin.tar.gz"
      sha256 "ec4c9f6be9d2b31aa2af17e5ca67355e4e63435d26fefe70f2ff4b803b4f4716"
    else
      odie "vagus ships only Apple Silicon (arm64) macOS bottles. Build from source: " \
           "cargo install --git https://github.com/vasovagal/vagus"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.6/vagus-0.1.6-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "03d3fa3ca198e26eb5874fd5b46495a443ee4e9d2ce3443e096923f182583d8a"
    else
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.6/vagus-0.1.6-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "034c98fdd894dd7735afda65e3b3b169ae3838ec3f0ef1d3671846f2c3b0291a"
    end
  end

  def install
    bin.install "vagus"
  end

  test do
    assert_match "vagus #{version}", shell_output("#{bin}/vagus --version")
  end
end
