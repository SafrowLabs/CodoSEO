# Releasing

One workflow, `.github/workflows/release.yml`, does everything. It runs on a `v*` tag push, by hand
(`workflow_dispatch`), and on pull requests that touch the release machinery (workflow, `packaging/`,
`npm/`, `Dockerfile`, Cargo manifests).

## What a release produces

| Output | Where |
| --- | --- |
| `codoseo-vX.Y.Z-<target>.tar.gz` + `.sha256` for `x86_64`/`aarch64` Linux (static musl), `x86_64`/`aarch64` macOS and `x86_64` Windows, plus `SHA256SUMS` | GitHub release |
| `ghcr.io/safrowlabs/codoseo:X.Y.Z`, `:X.Y`, `:latest` (amd64 + arm64, one manifest) | ghcr.io |
| `codoseo` and its library crates | crates.io |
| `codoseo` npm package (downloads the release binary) | npm |
| `Formula/codoseo.rb` (installs the release binary) | `SafrowLabs/homebrew-tap` |

Binaries and the image are built from the same pinned toolchain (`rust:1.99.0-alpine`, `--profile dist`).

## Secrets

Set these as repository secrets. `GITHUB_TOKEN` covers the GitHub release and ghcr.io.

| Secret | Used for |
| --- | --- |
| `CARGO_REGISTRY_TOKEN` | `cargo publish`. A crates.io token with publish-new and publish-update scopes. |
| `NPM_TOKEN` | `npm publish --provenance`. An automation or granular token with publish rights on `codoseo`. |
| `HOMEBREW_TAP_TOKEN` | Push to `SafrowLabs/homebrew-tap` (fine-grained token, contents: write on that repo only). |

A publish job whose secret is missing is skipped with a notice; the rest of the release carries on.

## Before tagging

1. Set the same version in three places: `[workspace.package] version` and every `codoseo-*` entry in
   `[workspace.dependencies]` of `Cargo.toml`, plus `npm/package.json` (and `npm/package-lock.json`).
   Refresh `Cargo.lock` (`cargo update -w`).
2. `python3 packaging/check-version.py vX.Y.Z` must print the version. The release workflow runs the
   same script and fails before building anything if the tag, the workspace version or the npm
   version disagree.
3. Merge to `main` with CI green (`ci.yml`: fmt, clippy, tests, cargo-deny, container build).

## Dry run

Actions, "release", "Run workflow", on the branch you want to check, leave `dry_run` ticked. It builds
all five binaries and both image architectures (nothing is pushed), then runs
`cargo publish --workspace --dry-run`, `npm test` + `npm pack --dry-run` and renders the Homebrew
formula. Nothing is published. Opening a pull request that touches the release files does the same.

## Release candidate

Tag `vX.Y.Z-rc.N` (the version in the manifests must be `X.Y.Z-rc.N`). A tag containing `-` is a
prerelease:

- GitHub release marked as prerelease.
- Image tagged `X.Y.Z-rc.N` only (no `X.Y`, no `latest`).
- npm published under the `next` dist-tag, so `npm install codoseo` still gets the last stable.
- crates.io and Homebrew are skipped.

Use it to check the install paths: `npx codoseo@next`, `docker run ghcr.io/safrowlabs/codoseo:X.Y.Z-rc.N`.

## Final release

```sh
git tag vX.Y.Z && git push origin vX.Y.Z
```

Order: version gate, binaries and image in parallel, GitHub release, then crates.io, npm and the
Homebrew tap (they need the release because the npm installer and the formula download from it).
Check afterwards: `brew install safrowlabs/tap/codoseo && codoseo --version`, `npx codoseo --version`,
`cargo install codoseo`.

Re-running a failed release: use "Re-run failed jobs" on the same run. `cargo publish` refuses crates
that already exist, so if it failed part-way, publish the remaining crates by hand with
`cargo publish -p <crate>` in dependency order (core, checks, crawler, diff, mcp, notify, store, web,
codoseo) and re-run the other jobs. Published crates.io and npm versions cannot be replaced, only
yanked or deprecated; fix forward with a new patch version.

## Nightly

`nightly.yml` runs the 50,000-page crawl memory test (`crates/crawler/tests/crawl_memory.rs`, ignored
in normal runs; locally: `cargo test --release -p codoseo-crawler --test crawl_memory -- --ignored
--nocapture`, `CODOSEO_MEMORY_PAGES` lowers the count) and `cargo deny check`.
