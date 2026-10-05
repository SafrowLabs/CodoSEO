# CodoSEO

CodoSEO is an SEO crawler and site auditor for the command line.

```sh
npm install -g codoseo
codoseo crawl https://example.com
```

Requires Node.js 18 or newer and `tar` on PATH (included with supported Windows, macOS, and typical Linux installations). The installer downloads the matching native binary and SHA-256 checksum from the GitHub release for the package version, verifies the archive, and installs the binary locally. Supported platforms are Linux x64 and arm64 (static musl binaries, so glibc and Alpine both work), macOS x64 and arm64, and Windows x64.

For local development or offline installation, set `CODOSEO_SKIP_DOWNLOAD=1` during installation, then set `CODOSEO_BINARY` to the absolute path of a compiled native binary when running the CLI. Setting `CODOSEO_BINARY` also skips the installer download. Do not point it to the npm launcher.

Project documentation: <https://github.com/SafrowLabs/CodoSEO>

## Releasing

1. Keep `npm/package.json`, `npm/package-lock.json`, and the Rust workspace version in sync. Run `npm ci --ignore-scripts`, `npm audit`, `npm test`, and `npm pack --dry-run` from `npm/`.
2. After merging, push the matching version tag (for example, `v0.0.1`). The release workflow checks the versions, builds and smoke-tests all five targets, and uploads each archive and its `.sha256` sidecar to a GitHub release.
3. Wait for all release jobs to pass. On a supported platform, run `npm install -g ./npm` from the repository and `codoseo --version` to verify the published binary download and checksum.
4. From `npm/`, run `npm publish --access public` using an authorized npm account. Publishing is manual; pushing the Git tag does not publish the npm package.

Do not publish to npm before the matching GitHub release assets are available: users' postinstall downloads would fail. The checksums detect corruption; they are served alongside the archives and are not independent signatures.

## GitHub Actions triggers

- **npm wrapper** runs on branch pushes and pull requests that change `npm/**` or `.github/workflows/npm.yml`. It also supports a manual run.
- **release binaries** validates pull requests changing `npm/**` or its workflow file. It builds and publishes GitHub release assets on `v*` tag pushes. A manual run on a branch builds artifacts only; a manual run on a `v*` tag also publishes the GitHub release assets.
- The existing Rust workspace CI continues to run on pushes and pull requests.

Once these workflows are merged into the default branch, trigger them manually from the repository root:

```sh
gh workflow run npm.yml --ref main
gh workflow run release-binaries.yml --ref main
# To build and publish GitHub release assets for an existing version tag:
gh workflow run release-binaries.yml --ref v0.0.1
```

These workflows do not publish to the npm registry. Use the release steps above for npm publication.
