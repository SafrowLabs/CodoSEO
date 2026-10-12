# REST API

A small JSON API over your monitored sites (reads, plus POST and DELETE routes to queue a crawl and repair AI access incidents), at `/api/v1` on the hosted app (`https://codoseo.com/api/v1`) and on any self-hosted install (`<BASE_URL>/api/v1`). It returns the same data as the keyed [MCP tools](mcp.md) and shares their daily allowance.

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
| `GET /api/v1/sites/{site}/changes` | `severity` (`critical`, `warning`, `notice`), `limit`, `offset` | Changes between the latest finished crawl and the one before: `total`, `changes` (kind, severity, url, before, after), `next_offset`. Kinds include the eight AI access ones: `ai_bot_blocked`, `ai_answers_restricted`, `ai_block_not_applied`, `ai_fetch_blocked` (synthetic fetch check), `ai_content_missing`, `ai_llms_txt_unavailable`, `ai_issue_resolved`, `ai_preferences_changed`. A crawl that failed on robots.txt (5xx or 429) is not a finished crawl: the AI access change it records shows up in the `ai-access` report's `open_incidents` and in alerts, not here. |
| `GET /api/v1/sites/{site}/ai-access` | | The latest [AI access](ai-access.md) report: `checked_at`, `crawl_id`, `robots`, `open_incidents`, per-bot `bots`, per-engine `engines`, the `declared` preferences, `probes_enabled` and `probes` (the synthetic fetch check, below), `content` and `llms_txt` (raw HTML content and llms.txt, [below](#content-and-llmstxt-in-ai-access)). Before the first report `checked_at` is `null` and `note` says it appears after the next crawl. |
| `GET /api/v1/sites/{site}/ai-access/incidents` | `format` (`json` default, or `csv`) | The [incident export](#ai-access-incidents): the site's open incidents and those resolved in the last 90 days. |
| `GET /api/v1/sites/{site}/ai-access/incidents/{id}/fix` | | What to change for one incident: `remedy` (`title`, `why`, `limits`, `steps`, `snippet`, `verify`, `sources`, `grade`) and the ready-made texts `copy_fix`, `developer_task` and `agent_prompt`. |
| `POST /api/v1/sites/{site}/ai-access/incidents/{id}/recheck` | | Queues a recheck of the incident, like Recheck now. `202` with `{recheck_id, due_at}`; read the result as the incident's `last_recheck` and `state` in `ai-access` a minute or two later. |
| `POST /api/v1/sites/{site}/ai-access/incidents/{id}/snooze` | body `{"days": 1\|7\|30}` | Snoozes the incident. Answers with the incident as it is now. |
| `DELETE /api/v1/sites/{site}/ai-access/incidents/{id}/snooze` | | Ends the snooze early. Answers with the incident. |
| `POST /api/v1/sites/{site}/ai-access/incidents/{id}/accept` | | Accepts the incident as it is (`fetch_blocked`, `content_missing` and `llms_txt_unavailable` only). Answers with the incident. |
| `DELETE /api/v1/sites/{site}/ai-access/incidents/{id}/accept` | | Takes the acceptance back. Answers with the incident. |
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

- `result` compares the bot with CodoSEObot: `same`, `blocked` (an error status, with `status`; no `status` when the connection was dropped), `challenged` (with the bot manager in `vendor`: `cloudflare`, `vercel`, `aws_waf`, `datadome`, `human`, `imperva`, `akamai`, `sucuri` or `generic`), `diverted` (redirected elsewhere), `degraded` (a much smaller page with another title), `throttled`, `inconclusive` (CodoSEObot itself got no usable page, no response, the confirming re-fetch disagreed, or there was no room left to confirm a worse result) or `skipped_robots` (robots.txt disallows the bot, so it was not requested). A bot treated worse on any page shows that result.
- `cause` is the subject of the matching `fetch_blocked` incident in `open_incidents`; `urls_affected` counts the pages where the bot was treated worse and the bot's `urls_probed` only the pages actually requested with its user agent: pages robots.txt keeps it from, and pages where CodoSEObot's own fetch gave nothing to compare with, are left out. The top-level `urls_probed` counts the pages fetched as CodoSEObot.
- `not_probed` reasons: `no_user_agent` (its operator documents no full user agent, and CodoSEO never makes one up), `verified_search_engine` (Googlebot, Bingbot and Applebot, which bot managers verify by address) and `not_probed_purpose` (training and ads crawlers, agents).
- `stop` (`budget` when the time ran out, `request_cap` when the request cap was reached) appears when the check stopped before its last planned page or partway through one; bots it did not reach on that page are `inconclusive`.

Only bots whose intent is `allow` become incidents. The incident kind is `fetch_blocked`, one per cause (`challenge_cloudflare`, `blocked_403`, `diverted`, ...), with a title ending "(synthetic check)". Its severity is at most `warning`.

### Content and llms.txt in `ai-access`

`content` says what the important HTML pages hold **in their HTML as served**, before any JavaScript runs. AI crawlers like GPTBot, ClaudeBot and PerplexityBot don't run it (Vercel and MERJ, 2024), so a client-side app's empty shell is all they read. It rests on evidence grade `A-`. No extra request is made: it is read from the crawl.

```json
"content": {
  "grade": "A-",
  "pages_checked": 12,
  "shells": 1,
  "pages": [
    { "url": "https://example.com/app", "verdict": "shell", "signals": ["mount_empty", "noscript_js"],
      "words": 0, "title": true, "h1": false, "description": false, "canonical": false,
      "jsonld": false, "links_internal": 0 },
    { "url": "https://example.com/", "verdict": "present", "words": 640, "title": true, "h1": true,
      "description": true, "canonical": true, "jsonld": true, "links_internal": 24 }
  ],
  "more": 0
}
```

- `verdict` is `shell`, `likely_shell`, `thin` (few words and no shell marker; informational) or `present`. `signals` appears only when there are some: `mount_empty`, `noscript_js`, `script_heavy`, `many_scripts`. `words` counts the text in the HTML as served.
- `pages` lists shells first, then the others in the report's order, at most 30; `more` counts the pages left out and is omitted when 0. `shells` counts `shell` and `likely_shell` over all checked pages.
- A shell open on the site is the `content_missing` incident in `open_incidents` (grade `A-`; `warning` when the home page or half or more of the important pages are shells, `notice` otherwise).

`llms_txt` is the site's `/llms.txt` as the last crawl found it. It is informational and never scored.

```json
"llms_txt": {
  "informational": true,
  "state": "available",
  "status": 200,
  "adopted": true,
  "title": "Example",
  "optional_section": false,
  "checks": [
    { "id": "h1", "ok": true, "detail": "…" },
    { "id": "links_resolve", "ok": false, "detail": "…" }
  ],
  "link_summary": { "checked": 7, "failing": 1, "not_checked": 3, "failing_urls": ["https://example.com/old"] }
}
```

- `state` is `available`, `absent` (a 4xx other than 429), `soft_not_found` (a web page where the file should be) or `unavailable` (429, 5xx or no answer, with `error` when nothing answered). `checks` is filled only for `available`; ids are `h1`, `summary`, `sections`, `links`, `length`, `size`, `mime` and `links_resolve`.
- `adopted` is true when the site serves, or served before, an llms.txt. Only an adopted file that becomes `unavailable` raises the `llms_txt_unavailable` incident (`warning`, grade `A`).
- `llms_txt_skipped` (top level, next to `llms_txt`, left out when absent) says why the crawl did not fetch the file: `robots_disallowed` (robots.txt disallows `/llms.txt`, or where it redirects, for CodoSEObot), `refused` (the address, or a redirect's, is one CodoSEO may not request) or `deadline` (the crawl ran out of time). `llms_txt` is then absent too.
- `link_summary` counts the links checked (a crawl status, or one request each up to the plan's cap: Free 5, other plans 20), those answering 4xx, 5xx or nothing, and those not checked (past the cap or on another site). `failing_urls` holds up to ten.

Both are left out of reports from before they existed, and `content` when no important page is HTML.

### AI access incidents

Each entry of `open_incidents` in `ai-access` (and the answer to snooze and accept) carries, besides `id`, `kind`, `subject`, `severity`, `title`, `summary`, `opened_at` and `last_seen_at`:

```json
{
  "id": 12, "kind": "bots_blocked", "subject": "search", "severity": "critical",
  "first_seen": "2026-10-09T06:12:44Z",
  "grade": "A",
  "sources": ["https://developers.openai.com/api/docs/bots"],
  "state": "verifying",
  "accepted": false,
  "recovered_at": "2026-10-10T08:00:00Z",
  "verify_due_at": "2026-10-11T08:00:00Z",
  "lag": { "hours": 24, "source": "https://developers.openai.com/api/docs/bots" },
  "affected_urls": ["https://example.com/"],
  "last_recheck": { "outcome": "clean", "at": "2026-10-10T08:00:00Z" }
}
```

- `first_seen` is `opened_at` under the name the export uses. `grade` is the [evidence grade](ai-access.md#evidence-grades) (`A`, `A-`, `B`, `C` or `X`) and `sources` the documents the finding rests on.
- `state` is `open`, `accepted`, `snoozed`, `verifying` (a recheck saw the fix and a follow-up, due at `verify_due_at`, confirms it), `waiting_for_crawl` (the fix was seen but nothing is scheduled to confirm it, so the next crawl will), `recheck_queued` or `recheck_running`. Your decisions win: an accepted or snoozed incident reports `accepted` or `snoozed` whatever CodoSEO is doing.
- `snoozed_until` appears while snoozed, `accepted` is a boolean, `recovered_at` and `verified_at` appear once a check saw the problem gone and once that was confirmed. `lag` is how long the operators take to notice a fix (`hours`) and where they say so (`source`, left out when nobody publishes it).
- `affected_urls` lists the pages it affects (at most 20). `last_recheck` is the latest finished recheck: `outcome` is `clean`, `still_failing`, `inconclusive` (with `notes` saying why), `failed` (it could not run, or the site did not answer) or `superseded` (a crawl judged instead), and `at` when it finished.

`GET .../ai-access/incidents` returns one record per incident, as JSON or `?format=csv` (`text/csv`). The columns are `kind`, `subject`, `severity`, `grade`, `title`, `state` (`resolved`, or what `state` says of an open one), `first_seen`, `last_seen`, `resolved_at`, `resolved_by` (`crawl`, `recheck` or `intent`), `verified_at`, `affected_urls` and `sources`. A `format` other than `csv` or `json` is a `400` ("format must be csv or json."). The export is the one the app's **Issues CSV** and **JSON** buttons download.

The fix, recheck, snooze and accept routes follow the rules of the app's buttons, so the same limits apply: a recheck counts against the account's daily rechecks (Free 3, Pro 20, Agency 60, unlimited self-hosted, over a trailing 24 hours) and each call counts against the API allowance. An `{id}` that is not a number is the same `404` as an unknown one. A snooze without a body, or with a number of days other than 1, 7 or 30, is a `400`.

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
| 403 | `plan_limit` | The plan's manual crawl allowance is used up, or its daily AI access rechecks. |
| 404 | `not_found` | Unknown site, unknown endpoint, no finished crawl yet, or the URL is not in the latest crawl. |
| 405 | `bad_request` | The method is not allowed for that path. |
| 409 | `crawl_in_progress` | The site already has a crawl queued or running. |
| 409 | `conflict` | An incident action the incident's state does not allow (resolved, a recheck already queued, a cooldown, a crawl running). |
| 429 | `quota_exceeded` | Today's API calls are used up. |
| 503 | `unavailable` | The database is unreachable; `Retry-After: 10`. |
| 500 | `internal` | Something went wrong on our side. |

Errors from the incident routes, with their messages (the incident-specific ones are the same on the MCP tools):

| Status | `code` | Message |
|---|---|---|
| 404 | `not_found` | `No such incident. get_ai_access lists a site's incidents.` |
| 409 | `conflict` | `That issue is already resolved.` (any action on a resolved incident) |
| 409 | `conflict` | `A recheck of this issue is already queued.`, or `Already verifying — due at <time>.` while the follow-up is pending |
| 409 | `conflict` | `Rechecked moments ago. Try again at <time>.` (within 10 minutes of the last manual recheck) |
| 409 | `conflict` | `A crawl is running; it will judge this.` |
| 403 | `plan_limit` | `<N> rechecks per day on your plan — next at <time>.` (`1 recheck` when the plan has one), or `Your plan has no rechecks. The next crawl will check this issue.` |
| 400 | `bad_request` | `Snooze for 1, 7 or 30 days.` (also for a missing or non-integer `days`) |
| 400 | `bad_request` | `The body must be JSON like {"days": 7}.` (a body that is not JSON) |
| 400 | `bad_request` | `A failing robots.txt can't be accepted: it can only be snoozed while you fix it.` |
| 400 | `bad_request` | `This issue is a choice through your intent: use Mark intended instead.` (accepting `bots_blocked`, `bots_not_blocked` or `answers_restricted`) |
| 400 | `bad_request` | `A snooze has to end in the future.` |

## Rate limits

Each authenticated call counts once against the account's daily allowance, whether it succeeds or not (only `GET /api/v1/usage` is free). The count resets at 00:00 UTC. Allowances: Free 100, Pro 2,000, Agency 10,000 calls per day; a self-hosted install has no limit.

Responses on a limited plan carry `X-RateLimit-Limit` and `X-RateLimit-Remaining`. Both are left out when there is no limit. A `429` adds `Retry-After` (seconds until 00:00 UTC) and sets `X-RateLimit-Remaining: 0`.

```sh
curl -si -H "Authorization: Bearer cdo_..." https://codoseo.com/api/v1/usage | head -n 20
```
