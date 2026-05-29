class Vagus < Formula
  desc "Local-first PARA second brain: hybrid search over a Markdown vault"
  homepage "https://github.com/vasovagal/vagus"
  version "0.1.1"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.1/vagus-0.1.1-aarch64-apple-darwin.tar.gz"
      sha256 "a308dbbc576ff6e245a8bc72cbc35b0b59ff80c21160e27f592b390a1b42f55f"
    else
      odie "vagus ships only Apple Silicon (arm64) macOS bottles. Build from source: " \
           "cargo install --git https://github.com/vasovagal/vagus"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.1/vagus-0.1.1-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "5883df1066f529a0d390616d39757cc8e9defe2dd94b0a2ebf12d060c05071a5"
    else
      url "https://github.com/vasovagal/vagus/releases/download/v0.1.1/vagus-0.1.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "9be94a0e35c96625e68b30b9db092f77d98b1e4902d8a281208add866066aaf3"
    end
  end

  def install
    bin.install "vagus"
  end

  test do
    assert_match "vagus #{version}", shell_output("#{bin}/vagus --version")
  end
end
