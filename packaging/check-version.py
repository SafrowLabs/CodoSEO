#!/usr/bin/env python3
"""Release version gate. Usage: check-version.py [TAG]

Prints the release version on stdout and exits 0 when everything agrees:
  * the workspace version in Cargo.toml (every crate inherits it),
  * the version on each internal `codoseo-*` entry of [workspace.dependencies] (cargo publish
    needs them to match the version being published),
  * npm/package.json,
  * the tag, when one is given (it must be `v` + that version).
Anything else prints the mismatches on stderr and exits 1. Run from anywhere in the repo.
"""
import json
import pathlib
import sys
import tomllib

root = pathlib.Path(__file__).resolve().parent.parent
cargo = tomllib.loads((root / "Cargo.toml").read_text())
version = cargo["workspace"]["package"]["version"]
problems = []

for name, spec in cargo["workspace"]["dependencies"].items():
    if name.startswith("codoseo-") and isinstance(spec, dict) and "version" in spec:
        if spec["version"] != version:
            problems.append(f"workspace dependency {name} is {spec['version']}, workspace version is {version}")

npm_version = json.loads((root / "npm" / "package.json").read_text())["version"]
if npm_version != version:
    problems.append(f"npm/package.json is {npm_version}, workspace version is {version}")

if len(sys.argv) > 1 and sys.argv[1] and sys.argv[1] != f"v{version}":
    problems.append(f"tag {sys.argv[1]} is not v{version}")

if problems:
    print("\n".join(problems), file=sys.stderr)
    sys.exit(1)
print(version)
