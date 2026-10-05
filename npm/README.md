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

Releases are cut by `.github/workflows/release.yml`; see [RELEASING.md](https://github.com/SafrowLabs/CodoSEO/blob/main/RELEASING.md) for the steps. Keep `npm/package.json`, `npm/package-lock.json` and the Rust workspace version in sync; `python3 packaging/check-version.py vX.Y.Z` checks them against the tag. Linux binaries are static musl builds.
