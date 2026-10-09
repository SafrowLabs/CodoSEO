# AI access

AI access monitoring answers one question: can the AI crawlers and answer engines you care about reach your site, and does what they find let your pages appear in their answers? It runs on every crawl, next to the SEO checks, and never touches the health score. There is no composite "GEO score" and no "citation probability": every finding says what was checked, what it rests on and where the operator documents it.

![The AI access screen](images/ai-access/ai-access-light.png)

## What is checked

**robots.txt, per bot.** For each bot in the [registry](#the-public-registry) (OpenAI, Anthropic, Perplexity, Google, Apple, Meta, Amazon, Microsoft, DuckDuckGo, Mistral, Common Crawl and more) CodoSEO reads the group of your robots.txt that applies to its product token, applies the longest-match rule the way the operators describe, and records whether the home page and the other important pages are allowed, with the winning rule and its line. Important pages are the home page and the most linked ones, up to 60. Control tokens such as `Google-Extended` never make requests but still decide what the operator may do, so they are listed too. A robots.txt that answers 5xx or 429 is its own finding: crawlers are told to stay away until it recovers.

**Page-level controls, per engine.** For each of ten engines (Google Search, AI Overviews and AI Mode, Bing and Copilot, Apple, Amazon, ChatGPT, Claude, Perplexity, Meta AI, DuckDuckGo, Mistral) CodoSEO reads the controls that engine documents on each important page: the robots meta tag, `X-Robots-Tag` and `data-nosnippet`. Only controls the operator documents count, and only for the engines that honour them: `noarchive` removes a page from Copilot's answers but does nothing to Google's. A page is *eligible*, *limited* (a snippet cap, or a large share of its text under `data-nosnippet`) or *excluded*.

**Robots-only engines.** Some engines document no page-level control at all. For those the page is always eligible, and robots.txt is the only lever; the screen says so instead of implying a clean bill of health.

**Declared preferences, declared, not enforced.** `Content-Signal` and `Content-Usage` lines in robots.txt and response headers, and TDM reservation (`tdm-reservation` meta tag and header, `/.well-known/tdmrep.json`) are listed as the site states them. Nothing enforces them, and CodoSEO does not claim any bot honours them.

## What is not checked yet

- Whether a bot really gets through: **synthetic fetch probes** (fetching a page as each bot and comparing what comes back) arrive later, in milestone B1. Today the verdict is what robots.txt and the page's markup say, not what a firewall or CDN does.
- Whether bots really visit: **log evidence** (which bots crawled you, verified by IP range) arrives later, in C1.
- Whether any engine cites you. CodoSEO reports access, not outcomes.

Every Phase A finding is evidence grade **A**: it rests on something the operator documents, and the finding links to that page.

## Intent

A blocked bot is a mistake or a choice, and only you know which. The intent model tells CodoSEO:

| Purpose | Default |
|---|---|
| AI search (OAI-SearchBot, PerplexityBot, Claude-SearchBot, ...) | Allow |
| User-triggered fetchers (ChatGPT-User, Claude-User, ...) | Allow |
| AI agents | No preference |
| AI training (GPTBot, ClaudeBot, Google-Extended, CCBot, ...) | No preference |
| Ads | No preference |

Set it per purpose, and override single bots, under **AI access, Intent** in the web app (`/s/{site}/ai-access/intent`). Severity follows the intent: blocking GPTBot is nothing to report when you have no preference, and a warning or worse when you said you want training crawlers in. A bot you want blocked that can still crawl is reported too. A bot with no preference is never reported either way. The CLI and MCP tools have nowhere to keep your intent, so they judge under the defaults above.

## Incidents and alerts

The web app turns findings into incidents that open when a crawl first sees a problem and resolve when a later crawl no longer does. The AI access screen lists the open ones, and the changes page and alerts use five change kinds for them:

| Kind | Meaning |
|---|---|
| `ai_bot_blocked` | Bots you want are kept out by robots.txt, or robots.txt fails |
| `ai_answers_restricted` | Page markup takes pages out of an engine's AI answers |
| `ai_block_not_applied` | A bot you want blocked can still crawl the site |
| `ai_issue_resolved` | An AI access incident is gone |
| `ai_preferences_changed` | Your declared Content-Signal, Content-Usage or TDM preferences changed |

They are in the alert settings grid like any other kind, so email, Slack, Discord and webhook alerts work for them.

**The first check is quiet.** The incidents the first report opens are your baseline: they are listed but write no changes and send no alerts, so turning the feature on does not fire a storm. Changing your intent re-evaluates quietly the same way.

**Mark intended** on an incident says "this is what I want": it sets the matching intent (for example, block this bot), re-evaluates quietly, and the incident resolves as a choice instead of a fix.

## The public registry

The list of bots behind all this is public at `/ai-bots` (a searchable table) and `/ai-bots.json` (the data), on codoseo.com and on every self-hosted install, no login needed. The data file is licensed **CC0-1.0**: use it for anything, no attribution needed. Each bot has its robots.txt token, operator, product, purpose, whether the operator says it honours robots.txt, the operator's IP-range file and verification hints where published, the source page it was checked against, and the date it was last reviewed.

To propose a change, open a pull request against [SafrowLabs/CodoSEO](https://github.com/SafrowLabs/CodoSEO) editing `crates/geo/data/ai-bots.json`. Cite an operator-owned page in `source_url`, never a third-party directory, and update `last_reviewed` and `updated`. `cargo test -p codoseo-geo` lints the file; the field list is in `crates/geo/data/README.md`.

## Using it

### CLI

```sh
# The registry
codoseo bots
codoseo bots --format json > ai-bots.json   # the file exactly as published

# What robots.txt says to each bot for a path, and the declared preferences
codoseo robots https://example.com/ --path /pricing
```

```
AI bots (path /pricing)
  GPTBot         OpenAI     training    Blocked  line 2
  OAI-SearchBot  OpenAI     search      Allowed
  ChatGPT-User   OpenAI     user fetch  Allowed  may ignore robots.txt
  ...
  Content-Signal (line 6): search=yes, ai-train=no
```

`--format json` adds `bots` (token, operator, purpose, `honours_robots`, `allowed`, `line`, `rule`) and `declared` to the existing fields.

`codoseo crawl` adds an **AI access** section to the table and markdown reports, and an `ai_access` object (`report` and `findings`, judged under the default intent) to the JSON audit. The audit's `format_version` stays 1; older audit files load and diff as before. `--fail-on` looks only at the SEO checks.

### Local MCP

`check_ai_access` takes a `url` and fetches its robots.txt and the page right now. It returns a short `summary`, every registry bot's verdict for the path (`token`, `operator`, `purpose`, `allowed`, `line`, `rule`), the declared preferences, and for each engine the page's effect (`eligible`, `limited` or `excluded`) with its causes. See [mcp.md](mcp.md).

### Hosted MCP and REST

`get_ai_access` (MCP) and `GET /api/v1/sites/{site}/ai-access` (REST) return the latest report of one of your sites and its open incidents:

```sh
curl -H "Authorization: Bearer cdo_..." https://codoseo.com/api/v1/sites/{site}/ai-access
```

```json
{
  "site_id": "…",
  "checked_at": "2026-10-09T06:12:44Z",
  "crawl_id": "…",
  "robots": { "status": 200, "availability": "ok" },
  "open_incidents": [
    { "id": 12, "kind": "bots_blocked", "subject": "search", "severity": "critical",
      "title": "OAI-SearchBot is blocked by robots.txt", "summary": "…",
      "opened_at": "…", "last_seen_at": "…" }
  ],
  "bots": [
    { "token": "OAI-SearchBot", "operator": "OpenAI", "purpose": "search", "intent": "allow",
      "home_allowed": false, "important_blocked": 3, "important_total": 3, "conflicts": true }
  ],
  "engines": [
    { "id": "google", "name": "Google Search, AI Overviews & AI Mode",
      "eligible": 3, "limited": 0, "excluded": 0, "page_controls": true }
  ],
  "declared": { "content_signals": [], "content_usage": [] }
}
```

Before a site has a report, `checked_at` is `null` and `note` says it appears after the next crawl. The call counts once against the daily allowance like any other ([api.md](api.md)).
