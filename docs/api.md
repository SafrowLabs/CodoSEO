# REST API

A small JSON API over your monitored sites, at `/api/v1` on the hosted app (`https://codoseo.com/api/v1`) and on any self-hosted install (`<BASE_URL>/api/v1`). It returns the same data as the keyed [MCP tools](mcp.md) and shares their daily allowance.

## Authentication

Create an API key under **Settings, API keys** (`/settings/api-keys`). Keys look like `cdo_` followed by 43 URL-safe characters, are shown once, and can be revoked. Send it as a bearer token. Nothing else authenticates the API; cookies are ignored.

```sh
curl -H "Authorization: Bearer cdo_..." https://codoseo.com/api/v1/sites
```

A missing, malformed, unknown or revoked key is a `401` with `WWW-Authenticate: Bearer`. The API needs no `Origin` header.

## Routes

All routes are scoped to the key's account. A site id that is unknown or belongs to another account is the same `404`.

| Method and path | Query | Returns |
|---|---|---|
| `GET /api/v1/sites` | | A list of sites: `id`, `domain`, `start_url`, `monitoring_active`, `schedule`, `health_score`, `last_crawled_at`. |
| `GET /api/v1/sites/{site}` | | The site's health: `latest_crawl` (score, checks passed and total, pages crawled, stop reason, up to 15 `failing_checks` with examples), `active_crawl`, `next_crawl_at`, `audit_url`. |
| `GET /api/v1/sites/{site}/issues/{check}` | `limit` (default 50, max 200), `offset` | Pages failing a check (by slug, for example `title_missing`) in the latest finished crawl: `total`, `urls`, `next_offset`. |
| `GET /api/v1/sites/{site}/page` | `url` (required; absolute or a path) | One page's record from the latest finished crawl: status, redirect chain, title, description, canonical, headings, word count, link counts, indexability, `issues`. |
| `GET /api/v1/sites/{site}/changes` | `severity` (`critical`, `warning`, `notice`), `limit`, `offset` | Changes between the latest finished crawl and the one before: `total`, `changes` (kind, severity, url, before, after), `next_offset`. Kinds include the six AI access ones: `ai_bot_blocked`, `ai_answers_restricted`, `ai_block_not_applied`, `ai_fetch_blocked` (synthetic fetch check), `ai_issue_resolved`, `ai_preferences_changed`. A crawl that failed on robots.txt (5xx or 429) is not a finished crawl: the AI access change it records shows up in the `ai-access` report's `open_incidents` and in alerts, not here. |
| `GET /api/v1/sites/{site}/ai-access` | | The latest [AI access](ai-access.md) report: `checked_at`, `crawl_id`, `robots`, `open_incidents`, per-bot `bots`, per-engine `engines`, the `declared` preferences, `probes_enabled` and `probes` (the synthetic fetch check, below). Before the first report `checked_at` is `null` and `note` says it appears after the next crawl. |
| `POST /api/v1/sites/{site}/crawls` | | Queues a crawl. `202` with `{site_id, crawl_id, number, status:"queued"}`. |
| `GET /api/v1/usage` | | `calls_today`, `limit`, `remaining`, `resets_at`. Not counted against the allowance. |

`{site}` is the site's UUID from `GET /api/v1/sites`. Times are RFC 3339 in UTC. Responses carry `Cache-Control: no-store`. A path that does not exist is a JSON `404`, and the wrong method on a real path is a JSON `405`. List endpoints with more rows return `next_offset`; pass it as `offset` for the next page.

### The synthetic fetch check in `ai-access`

After each monitored crawl CodoSEO fetches a few important pages with the documented user agent of each AI search and user-triggered bot, from its own address, and compares each response with CodoSEObot fetching the same page at the same moment. `probes` reports the latest check. It is synthetic: real bots come from their own addresses and may be treated differently, so the results carry evidence grade `B`. `probes` is left out when the owner switched the check off (`probes_enabled: false`) and when the latest report has none (a report from before the check existed, or a crawl whose robots.txt gave no verdicts).

