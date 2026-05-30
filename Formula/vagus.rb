class Vagus < Formula
  desc "Local-first PARA second brain: hybrid search over a Markdown vault"
  homepage "https://github.com/vasovagal/vagus"
  version "0.1.3"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.3/vagus-0.1.3-aarch64-apple-darwin.tar.gz"
      sha256 "949a1fba9435968729f8182d90557cf01d0122a5600aa445dcddfe8255cf2090"
    else
      odie "vagus ships only Apple Silicon (arm64) macOS bottles. Build from source: " \
           "cargo install --git https://github.com/vasovagal/vagus"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.3/vagus-0.1.3-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "a175b27e25565c3fc95a2dfe61f9f629f0136d3df5e467cce3fc12549efaa9cf"
    else
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.3/vagus-0.1.3-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "30bf56eaa974c0cf035e233deeaa5f85bb20fcefd621c1b76e32fcac5dd7c05c"
    end
  end

  def install
    bin.install "vagus"
  end

  test do
    assert_match "vagus #{version}", shell_output("#{bin}/vagus --version")
  end
end
