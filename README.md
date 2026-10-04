# CodoSEO

CodoSEO is an open-source SEO crawler and monitor, written in Rust and licensed under AGPL-3.0. This release is the `codoseo` command line tool. It crawls a site politely, runs 44 SEO checks, gives the site a health score and compares two crawls. The MCP server and the web app are coming later.

## Install

```sh
cargo install codoseo
```

You need Rust 1.88 or newer.

## Usage

```sh
codoseo crawl <URL>              # crawl a site, run the checks, print a report
codoseo check <URL>              # inspect one page: fields, redirect chain, page issues
codoseo robots <URL>             # show robots.txt and whether CodoSEObot may fetch a path
codoseo redirects <URL>          # follow a URL's redirects hop by hop
codoseo diff <BEFORE> <AFTER>    # compare two saved audits
```

These are the main flags:

| Command | Flags |
|---|---|
| `crawl` | `--max-pages N` (default 500), `--max-time SECS` (default 600), `--rps N` (1 to 50, default 5), `--format table\|json\|md\|csv`, `-o FILE`, `--fail-on critical\|warning` |
| `check` | `--format table\|json` |
| `robots` | `--path PATH` (the path to test), `--format table\|json` |
| `redirects` | `--format table\|json` |
| `diff` | `--format table\|json\|md`, `--fail-on critical\|warning` |

Run `codoseo <command> --help` to see every flag.

The crawler identifies itself as `CodoSEObot/0.1 (+https://codoseo.com/bot)`. It obeys robots.txt and `Crawl-delay`, and it slows down when a site answers 429 or 503. Progress goes to stderr, and only when stderr is a terminal.

### Exit codes

- `0`: success.
- `1`: a `--fail-on` threshold was reached.
- `2`: a usage error, or the crawl could not run. A crawl can fail to run because the site is unreachable, the site blocked the crawler, or robots.txt blocks the whole site.

### In CI

```sh
# Fail the build when a critical check fails.
codoseo crawl https://example.com --format json -o audit.json --fail-on critical

# Fail when anything got critically worse since the last saved audit.
codoseo diff previous-audit.json audit.json --fail-on critical
```

If either crawl stopped early, `diff` prints a note, because it does not compare new or removed URLs in that case.

## Audit file format

`crawl --format json` writes an audit: the report, which holds the health score and the failing checks, plus a snapshot of every page that `diff` reads. The top-level `format_version` field is `1`. `diff` refuses files with any other version. Audits written by a later 0.0.x release that adds checks still load, and counts for checks this version does not know are skipped. Hashes such as `url_hash` are unsigned 64-bit integers, so JavaScript and `jq` can lose precision on them. Treat them as opaque IDs.

## License

[AGPL-3.0-only](https://www.gnu.org/licenses/agpl-3.0.html)
