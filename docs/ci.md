# CodoSEO in CI

`codoseo crawl` is built to gate a deploy: it fails the build on the SEO and GEO (AI access) regressions you name, compares with the audit of an earlier run so only new problems fail it, and writes SARIF for GitHub code scanning. The GitHub Action in this repository wraps all of that.

- [Exit codes](#exit-codes)
- [`--fail-on`](#--fail-on)
- [`--baseline`](#--baseline)
- [Output formats and `--save`](#output-formats-and---save)
- [SARIF](#sarif)
- [GitHub Action](#github-action)
- [Other CI systems](#other-ci-systems)

## Exit codes

| Code | Meaning |
|---|---|
| `0` | The crawl ran and no `--fail-on` gate failed. |
| `1` | A `--fail-on` gate failed. |
| `2` | Usage error, unreadable `--baseline`, or the crawl could not run (site unreachable, crawler blocked, robots.txt disallows the whole site, robots.txt answers 5xx or 429). |

One exception to the last row: a robots.txt that answers 5xx or 429 stops the crawl, but when a gate covers `robots_unavailable` (`geo`, `geo:critical` or `robots_unavailable` itself) finding it is the gate's job, so the exit code is `1`.

## `--fail-on`

Comma-separated, or repeat the flag. Any gate that fails makes the exit code `1`.

| Value | Fails when |
|---|---|
| `critical` | An SEO check of severity critical fails. |
| `warning` | An SEO check of severity warning or critical fails. |
| `geo` | The GEO gates for CI, listed below. |
| `geo:critical`, `geo:warning`, `geo:notice` | An AI access finding at that severity or worse. |
| `<kind>` | An AI access finding of that kind: `robots_unavailable`, `bots_blocked`, `bots_not_blocked`, `answers_restricted`, `fetch_blocked`, `content_missing`, `llms_txt_unavailable`. |
| `<kind>:<subject>` | One subject of a kind. `bots_blocked` and `bots_not_blocked` take a purpose (`search`, `user_fetch`, `agent`, `training`, `ads`); `answers_restricted` a directive (`noindex`, `nosnippet`, `max_snippet`, `noarchive`, `nocache`, `data_nosnippet`); `fetch_blocked` a cause (`challenge_cloudflare`, `blocked_403`, ...). |

`geo` covers the four regressions that take a site out of AI answers:

| Regression | Finding |
|---|---|
| robots.txt answers 5xx or 429 | `robots_unavailable` |
| An AI search or user-triggered bot you want is newly blocked by robots.txt | `bots_blocked` |
| `nosnippet` or `noarchive` added to important pages | `answers_restricted:nosnippet`, `answers_restricted:noarchive` |
| Raw HTML content missing: important pages are an empty app shell before JavaScript runs | `content_missing` |

The AI access checks are judged under the default intent: AI search and user-triggered bots are wanted, training, agent and ads bots are your call (never reported either way). They never touch the health score; a gate only decides the exit code. `notice` is not an SEO gate: there are too many notices to fail a build on. The `fetch_blocked` finding needs `--probes` (a synthetic fetch of up to 5 pages as each AI bot), which is off by default.

```sh
codoseo crawl https://example.com --fail-on critical,geo
codoseo crawl https://example.com --fail-on warning --fail-on bots_blocked:search
```

After the report, `crawl` prints on stderr what each gate made of the run:

```text
fail-on critical,geo: 1 result failed against the baseline
  Critical  bots_blocked:search  OAI-SearchBot is blocked by robots.txt (new since the baseline)
```

## `--baseline`

`--baseline AUDIT.json` takes an audit saved by an earlier run (`--format json`, or `--save audit.json`). The gates then count only what is new or worse since it:

- **SEO**: a page that fails a check it passed in the baseline, a page the baseline did not have (only when the baseline crawl was complete; an incomplete one may not have reached it), a site-wide check that newly fails, and the changes [`codoseo diff`](../README.md#catch-regressions-between-deploys) reports (status turned into an error, became noindex, title removed, robots.txt rules changed, sitemap shrank, error spike, ...) at the gate's severity.
- **AI access**: a finding that is new, more severe than in the baseline, or that now names a bot or engine it did not. These are the rules the web app uses to open, escalate and widen incidents.

A baseline saved before audits had an AI access section makes every AI access finding new. Without `--baseline` every result counts.

The usual setup: keep the audit of each run on your main branch, and pass it as the baseline when you check a pull request or a deploy preview.

## Output formats and `--save`

`--format` picks what goes to stdout (or `-o FILE`); `--save FILE` writes the same report again in the format the extension names, and can be repeated, so one crawl gives you every file you need.

| Format | `--save` extension | Use |
|---|---|---|
| `table` | `.txt` | Reading in a terminal. |
| `json` | `.json` | The audit file: the next run's `--baseline`, `codoseo diff`, your own tooling. |
| `md` | `.md` | A job summary or a pull request comment. |
| `csv` | `.csv` | One row per page. |
| `sarif` | `.sarif` | GitHub code scanning and other SARIF readers ([below](#sarif)). |
| `github` | | GitHub Actions annotations: `::error` for each result that failed a gate, `::warning` for other critical and warning results, `::notice` for notices, then a short summary. |

```sh
codoseo crawl https://example.com --format github --fail-on critical,geo \
  --baseline baseline.json --save codoseo.sarif --save audit.json --save summary.md
```

## SARIF

`--format sarif` (or `--save codoseo.sarif`) writes [SARIF 2.1.0](https://docs.oasis-open.org/sarif/sarif/v2.1.0/sarif-v2.1.0.html). A test validates the output against the [published schema](https://json.schemastore.org/sarif-2.1.0.json) on every build.

- **One run**, tool `CodoSEO`, with **one rule per check**: `seo/<slug>` for the 44 SEO checks and `geo/<kind>` for the 7 AI & GEO checks, always all 51 so rule indexes never move. Each rule has a short and full description, help text (Markdown too), a `helpUri`, its default level and `properties` (`category` and `severity` for SEO; `evidenceGrade` and `sources` for AI access).
- **Results**: one per page for a failing page check (at most 100 per rule; the run's `properties.omitted` counts the rest), one for a failing site-wide check, one per AI access finding. Each AI access result carries its `evidenceGrade`, `subject`, `sources`, `affectedUrls` and the `gate` value that selects it in `properties`, and its fix in the message.
- **Level** comes from the severity: critical `error`, warning `warning`, notice `note`.
- **Locations** are the page URLs. A site has no source files, and GitHub rejects absolute `https:` URIs next to its file source root, so each `artifactLocation.uri` is the page's path relative to the `SITE` base (`uriBaseId: "SITE"`), and `originalUriBaseIds.SITE` is the site's origin: `about/team` under `https://example.com/` is `https://example.com/about/team`. The home page is `./`; robots.txt findings point at `robots.txt`, llms.txt findings at `llms.txt`.
- **`partialFingerprints`** (`primaryLocationLineHash` and `codoseo/v1`) are stable per rule and URL (per rule, subject and site for AI access), so code scanning tracks an alert across runs and closes it when the result goes away.
- **`baselineState`** (`new`, `updated`, `unchanged`) is set on every result when `--baseline` is given.

In GitHub code scanning the alerts show with the page path as their "file", without a code preview, because the path is not a file in your repository.

## GitHub Action

`action.yml` at the root of this repository is a composite action. It installs the released `codoseo` binary for the runner (Linux and macOS on x64 and arm64, Windows on x64), checks the archive against the `.sha256` file the release publishes, then runs `codoseo crawl` with annotations, a SARIF file, the audit and a job summary.

| Input | Default | |
|---|---|---|
| `url` | required | The start URL, for example a preview deployment. |
| `fail-on` | `critical,geo` | As [`--fail-on`](#--fail-on). Empty: never fail on results. |
| `baseline` | | An audit from an earlier run. A path that does not exist (the first run) is skipped with a notice. |
| `sarif` | `codoseo.sarif` | Where to write SARIF. |
| `audit` | `codoseo-audit.json` | Where to write the audit, the next run's baseline. |
| `max-pages` | `500` | Most pages to crawl. |
| `max-time` | `600` | Longest crawl, in seconds. |
| `rps` | `5` | Requests per second. |
| `version` | | The release to install (`0.2.0`, `v0.2.0` or `latest`). Empty: the release the action is used at when that is a version tag (`SafrowLabs/CodoSEO@v0.2.0`), else the latest release. |
| `args` | | Extra `crawl` flags, separated by spaces (for example `--probes`). |

Outputs: `exit-code` (`0`, `1` or `2`), `sarif` and `audit` (the paths). The step fails with the CLI's exit code.

The gates, `--baseline`, `--save` and the `sarif` and `github` formats are new in the release after `0.1.0-rc.1`; earlier releases can't run the action. Set `version` to a release that has them.

[`examples/github-action/codoseo.yml`](../examples/github-action/codoseo.yml) is a complete workflow: it audits on every push to `main` and keeps the audit as the baseline (in the Actions cache), audits every pull request against it, uploads the SARIF with `github/codeql-action/upload-sarif` (even when the gate failed, so the alerts show on the pull request) and keeps the reports as an artifact. Every third-party action in it is pinned by commit SHA. The upload needs `permissions: security-events: write`; code scanning is free on public repositories and needs GitHub Code Security on private ones.

```yaml
- uses: SafrowLabs/CodoSEO@<release commit SHA> # vX.Y.Z
  with:
    url: https://example.com/
    fail-on: critical,geo
    baseline: codoseo-baseline.json
- uses: github/codeql-action/upload-sarif@24c54180a607b1449ed407dd24f251e4e9147c8d # v4.38.3
  if: always()
  with:
    sarif_file: codoseo.sarif
    category: codoseo
```

## Other CI systems

Anything that runs a binary and reads an exit code works. Install from a [release](https://github.com/SafrowLabs/CodoSEO/releases), `npm i -g codoseo`, `brew install safrowlabs/codoseo/codoseo`, `cargo install codoseo` or the `ghcr.io/safrowlabs/codoseo` image, then:

```sh
codoseo crawl "$SITE_URL" --fail-on critical,geo --baseline baseline.json \
  --save codoseo.sarif --save audit.json
```

GitLab reads SARIF only through its own converters; its code quality report is a different format, so use the table or Markdown output there.
