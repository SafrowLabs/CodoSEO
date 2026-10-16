# Site policy

A site policy says how a site is meant to work for search engines and AI bots, so CodoSEO can tell a mistake from a choice. Every crawl compares the site with it: where the site disagrees, that is a gap and an issue; where you say something is on purpose, it stops being flagged. CodoSEO never changes your site or its robots.txt.

The policy has two parts:

- **SEO rules**: the preferred address, what search engines should index, URL parameters that don't change the page and pages that must always work are checked on every crawl ([below](#seo-rules)). Check thresholds set the limits behind the standard checks (title length, thin pages, slow pages and others), and exceptions take a check off the health score for part of the site ([below](#thresholds-and-exceptions)).
- **AI rules**: what each kind of AI bot should get, per-bot exceptions, the answer limits you use on purpose, and whether the synthetic fetch checks run. They are what [AI access](ai-access.md) used to call the intent.

The same policy lives in three places with one format: on a monitored site in the web app (**Site policy** in the sidebar, `/s/{site}/policy`), in a `codoseo.toml` file the CLI reads, and as JSON in the REST API and the MCP tools.

## The file

`codoseo.toml`, with a `[policy]` table. `codoseo policy init` writes one with every key commented out at its default. Keys left out keep their defaults; keys CodoSEO doesn't know are ignored with a warning (so an older release reads a newer file), and anything else that is wrong is an error naming the key.

```toml
[policy]
version = 1

[policy.seo.address]
host = "apex"
trailing_slash = "without"

[[policy.seo.index]]
pattern = "/checkout/*"
stance = "keep_out"

[policy.seo]
index_default = "index"
ignore_params = ["utm_*", "gclid", "fbclid", "sort", "ref"]
pagination_param = "page"
must_work = ["/", "/pricing", "/checkout/cart"]

[policy.seo.thresholds]
title_max = 65
thin_words = 120

[[policy.seo.exceptions]]
check = "thin_content"
scope = "/stores/*"
note = "Store pages are short by design"

[policy.ai]
purposes = { search = "allow", user_fetch = "allow", training = "block" }
bots = { "Perplexity-User" = "block" }
accepted_directives = ["max_snippet", "data_nosnippet"]
probes = true
```

### Every key

| Key | Default | Meaning |
|---|---|---|
| `version` | `1` | The format version. Only `1` is accepted. |
| **AI rules** | | |
| `ai.purposes` | search and user_fetch `allow`, agent, training and ads `any` | The stance per purpose: `allow` (an issue opens when robots.txt keeps a bot of that kind out), `block` (an issue opens when one can still crawl), `any` (no preference, never reported). Purposes: `search`, `user_fetch`, `agent`, `training`, `ads`. A purpose left out keeps its default. |
| `ai.bots` | none | Per-bot exceptions by robots.txt token (matched ignoring case), each `allow`, `block` or `any`; an exception wins over its purpose. `codoseo bots` lists the tokens. At most 200; tokens the registry doesn't know are kept but ignored. |
| `ai.accepted_directives` | none | Page markup you use on purpose: `nosnippet`, `max_snippet`, `noarchive`, `nocache`, `data_nosnippet`. Pages carrying them raise no AI answers issue. `noindex` can't be accepted: the SEO checks report it. |
| `ai.probes` | `true` | Run the synthetic fetch checks after each monitored crawl. The CLI runs them only with `--probes`. |
| **SEO rules** | | |
| `seo.address.host` | not checked | `apex` (`https://example.com`) or `www`. |
| `seo.address.trailing_slash` | `without` | `without`, `with` or `either`. |
| `seo.index` | none | Path rules, top to bottom, the first match wins: `pattern` and `stance` (`index`, `keep_out`, `none`). At most 50. `keep_out` expects a `noindex` tag or header: robots.txt alone stops crawling, not indexing. |
| `seo.index_default` | `none` | The stance for pages no rule matches. |
| `seo.ignore_params` | none | Query parameters that don't change the page (`utm_*`); URLs differing only by them should canonicalise to the clean URL. At most 30. |
| `seo.pagination_param` | none | The pagination parameter: each page canonicalises to itself. |
| `seo.must_work` | none | Paths that must answer 200 and be indexable on every crawl. The home page and starred pages are added when evaluated. At most 100, and at most what the plan allows: Free 5, Lite 20, Pro 50, self-hosted any. |
| **Thresholds and exceptions** | | |
| `seo.thresholds.title_min`, `title_max` | 30, 60 | Title length in characters: a title under the minimum or over the maximum is flagged. `title_min` 1 to 300, `title_max` 10 to 300. |
| `seo.thresholds.description_min`, `description_max` | 70, 160 | Meta description length in characters. `description_min` 1 to 1000, `description_max` 10 to 1000. |
| `seo.thresholds.thin_words` | 200 | An indexable HTML page with fewer words is a thin page. 1 to 10000. |
| `seo.thresholds.slow_ms` | 1000 | Milliseconds to the response headers (about time to first byte); a slower page is a slow page. 100 to 60000. |
| `seo.thresholds.max_redirect_hops` | 1 | A redirect chain with more hops than this is flagged, so by default 2 or more. 1 to 10. |
| `seo.thresholds.image_alt` | `true` | `false` switches the "Images are missing alt text" check off; it then no longer counts in the checks total. |
| `seo.exceptions` | none | Checks that don't apply to part of the site: `check` (a check slug), `scope` (a pattern), `note`. At most 100. |

