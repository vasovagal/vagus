class Vagus < Formula
  desc "Local-first PARA second brain: hybrid search over a Markdown vault"
  homepage "https://github.com/vasovagal/vagus"
  version "0.1.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.0/vagus-0.1.0-aarch64-apple-darwin.tar.gz"
      sha256 "201ccc44212ecf47c12b800f690d01452f264633dfc2f82e4af654e6fecafeb0"
    else
      odie "vagus ships only Apple Silicon (arm64) macOS bottles. Build from source: " \
           "cargo install --git https://github.com/vasovagal/vagus"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.0/vagus-0.1.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "0d0881801c739c83cd8ffeeb37643c902008f5b3f4081bf7014c5755f16e6657"
    else
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.0/vagus-0.1.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "84846d2f78007b9859296d0dc27119e00cae3696b60d0f72b1ed671f722b973a"
    end
  end

  def install
    bin.install "vagus"
  end

  test do
    assert_match "vagus #{version}", shell_output("#{bin}/vagus --version")
  end
end
