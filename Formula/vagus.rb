class Vagus < Formula
  desc "Local-first PARA second brain: hybrid search over a Markdown vault"
  homepage "https://github.com/vasovagal/vagus"
  version "0.1.4"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.4/vagus-0.1.4-aarch64-apple-darwin.tar.gz"
      sha256 "9b6a7c900c6d340c966f63a63daea57eade74810aa65638d6d55e7fa0ab8f93a"
    else
      odie "vagus ships only Apple Silicon (arm64) macOS bottles. Build from source: " \
           "cargo install --git https://github.com/vasovagal/vagus"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.4/vagus-0.1.4-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "29012a18c8318f61414d2b15221a1a27004e00099ee47eb6e424e9add8c9bb9a"
    else
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.4/vagus-0.1.4-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "d58801c2e9f8f0cbd659d22c466722bc0f99a4e917543a6e2dffee8afc09fc81"
    end
  end

  def install
    bin.install "vagus"
  end

  test do
    assert_match "vagus #{version}", shell_output("#{bin}/vagus --version")
  end
end
