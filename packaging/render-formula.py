#!/usr/bin/env python3
"""Render the Homebrew formula. Usage: render-formula.py VERSION ASSETS_DIR

ASSETS_DIR holds the release's `codoseo-vVERSION-<target>.tar.gz.sha256` files. The filled-in
formula goes to stdout; a missing checksum file is an error.
"""
import pathlib
import re
import sys

version, assets = sys.argv[1], pathlib.Path(sys.argv[2])
template = (pathlib.Path(__file__).resolve().parent / "homebrew" / "codoseo.rb").read_text()
targets = {
    "AARCH64_APPLE_DARWIN": "aarch64-apple-darwin",
    "X86_64_APPLE_DARWIN": "x86_64-apple-darwin",
    "AARCH64_LINUX_MUSL": "aarch64-unknown-linux-musl",
    "X86_64_LINUX_MUSL": "x86_64-unknown-linux-musl",
}
out = template.replace("@VERSION@", version)
for key, target in targets.items():
    name = f"codoseo-v{version}-{target}.tar.gz"
    digest = (assets / f"{name}.sha256").read_text().split()[0]
    if len(digest) != 64:
        sys.exit(f"bad checksum in {name}.sha256")
    out = out.replace(f"@SHA256_{key}@", digest)
if re.search(r"@[A-Z0-9_]+@", out):
    sys.exit("unfilled placeholder left in the formula")
sys.stdout.write(out)
