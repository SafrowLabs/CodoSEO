# CodoSEO — Open-Source SEO Crawler & Site Audit CLI

[![crates.io](https://img.shields.io/crates/v/codoseo)](https://crates.io/crates/codoseo)
[![Downloads](https://img.shields.io/crates/d/codoseo)](https://crates.io/crates/codoseo)
[![License: AGPL-3.0](https://img.shields.io/crates/l/codoseo)](https://www.gnu.org/licenses/agpl-3.0.html)
[![CI](https://github.com/SafrowLabs/codoSEO/actions/workflows/ci.yml/badge.svg)](https://github.com/SafrowLabs/codoSEO/actions/workflows/ci.yml)

**CodoSEO** is a fast, polite, open-source SEO crawler and technical site-audit tool written in Rust. It crawls a site, runs **44 SEO checks** across response health, indexability, on-page factors, content, links, schema and more, scores the result 0–100, and can diff two audits to catch regressions in CI.

- Crawls up to 500 pages by default; configurable to any limit
- Respects `robots.txt`, `Crawl-delay`, and backs off on `429`/`503`
- Outputs table, JSON, Markdown or CSV — pipe-friendly and CI-ready
- Ships a local [MCP](https://modelcontextprotocol.io) server (`codoseo mcp`) for AI agents like Claude Code
- Single static binary; no runtime dependencies
- AGPL-3.0 — free to self-host, source-available

---

## Install

```sh
cargo install codoseo
```

Requires **Rust 1.88** or newer. Binaries for Linux, macOS and Windows are on the [releases page](https://github.com/SafrowLabs/codoSEO/releases).

---

## Quick start

```sh
# Audit a site and print a table report
codoseo crawl https://example.com

# Save as JSON for diffing later
codoseo crawl https://example.com --format json -o audit.json

# Inspect one page
codoseo check https://example.com/about

# Compare two audits
codoseo diff baseline.json audit.json
```

**Example report (Markdown output):**

```
# CodoSEO report for https://example.com/

- **Health score:** 84 / 100
- **Checks:** 37 of 44 checks passed
- **Stop reason:** Completed

## Critical (1)

| Check                        | Slug       | Affected | Examples                        |
|------------------------------|------------|----------|---------------------------------|
| Page returns a 4xx error     | http_4xx   | 1 page   | https://example.com/old-page   |

## Warning (2)

| Check                        | Slug              | Affected | Examples                        |
|------------------------------|-------------------|----------|---------------------------------|
| Page links to a broken URL   | links_to_broken   | 1 page   | https://example.com/contact    |
| Page is served over HTTP     | not_https         | 3 pages  | https://example.com/ …         |

## Summary

- **Pages:** 142 (138 indexable)
- **Status:** 2xx 138 · 3xx 3 · 4xx 1 · 5xx 0
- **Click depth:** 0: 1 · 1: 12 · 2: 87 · 3: 42
- **Avg response:** 210 ms
- **Duration:** 28 s
```

---

## Commands

| Command | Description |
|---|---|
| `codoseo crawl <URL>` | Crawl a site, run all 44 checks, print a report |
| `codoseo check <URL>` | Inspect one page: fields, redirect chain, page-level issues |
| `codoseo robots <URL>` | Show the site's `robots.txt` and test whether CodoSEObot may fetch a path |
| `codoseo redirects <URL>` | Follow a URL's redirect chain hop by hop |
| `codoseo diff <BEFORE> <AFTER>` | Compare two saved JSON audits; surface new issues and regressions |
| `codoseo mcp` | Run the local MCP server over stdio, for AI agents (see [Local MCP](#local-mcp)) |
| `codoseo web` | Serve the web app (see [Web app and self-hosting](#web-app-and-self-hosting)) |
| `codoseo worker` | Claim and run crawls from the Postgres queue |
| `codoseo all` | Self-hosting in one process: apply migrations, then run the web app and a worker |
| `codoseo migrate` | Apply pending Postgres migrations |

### Flags

**`crawl`**

| Flag | Default | Description |
|---|---|---|
| `--max-pages N` | 500 | Stop after N pages |
| `--max-time SECS` | 600 | Stop after N seconds |
| `--rps N` | 5 | Requests per second (1–50) |
| `--format table\|json\|md\|csv` | table | Output format |
| `-o FILE` | — | Write output to a file |
| `--fail-on critical\|warning` | — | Exit 1 if threshold is reached |

**`check`**

| Flag | Default | Description |
|---|---|---|
| `--format table\|json` | table | Output format |

**`robots`**

| Flag | Default | Description |
|---|---|---|
| `--path PATH` | — | Test whether this path is allowed |
| `--format table\|json` | table | Output format |

**`redirects`**

| Flag | Default | Description |
|---|---|---|
| `--format table\|json` | table | Output format |

**`diff`**

| Flag | Default | Description |
|---|---|---|
| `--format table\|json\|md` | table | Output format |
| `--fail-on critical\|warning` | — | Exit 1 if threshold is reached |

Run `codoseo <command> --help` for the full flag list.

---

## The 44 SEO checks

Every crawled page is evaluated against these checks. Severity: **C** = Critical · **W** = Warning · **N** = Notice.

### Response

| Check | Severity | Slug |
|---|---|---|
| Page returns a 4xx error | C | `http_4xx` |
| Page returns a 5xx error | C | `http_5xx` |
| Page could not be fetched | C | `fetch_failed` |
| Redirect loop or too many redirects | C | `redirect_loop` |
| Redirect chain has 2 or more hops | W | `redirect_chain` |
| URL redirects to another URL | N | `redirected` |

### Indexability

| Check | Severity | Slug |
|---|---|---|
| robots.txt blocks the whole site | C | `robots_blocks_site` |
| Page is blocked by robots.txt | W | `blocked_by_robots` |
| Canonical URL does not return 200 | W | `canonical_to_non200` |
| Page is set to noindex | N | `noindex` |
| Page points to a different canonical URL | N | `canonicalised` |
| Canonical URL is missing | N | `canonical_missing` |

### On-Page

| Check | Severity | Slug |
|---|---|---|
| Title is missing | W | `title_missing` |
| Page has more than one title | W | `title_multiple` |
| Title is used on other pages | W | `title_duplicate` |
| Meta description is missing | W | `description_missing` |
| Meta description is used on other pages | W | `description_duplicate` |
| H1 heading is missing | W | `h1_missing` |
| Title is over 60 characters | N | `title_too_long` |
| Title is under 30 characters | N | `title_too_short` |
| Meta description is over 160 characters | N | `description_too_long` |
| Meta description is under 70 characters | N | `description_too_short` |
| Page has more than one H1 | N | `h1_multiple` |
| H1 is used on other pages | N | `h1_duplicate` |

### Content

| Check | Severity | Slug |
|---|---|---|
| Page content duplicates another page | W | `content_duplicate` |
| Page has under 200 words | N | `thin_content` |
| Images are missing alt text | N | `images_missing_alt` |

### Links

| Check | Severity | Slug |
|---|---|---|
| Page links to a broken URL | W | `links_to_broken` |
| Page has no internal links pointing to it | W | `orphan` |
| Page links to a redirecting URL | N | `links_to_redirect` |
| Page has no internal links out | N | `no_internal_outlinks` |
| Page has nofollow internal links | N | `nofollow_internal_links` |
| Page is more than 3 clicks from the homepage | N | `deep_page` |

### Technical

| Check | Severity | Slug |
|---|---|---|
| HTTPS page loads HTTP resources | W | `mixed_content` |
| Page is served over HTTP | W | `not_https` |
| Response took over 1 second | N | `slow_response` |

### Sitemap

| Check | Severity | Slug |
|---|---|---|
| Page is in the sitemap but not 200 | W | `sitemap_non200` |
| Page is in the sitemap but noindex | W | `sitemap_noindex` |
| Indexable page is not in the sitemap | N | `not_in_sitemap` |
| Page is in the sitemap but canonicalised | N | `sitemap_canonicalised` |
| Site has no sitemap | N | `sitemap_missing` |

### Social & Schema

| Check | Severity | Slug |
|---|---|---|
| JSON-LD does not parse | W | `jsonld_invalid` |
| Open Graph title or image is missing | N | `og_missing` |
| Hreflang has no entry for the page itself | N | `hreflang_missing_self` |

---

## CI integration

### Fail on critical issues

```sh
codoseo crawl https://example.com --format json -o audit.json --fail-on critical
```

Exit codes: `0` success · `1` fail-on threshold reached · `2` usage error or crawl could not start (unreachable, blocked, or `robots.txt` disallows the whole site).

### Catch regressions between deploys

```sh
# Save a baseline before your deploy
codoseo crawl https://example.com --format json -o before.json

# After deploy, audit again and diff
codoseo crawl https://example.com --format json -o after.json
codoseo diff before.json after.json --fail-on critical
```

`diff` reports new and resolved issues, score changes, and URL-level changes (added, removed, status change, title change, redirect chain changes, robots rule changes). If either crawl stopped early, `diff` notes that and skips new/removed URL comparisons.

### GitHub Actions example

```yaml
- name: SEO audit
  run: |
    cargo install codoseo
    codoseo crawl ${{ env.SITE_URL }} --format json -o audit.json --fail-on critical
```

---

## Audit file format

`crawl --format json` writes an **audit file**: the full report (health score, per-check counts, failing examples) plus a snapshot of every crawled page. `diff` reads two audit files and computes the delta.

- `format_version`: `1` — `diff` rejects any other version
- Audits written by a later `0.0.x` release that adds new checks still load; counts for unknown checks are skipped
- `url_hash` and similar fields are unsigned 64-bit integers — treat them as opaque IDs; JavaScript and `jq` can lose precision on them

### Health score

The score is 0–100. Each check carries a weight proportional to its severity (critical > warning > notice) and the fraction of affected pages. A site where every page fails every check scores 0; a site with no failures scores 100.

---

## Crawler behaviour

CodoSEO identifies itself as `CodoSEObot/0.1 (+https://codoseo.com/bot)`. It:

- Fetches and obeys `robots.txt` before starting the crawl
- Respects the `Crawl-delay` directive
- Slows down automatically on `429 Too Many Requests` and `503 Service Unavailable`
- Refuses to crawl private/internal IP ranges (SSRF protection)
- Caps HTML bodies at 5 MB and does not download non-HTML resources
- Writes crawl progress to stderr, only when stderr is a terminal

---

## Local MCP

`codoseo mcp` runs a local [MCP](https://modelcontextprotocol.io) server over stdio, so an AI agent can crawl and audit sites directly from your machine — no account, no cloud calls, and (unlike the hosted version) private and internal addresses are allowed.

Add it to Claude Code:

```sh
claude mcp add codoseo -- codoseo mcp
```

Or in any MCP-compatible client, point it at `codoseo mcp` as a stdio server.

**Tools:**

| Tool | Description |
|---|---|
| `audit_site` | Crawl a site and run the checks; returns the summary if it finishes quickly, otherwise a running `audit_id` to poll |
| `get_audit` | Check an audit's progress or summary by id |
| `get_issue_urls` | List the URLs affected by one failing check, paginated |
| `get_page` | Get one page's full record from a finished audit |
| `check_page` | Fetch and check one page right now, without a full crawl |
| `check_robots` | Show a site's `robots.txt` and whether CodoSEObot may fetch a path |
| `check_redirects` | Follow a URL's redirects hop by hop |
| `compare_audits` | Compare two finished audits and list what changed |

Audits are cached as JSON under your user cache directory (`~/Library/Caches/codoseo/audits/` on macOS, `~/.cache/codoseo/audits/` on Linux) so `get_audit`, `get_issue_urls`, `get_page` and `compare_audits` can be called after `audit_site` returns.

---

## Web app and self-hosting

The web app lets you add sites, run crawls, and work through them in a URL explorer (filters for status, indexability, content type and every check; a SERP preview with pixel-width meters; inlinks; reconstructed headers), a site audit with your health score and issues, and a changes view that compares each crawl with the one before. It runs on Postgres and needs nothing else: no Redis, no outside requests (fonts and scripts are bundled).

Run everything in one process:

```sh
createdb codoseo
DATABASE_URL=postgres://localhost/codoseo codoseo all
# open http://localhost:8080 — the first account to sign in becomes the owner
```

Sign-in uses magic links. Without an email server the link is printed to the server log; set `GITHUB_CLIENT_ID` / `GITHUB_CLIENT_SECRET` to add "Continue with GitHub". The owner can close signups from **Account**.

| Variable | Default | Meaning |
|---|---|---|
| `DATABASE_URL` | — | Postgres connection string (required) |
| `CODOSEO_MODE` | `selfhost` | `selfhost` or `cloud` (cloud turns on plan limits) |
| `BASE_URL` | `http://localhost:<port>` | Public address, used for links, cookies and the `Origin` check on every form post |
| `CODOSEO_BIND` | `0.0.0.0:8080` | Listen address (`--bind` overrides it) |
| `SECRET_KEY` | dev key | Required in cloud mode |
| `GITHUB_CLIENT_ID`, `GITHUB_CLIENT_SECRET` | — | Optional GitHub sign-in (callback: `<BASE_URL>/auth/github/callback`) |

For larger setups run `codoseo web` and one or more `codoseo worker` processes against the same database, after `codoseo migrate`. Health checks: `/healthz` (process up) and `/readyz` (database reachable).

Keyboard: <kbd>⌘K</kbd> command palette and URL search, <kbd>G</kbd> then <kbd>E</kbd>/<kbd>A</kbd>/<kbd>C</kbd>/<kbd>H</kbd> to switch screens, <kbd>J</kbd>/<kbd>K</kbd> to move through rows, <kbd>/</kbd> to filter, <kbd>[</kbd> to collapse the sidebar, <kbd>?</kbd> for the full list.

---

## Project layout

The project is a Cargo workspace:

```
crates/
  core/       — shared types: URLs, page records, check IDs, audit format
  crawler/    — fetcher, robots.txt, sitemaps, politeness, crawl loop
  checks/     — the 44 check definitions, scoring, site-wide analysis
  diff/       — audit comparison engine
  mcp/        — local MCP server: the 8 tools above, over rmcp
  store/      — Postgres: migrations, crawl and jobs queues, queries for the web app
  web/        — the web app: axum routes, askama templates, htmx, bundled assets
  app/        — CLI (the `codoseo` binary)
  testkit/    — shared test helpers (not published to crates.io)
```

Each crate is published to [crates.io](https://crates.io/crates/codoseo) independently so you can embed the crawler or checks in your own Rust project.

---

## Contributing

Issues and pull requests are welcome. Run the full test suite with:

```sh
cargo test
```

---

## License

[AGPL-3.0-only](https://www.gnu.org/licenses/agpl-3.0.html). The MCP server and web app are coming later — see [codoseo.com](https://codoseo.com).