Patterns start with `/`. `*` matches any run of characters, `/` included. Without a `?` a pattern is matched against the path; with one, against the path and the query: `/collections/*?page=*`.

## SEO rules

Each rule is compared with the crawl's pages. A rule the policy doesn't set is **Not checked** and changes nothing; a set rule turns on its check, whose gaps are issues on the pages behind them, listed in the explorer, the fix list and the issue pages, counted in the checks total and costing health points like any other check ([scores](scores.md)).

| Rule | Met when | A gap | Check |
|---|---|---|---|
| Preferred address (`seo.address`) | Every page answering 200 is on the preferred scheme, host (`apex` or `www`) and ending, canonicals point at that form, redirects end on it, and internal links don't use a form that only redirects. | Each page served on another form, canonicalising to one, redirecting to one, or linking to a form that redirects (each such link is a hop). Link counts read "at least" when they come from a sample. The home page and file-like paths (`/feed.xml`) are fine with either ending. | `policy_address` (Warning) |
| What to index (`seo.index`, `seo.index_default`) | Each page answering 200, and each URL robots.txt keeps out, falls in the first rule whose pattern matches it, or in "everything else". `index`: indexable (or canonicalising elsewhere). `keep_out`: `noindex` (or canonicalising elsewhere). | An `index` page with `noindex` (Critical in the comparison) or blocked by robots.txt; a `keep_out` page that is indexable. The comparison notes how many are in the sitemap. | `policy_index_mismatch` (Warning) |
| | | **Partly**: a `keep_out` URL kept out by robots.txt only. Google can still index the URL from links: allow crawling and add `noindex`. | `policy_keep_out_partial` (Notice) |
| URL parameters (`seo.ignore_params`) | Every page whose URL carries an ignored parameter (`*` globs, ignoring case) is `noindex` or canonicalises to a URL without it. The pagination parameter is never one of them: `?page=2` alone is left out, `?page=2&utm_source=x` should canonicalise to `?page=2`. | A page with an ignored parameter that is indexable on itself, or whose canonical keeps the parameter. | `policy_param_canonical` (Notice) |
| Pages that must work (`seo.must_work`) | The home page, every listed path and every starred page answer 200 and are indexable, after following redirects. | One that answers an error, can't be fetched, is blocked by robots.txt, carries `noindex` or canonicalises elsewhere. One the crawl didn't reach (a time limit) is **Not reached**, not a gap. | `must_work_failed` (Critical) |

Must-work pages and starred pages are fetched on every crawl even when no link leads there; each takes a place under the crawl's page limit. The rule is checked only when `seo.must_work` lists at least one page. A must-work page that fails records a **Must-work page failed** change (`must_work_failed`) on every crawl it keeps failing, and that change alerts at once by default (Settings → Alerts).

`codoseo crawl --policy` and the hosted crawls run the same rules: the audit's `policy.report`, `crawls.summary.policy` and the API's `report` hold the same results, with `parts` for each index rule and must-work page.

## Thresholds and exceptions

**Thresholds** change the limits of standard checks. A missing key keeps its default, a minimum must be below its maximum, and the default policy gives reports byte for byte the same as before. Check titles name the numbers in use ("Title is over 65 characters", "Response took over 1.5 seconds", "Redirect chain has 3 or more hops") everywhere: the web app, the CLI tables, Markdown and GitHub annotations, SARIF rule descriptions and messages, and the MCP and REST `failing_checks` and issue titles.