```json
"probes_enabled": true,
"probes": {
  "synthetic": true,
  "grade": "B",
  "urls_probed": 3,
  "from": "hello@codoseo.com",
  "bots": [
    { "token": "OAI-SearchBot", "purpose": "search", "intent": "allow", "result": "challenged",
      "cause": "challenge_cloudflare", "vendor": "cloudflare", "urls_affected": 3, "urls_probed": 3 },
    { "token": "PerplexityBot", "purpose": "search", "intent": "allow", "result": "blocked",
      "cause": "blocked_403", "status": 403, "urls_affected": 1, "urls_probed": 3 },
    { "token": "ChatGPT-User", "purpose": "user_fetch", "intent": "allow", "result": "same",
      "urls_affected": 0, "urls_probed": 2 }
  ],
  "not_probed": [
    { "token": "Claude-SearchBot", "reason": "no_user_agent" },
    { "token": "Googlebot", "reason": "verified_search_engine" }
  ]
}
```

- `result` compares the bot with CodoSEObot: `same`, `blocked` (an error status, with `status`; no `status` when the connection was dropped), `challenged` (with the bot manager in `vendor`: `cloudflare`, `vercel`, `aws_waf`, `datadome`, `human`, `imperva`, `akamai`, `sucuri` or `generic`), `diverted` (redirected elsewhere), `degraded` (a much smaller page with another title), `throttled`, `inconclusive` (CodoSEObot itself got no usable page, no response, or the confirming re-fetch disagreed) or `skipped_robots` (robots.txt disallows the bot, so it was not requested). A bot treated worse on any page shows that result.
- `cause` is the subject of the matching `fetch_blocked` incident in `open_incidents`; `urls_affected` counts the pages where the bot was treated worse and `urls_probed` the pages actually requested with its user agent.
- `not_probed` reasons: `no_user_agent` (its operator documents no full user agent, and CodoSEO never makes one up), `verified_search_engine` (Googlebot, Bingbot and Applebot, which bot managers verify by address) and `not_probed_purpose` (training and ads crawlers, agents).
- `stop` (`budget`, `request_cap` or `cancelled`) appears when the check stopped before its last planned page.

Only bots whose intent is `allow` become incidents. The incident kind is `fetch_blocked`, one per cause (`challenge_cloudflare`, `blocked_403`, `diverted`, ...), with a title ending "(synthetic check)". Its severity is at most `warning`.

`POST .../crawls` follows the same rules as the Run crawl button: the plan's manual crawl allowance applies and only one crawl runs per site at a time.

## Errors

Errors have one shape:

```json
{ "error": { "code": "not_found", "message": "No such site. List your sites to see their ids." } }
```

| Status | `code` | When |
|---|---|---|
| 400 | `bad_request` | A bad query string, unknown check slug or severity, missing or invalid `url`; also used for the 405. |
| 401 | `unauthorized` | No key, or a key that is not valid. |
| 403 | `plan_limit` | The plan's manual crawl allowance is used up. |
| 404 | `not_found` | Unknown site, unknown endpoint, no finished crawl yet, or the URL is not in the latest crawl. |
| 405 | `bad_request` | The method is not allowed for that path. |
| 409 | `crawl_in_progress` | The site already has a crawl queued or running. |
| 429 | `quota_exceeded` | Today's API calls are used up. |
| 503 | `unavailable` | The database is unreachable; `Retry-After: 10`. |
| 500 | `internal` | Something went wrong on our side. |

## Rate limits

Each authenticated call counts once against the account's daily allowance, whether it succeeds or not (only `GET /api/v1/usage` is free). The count resets at 00:00 UTC. Allowances: Free 100, Pro 2,000, Agency 10,000 calls per day; a self-hosted install has no limit.

Responses on a limited plan carry `X-RateLimit-Limit` and `X-RateLimit-Remaining`. Both are left out when there is no limit. A `429` adds `Retry-After` (seconds until 00:00 UTC) and sets `X-RateLimit-Remaining: 0`.

```sh
curl -si -H "Authorization: Bearer cdo_..." https://codoseo.com/api/v1/usage | head -n 20
```
