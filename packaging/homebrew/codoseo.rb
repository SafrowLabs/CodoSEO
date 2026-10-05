# Formula template. The release workflow fills in the @...@ placeholders (packaging/render-formula.py)
# and pushes the result to SafrowLabs/homebrew-tap as Formula/codoseo.rb. It installs the prebuilt
# release binaries, so nothing is compiled on the user's machine.
class Codoseo < Formula
  desc "Fast, polite SEO crawler and site auditor"
  homepage "https://codoseo.com"
  version "@VERSION@"
  license "AGPL-3.0-only"

  on_macos do
    on_arm do
      url "https://github.com/SafrowLabs/codoSEO/releases/download/v@VERSION@/codoseo-v@VERSION@-aarch64-apple-darwin.tar.gz"
      sha256 "@SHA256_AARCH64_APPLE_DARWIN@"
    end
    on_intel do
      url "https://github.com/SafrowLabs/codoSEO/releases/download/v@VERSION@/codoseo-v@VERSION@-x86_64-apple-darwin.tar.gz"
      sha256 "@SHA256_X86_64_APPLE_DARWIN@"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/SafrowLabs/codoSEO/releases/download/v@VERSION@/codoseo-v@VERSION@-aarch64-unknown-linux-musl.tar.gz"
      sha256 "@SHA256_AARCH64_LINUX_MUSL@"
    end
    on_intel do
      url "https://github.com/SafrowLabs/codoSEO/releases/download/v@VERSION@/codoseo-v@VERSION@-x86_64-unknown-linux-musl.tar.gz"
      sha256 "@SHA256_X86_64_LINUX_MUSL@"
    end
  end

  def install
    bin.install "codoseo"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/codoseo --version")
  end
end
