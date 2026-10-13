#!/usr/bin/env bash
# Installs the codoseo release binary for this runner and puts it on the PATH.
#
# The archives and checksums are the ones .github/workflows/release.yml publishes on each GitHub
# release: codoseo-vX.Y.Z-<target>.tar.gz and codoseo-vX.Y.Z-<target>.tar.gz.sha256. The archive is
# checked against its .sha256 file before anything is extracted.
#
# Inputs (environment): CODOSEO_VERSION (empty, "latest", "0.2.0" or "v0.2.0"), ACTION_REF (the ref
# the action was used at; a vX.Y.Z tag there is the default version), RUNNER_OS, RUNNER_ARCH,
# RUNNER_TEMP, GITHUB_PATH. CODOSEO_REPO overrides the repository (owner/name).
set -euo pipefail

repo="${CODOSEO_REPO:-SafrowLabs/CodoSEO}"

case "${RUNNER_OS:-}/${RUNNER_ARCH:-}" in
  Linux/X64) target=x86_64-unknown-linux-musl; binary=codoseo ;;
  Linux/ARM64) target=aarch64-unknown-linux-musl; binary=codoseo ;;
  macOS/X64) target=x86_64-apple-darwin; binary=codoseo ;;
  macOS/ARM64) target=aarch64-apple-darwin; binary=codoseo ;;
  Windows/X64) target=x86_64-pc-windows-msvc; binary=codoseo.exe ;;
  *)
    echo "::error::CodoSEO publishes no binary for ${RUNNER_OS:-?}/${RUNNER_ARCH:-?} (Linux, macOS: X64, ARM64; Windows: X64)."
    exit 1
    ;;
esac

version="${CODOSEO_VERSION:-}"
# Used at a release tag (SafrowLabs/CodoSEO@v0.2.0), the action installs that release.
if [ -z "$version" ] && [[ "${ACTION_REF:-}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+ ]]; then
  version="$ACTION_REF"
fi
if [ -z "$version" ] || [ "$version" = latest ]; then
  # /releases/latest redirects to the newest release that is not a pre-release.
  effective="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/${repo}/releases/latest")"
  version="${effective##*/}"
fi
tag="v${version#v}"
if ! [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]]; then
  echo "::error::Not a CodoSEO version: '${version}'."
  exit 1
fi

archive="codoseo-${tag}-${target}.tar.gz"
base="https://github.com/${repo}/releases/download/${tag}"
dir="${RUNNER_TEMP:-/tmp}/codoseo-${tag}"
mkdir -p "$dir/bin"
echo "Installing CodoSEO ${tag} (${target})"
for file in "$archive" "$archive.sha256"; do
  if ! curl -fsSL --retry 3 -o "$dir/$file" "$base/$file"; then
    echo "::error::Could not download ${base}/${file}. Does release ${tag} publish binaries? Set the version input to one that does."
    exit 1
  fi
done

# The checksum file is "<sha256>  <archive name>"; both parts must match.
read -r want name < "$dir/$archive.sha256"
if [ "${name#\*}" != "$archive" ] || ! [[ "$want" =~ ^[0-9a-fA-F]{64}$ ]]; then
  echo "::error::The checksum file of ${archive} is not valid."
  exit 1
fi
if command -v sha256sum > /dev/null; then
  got="$(sha256sum "$dir/$archive" | cut -d' ' -f1)"
else
  got="$(shasum -a 256 "$dir/$archive" | cut -d' ' -f1)"
fi
if [ "$(tr 'A-F' 'a-f' <<< "$want")" != "$got" ]; then
  echo "::error::Checksum mismatch for ${archive}: expected ${want}, got ${got}."
  exit 1
fi

tar -xzf "$dir/$archive" -C "$dir/bin" "$binary"
chmod +x "$dir/bin/$binary" 2> /dev/null || true
"$dir/bin/$binary" --version
echo "$dir/bin" >> "${GITHUB_PATH:-/dev/null}"
