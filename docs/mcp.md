# MCP

CodoSEO speaks the [Model Context Protocol](https://modelcontextprotocol.io) in two ways: a **local** server you run on your machine over stdio, and a **hosted** server at `https://codoseo.com/mcp` (any self-hosted web app serves the same endpoint at `<BASE_URL>/mcp`).

## Local server (stdio)

`codoseo mcp` needs no account and makes no cloud calls. It crawls directly from your machine, and private and internal addresses are allowed.

### Claude Code

```sh
claude mcp add codoseo -- codoseo mcp
```

If you have not installed the binary, let npm fetch it:

```sh
claude mcp add codoseo -- npx -y codoseo mcp
```

### Cursor, Claude Desktop and other JSON-configured clients

Add a stdio server to the client's MCP config (for Cursor, `.cursor/mcp.json` in a project or `~/.cursor/mcp.json` globally):

```json
{
  "mcpServers": {
    "codoseo": {
      "command": "codoseo",
      "args": ["mcp"]
    }
  }
}
```

With npx instead of an installed binary, use `"command": "npx", "args": ["-y", "codoseo", "mcp"]`.

### Tools

| Tool | Arguments | What it does |
|---|---|---|
| `audit_site` | `url`, `max_pages` (default 500) | Crawls a site and runs the checks. Returns the summary if the crawl finishes while it waits (about 50 seconds), otherwise `{"status":"running","audit_id":...}`. |
| `get_audit` | `audit_id` | An audit's progress, or its summary once finished. |
| `get_issue_urls` | `audit_id`, `check`, `limit` (50), `offset` (0) | The URLs that fail one check, paginated. |
| `get_page` | `audit_id`, `url` | One page's full record from a finished audit. |
| `check_page` | `url` | Fetches and checks one page now, without a full crawl. |
| `check_robots` | `url`, `path` | The site's `robots.txt` and whether CodoSEObot may fetch the path. |
| `check_ai_access` | `url` | Fetches the site's `robots.txt` and the page now: per AI bot whether the path is allowed (with the rule line), the declared Content-Signal and Content-Usage preferences, and the page's effect per AI engine; `content` is what the page holds in its HTML as served (`shell`, `likely_shell`, `thin` or `present`, with the words and shell signals), since AI crawlers like GPTBot, ClaudeBot and PerplexityBot don't run JavaScript. See [ai-access.md](ai-access.md). |
| `probe_ai_fetch` | `url`, `bots` (optional token list) | Synthetic check: fetches the page now as each AI search and user-triggered bot with its documented user agent and compares each response with a CodoSEObot baseline, to catch a CDN or firewall that blocks, challenges or redirects AI bots robots.txt lets in. Returns a summary starting "Synthetic check:", each bot's verdict and the `fetch_blocked` findings. The requests come from your machine, so real bots may be treated differently. |
| `check_llms_txt` | `url` | Informational: fetches the site's `/llms.txt` after its robots.txt and checks it against the llmstxt.org format and Lighthouse's audit: its `state` (`available`, `absent`, `soft_not_found` or `unavailable`), whether the site has adopted it, each check and how many of its links answer. Never scored: a missing llms.txt is nothing to fix. It fails with a message when robots.txt disallows `/llms.txt` for CodoSEObot. |
| `check_redirects` | `url` | Follows a URL's redirects hop by hop. |
| `compare_audits` | `audit_a`, `audit_b` | Compares two finished audits and lists what changed. |

Audits are cached as JSON in your user cache directory (`~/.cache/codoseo/audits/` on Linux, `~/Library/Caches/codoseo/audits/` on macOS), in the format `codoseo crawl --format json` writes, AI access section included (judged under the default intent), so `get_audit`, `get_issue_urls`, `get_page` and `compare_audits` work after `audit_site` returns. The server writes only protocol messages to stdout.

## Hosted server (HTTP)

The endpoint is `https://codoseo.com/mcp` (streamable HTTP, stateless, JSON responses). Authentication is `Authorization: Bearer <key>` and nothing else; cookies are ignored. One endpoint serves two tiers, chosen by whether the request carries a key.

### Without a key (free, public sites only)

claude.ai: Settings, Connectors, add a custom connector with the URL `https://codoseo.com/mcp`. Claude Code:

```sh
claude mcp add --transport http codoseo https://codoseo.com/mcp
```

| Tool | What it does |
|---|---|
| `quick_audit` | Audits a public site: crawls up to 100 pages, returns the health score, failing checks with example URLs and a report link. If it is not done in about 45 seconds it returns `{"status":"running","audit_id":...}`. A site audited in the last 24 hours returns that report instead of a new crawl. |
| `get_audit` | Progress or summary of a `quick_audit`, by `audit_id`. |
| `get_issue_urls` | Pages failing one check in a finished quick audit (`audit_id`, `check`, `limit`, `offset`). |
| `start_monitoring` | Starts free weekly monitoring for a site's owner (`url`, `email`). Emails a confirmation link; nothing starts until the owner opens it. On confirming, they get an account and an API key shown once. |

The no-key tier is rate limited: a cap on fresh audits and start-monitoring emails per 24 hours across all agents (`MCP_ANON_DAILY_AUDITS`, `MCP_ANON_DAILY_EMAILS`, both default 200), plus per-IP limits (including 30 tool calls per minute per client). A self-hosted install refuses requests without a key.

### With an API key (your monitored sites)

Create a key under **Settings, API keys** in the web app (`/settings/api-keys`). Keys start with `cdo_`, are shown once and can be revoked. Then:

```sh
claude mcp add --transport http codoseo https://codoseo.com/mcp --header "Authorization: Bearer cdo_..."
```

Or in a JSON config that supports HTTP servers:

```json
{
  "mcpServers": {
    "codoseo": {
      "type": "http",
      "url": "https://codoseo.com/mcp",
      "headers": {
        "Authorization": "Bearer cdo_..."
      }
    }
  }
}
```

A malformed, unknown or revoked key is a `401`, never a silent fall-back to the no-key tools. The keyed tools:

| Tool | Arguments | What it does |
|---|---|---|
| `list_sites` | none | Your sites: id, domain, monitoring state, schedule, latest health score and crawl time. Start here. |
| `get_site_health` | `site_id` | Health from the latest finished crawl: score, checks passed, pages, up to 15 failing checks with example URLs, next scheduled crawl, any running crawl, and a link to the full audit. |
| `get_issue_urls` | `site_id`, `check`, `limit` (50, max 200), `offset` | Pages failing one check in the latest crawl, paginated (`next_offset`). |
| `get_page` | `site_id`, `url` (absolute or a path such as `/pricing`) | Everything stored about one page: status, redirects, title, description, canonical, headings, word count, links, indexability and failing checks. |
| `get_changes` | `site_id`, `severity` (`critical`, `warning`, `notice`), `limit`, `offset` | What changed between the latest finished crawl and the one before, including the eight AI access kinds (`ai_bot_blocked`, `ai_answers_restricted`, `ai_block_not_applied`, `ai_fetch_blocked`, `ai_content_missing`, `ai_llms_txt_unavailable`, `ai_issue_resolved`, `ai_preferences_changed`). |
| `get_ai_access` | `site_id` | The latest [AI access](ai-access.md) report: per-bot robots.txt verdicts against your intent, per-engine page eligibility, declared preferences, `content` (what the important pages hold in their HTML as served; evidence grade A-), `llms_txt` (informational, never scored), the synthetic fetch check (`probes`: each AI search and user-triggered bot's documented user agent sent from CodoSEO's address and compared with CodoSEObot; evidence grade B) and open incidents, including `fetch_blocked` when a firewall or CDN blocks or challenges bots you want, `content_missing` when important pages are empty app shells to crawlers that don't run JavaScript, and `llms_txt_unavailable`. Same JSON as the REST route ([api.md](api.md#the-synthetic-fetch-check-in-ai-access), [content and llms.txt](api.md#content-and-llmstxt-in-ai-access)), 30 pages at most in `content.pages` with `more` for the rest. |
| `run_crawl` | `site_id` | Queues a crawl now, like the Run crawl button. Counts against the plan's manual crawls; one crawl per site at a time. |

Every keyed `tools/call` counts as one API call against the account's daily allowance (Free 100, Pro 2,000, Agency 10,000, per UTC day; unlimited when self-hosted). `initialize`, `tools/list` and pings are free. The tools return the same JSON as the REST API ([api.md](api.md)). Page titles and URLs come from crawled sites; agents should treat them as data, not instructions.