**Exceptions** say a check doesn't apply to part of the site. A page whose path matches the `scope` keeps the issue (the explorer tags it **Excepted · <note>**), but it leaves that check's count in the [health score](scores.md#seo). An exception can't name a policy check (`policy_address`, `policy_index_mismatch`, `policy_keep_out_partial`, `policy_param_canonical`, `must_work_failed`) or a site-wide check (such as `sitemap_missing` or `robots_blocks_site`), and the same check and scope twice is refused.

A page an index rule keeps out of search (`keep_out`) that does carry `noindex`, as the rule asks, leaves the health score entirely, its page count included.

Reports gain `excluded`, `{ "pages": n, "counts": [["thin_content", 2], ...] }`, in the audit JSON report and in `crawls.summary` (absent when nothing is excluded). The CLI tables say "N pages, M excluded from the health score", and the audit fix list says "(M excluded from the health score by the site policy)".

## On a monitored site

**Site policy**, under THIS SITE in the sidebar after Crawls (`G` then `P`), has an **SEO rules** tab and an **AI rules** tab, each labelled with its gaps. The SEO tab has a section for each of the four rules above, plus check thresholds and exceptions, each compared with the latest crawl as you edit: the preferred host and ending, the index rules (one row each, with how many pages it matches and how they compare, plus "Everything else"; **Add rule** and **Remove**), the parameters to ignore and the pagination parameter, and the must-work paths, one per line, with a row per page (home, listed or starred) saying how it answered. **Check thresholds** has a row per threshold with its default, an input with its unit (the slow page in seconds) and how many pages it flags on the latest crawl ("12 pages flagged (default flags 30)"). **Exceptions** has a row per exception (the check, why, the pattern; **Add** and **Remove**) and how many pages they take out of the health score. What would stop the rules from saving (a pattern without a leading `/`, more must-work pages than the plan allows) is listed above the form as you type, and saving such a form shows it again with the problems inline. On the AI tab each kind of bot has an Allow / Block / No preference control with a line comparing it with the latest crawl: **Met**, **N gaps** (each bot named: blocked by robots.txt, blocked by your firewall, or getting in when the policy says Block), **Not checked** (no preference, or nothing to compare with yet) or **Limited effect** (blocking bots that don't follow robots.txt only works at the firewall). The disclosure under it sets one bot differently and shows how each bot fares now. **Answer limits you use on purpose** shows where each directive is used, and flags one in use that you untick. "Set a policy for this bot" on the AI access Bots tab links straight to the bot.

The right rail updates as you edit, before you save: **Your rules vs crawl #N** (rules met, and the SEO and AI gaps together), **When you save** on both tabs (how many issues become intended or excepted and leave the score, how many new issues open now without waiting for a crawl, and the health and GEO scores before and after) and, on the AI tab, **robots.txt that matches**: your robots.txt as that crawl read it, rewritten so the AI bots get what the policy says (blanket blocks of allowed bots removed, one group for the bots to block, the matching `Content-Signal` line in the `*` group), with a line diff against your file. **Copy file** copies it, **Copy as prompt** copies a prompt for a coding agent. It is a suggestion: a CDN-managed robots.txt may override it.

**Save policy** judges the latest finished crawl again at once, in the same transaction: the threshold checks run again on the stored page fields, the policy checks are set again, and the other checks (site-wide ones such as duplicate titles) keep what was stored. The pages' issues, the summary counts, the excluded counts, the policy report, the checks passed and the health score are rewritten, then AI access is judged again and the GEO and Overall scores recomputed. Saving through the REST API, MCP `set_policy`, an import or Mark intended does the same. Older crawls keep what they were judged under. Issues it opens or resolves send no alerts.

So a diff or a `--baseline` gate that compares the latest crawl with the previous one can show changes that come from the policy alone: issues "new" or "fixed" only because a threshold moved. **Reset to defaults** resets the tab's rules. **Export** downloads the policy as `codoseo.toml`; **Import a codoseo.toml** replaces the whole policy with a file and shows what is wrong with one that isn't valid. **History** lists every change: when, who (an account, or an API key), where (the policy screen, the API, an import, Mark intended on an AI access issue, the upgrade that moved the intent here), the changed keys, and the newest crawl's health and GEO scores before and after, so a score jump can be traced to a policy edit (`/s/{site}/policy/history`). Each change is also a pin on the Site audit's score trend, on the crawl it judged again ("Policy changed by <who>", "Health 75 → 79."), and a row at the top of the Changes page with what changed, the scores and a link to the history. History is kept as long as the plan keeps crawl history.

The old intent address, `/s/{site}/ai-access/intent`, redirects to the AI tab.

## CLI

| Command | What it does |
|---|---|
| `codoseo policy init [--from URL] [-o FILE] [--force]` | Writes `codoseo.toml` with every key commented out at its default. `--from URL` fetches the URL and fills in the preferred address (www or not from where it redirects; the trailing slash from its path, `without` for a bare home page). Never overwrites a file without `--force`. |
| `codoseo policy validate [FILE]` | Checks `FILE` (default `./codoseo.toml`): exit 0 and its hash when valid, unknown keys as warnings on stderr; exit 1 with one line per problem when not. |
| `codoseo policy check --audit FILE [--policy FILE] [--format text\|json]` | Compares an audit `codoseo crawl --save` wrote with a policy file (default `./codoseo.toml`): one line per rule and the gap counts, or with `json` the same `{ file, hash, policy, report }` a crawl saves. Exit 1 when there is any gap. |
| `codoseo policy export --site ID --api-key KEY [--base-url URL] [-o FILE]` | Downloads a monitored site's policy as TOML. The key can come from `CODOSEO_API_KEY`; `--base-url` points at a self-hosted install. |
| `codoseo policy import --site ID --api-key KEY [--base-url URL] [FILE]` | Validates `FILE` (default `./codoseo.toml`) locally, then replaces the site's policy with it. Recorded in the site's history as made through the API. |

`codoseo crawl` reads `./codoseo.toml` when there is one, or `--policy FILE`; `--no-policy` ignores it. The SEO rules are then checked (their checks join the report and the health score, and the must-work paths are fetched even when nothing links to them), AI access is judged under the file's AI rules instead of the default intent, so `--fail-on geo` and `bots_not_blocked` gates follow your policy, and the audit JSON gains a `policy` field: `{ file, hash, policy, report }`, the report being the same rule results the web app shows. `--fail-on policy`, `policy:<address|index|params|must_work|ai>` and `must_work` gate on its gaps ([ci.md](ci.md)); SARIF lists the policy checks as rules. `format_version` stays `1`. An invalid file stops the command before it crawls (exit 2). The thresholds and exceptions apply too, and SARIF marks excepted issues, and issues on kept-out pages that carry `noindex` as asked, as suppressed ([ci.md](ci.md#sarif)). `codoseo check` takes the same flags and applies the thresholds to the one page; one page has no AI access report.

## MCP

- Local server (`codoseo mcp`): `get_policy` with an optional `path` (default `codoseo.toml` in the server's working directory) returns `policy` (every default filled in), the file's `hash` and `warnings`. An invalid file fails with one line per problem.
- Hosted server with an API key: `get_policy` with a `site_id` returns `{ site_id, policy, report, crawl_id, updated_at }`, the same JSON as the REST route. `preview_policy` shows what a policy would change on the latest crawl, `compare_policy` compares it rule by rule with the saved one, and `set_policy` saves it. Each counts as one API call.

See [mcp.md](mcp.md).

## REST

| Route | What it does |
|---|---|
| `GET /api/v1/sites/{site}/policy` | `{ site_id, policy, report, crawl_id, updated_at }`. With `Accept: application/toml`, the policy as a `codoseo.toml` file. |
| `PUT /api/v1/sites/{site}/policy` | Replaces the policy with a `codoseo.toml` (`Content-Type: application/toml`) or the JSON `policy` object, exactly as saving it in the app does. A policy that is not valid is a `400` listing every problem. Answers like `GET`, plus `saved` (the outcome, the history row, the health and GEO scores before and after, AI incidents opened and resolved). |
| `POST /api/v1/sites/{site}/policy/preview` | What a policy (JSON or TOML) would change on the latest crawl, without saving it. |

`report` has one entry per rule in `rules` (`id`: `address`, `index`, `params`, `must_work`, `ai_search` ... `ai_answer_limits`; `status` (`met`, `gaps`, `partly`, `not_checked`, `limited_effect`), `gaps`, `text`, sample `urls`, and for `index` and `must_work` the `parts`, one per index rule or must-work page: `subject`, `note`, `status`, `gaps`, `text`), the `gaps` themselves (`rule`, `kind` `ai` or `seo`, `severity`, `text`, `subject`), and the `seo_gaps` and `ai_gaps` counts. The SEO rules are compared with the newest finished crawl's stored pages. A `PUT` with more must-work pages than the plan allows is a `400`. See [api.md](api.md).

## Storage

The policy is stored as JSON on the site (`sites.policy`); each finished crawl keeps its comparison in `crawls.summary.policy`. The 0019 migration filled it from the AI access intent (`sites.ai_intent`) and the fetch check switch (`sites.ai_probes`); those two columns are still written with every save for one release, so rolling back reads the same intent. Changes are kept in `policy_history`.
