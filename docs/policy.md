# Site policy

A site policy says how a site is meant to work for search engines and AI bots, so CodoSEO can tell a mistake from a choice. Every crawl compares the site with it: where the site disagrees, that is a gap and an issue; where you say something is on purpose, it stops being flagged. CodoSEO never changes your site or its robots.txt.

The policy has two parts:

- **AI rules** (evaluated now): what each kind of AI bot should get, per-bot exceptions, the answer limits you use on purpose, and whether the synthetic fetch checks run. They are what [AI access](ai-access.md) used to call the intent.
- **SEO rules** (the format is stable, evaluation arrives in the next release): the preferred address, what search engines should index, URL parameters that don't change the page, pages that must always work, check thresholds and exceptions. A policy can hold them today: they are imported, exported and kept.

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
| **SEO rules** (kept, evaluated from the next release) | | |
| `seo.address.host` | not checked | `apex` (`https://example.com`) or `www`. |
| `seo.address.trailing_slash` | `without` | `without`, `with` or `either`. |
| `seo.index` | none | Path rules, top to bottom, the first match wins: `pattern` and `stance` (`index`, `keep_out`, `none`). At most 50. `keep_out` expects a `noindex` tag or header: robots.txt alone stops crawling, not indexing. |
| `seo.index_default` | `none` | The stance for pages no rule matches. |
| `seo.ignore_params` | none | Query parameters that don't change the page (`utm_*`); URLs differing only by them should canonicalise to the clean URL. At most 30. |
| `seo.pagination_param` | none | The pagination parameter: each page canonicalises to itself. |
| `seo.must_work` | none | Paths that must answer 200 and be indexable on every crawl. The home page and starred pages are added when evaluated. |
| `seo.thresholds.title_min`, `title_max` | 30, 60 | Title length in characters. |
| `seo.thresholds.description_min`, `description_max` | 70, 160 | Meta description length in characters. |
| `seo.thresholds.thin_words` | 200 | Fewer words than this is a thin page. |
| `seo.thresholds.slow_ms` | 1000 | A slower time to first byte is a slow page. |
| `seo.thresholds.max_redirect_hops` | 1 | More hops than this is a redirect chain. |
| `seo.thresholds.image_alt` | `true` | Flag images without alt text. |
| `seo.exceptions` | none | Checks that don't apply to part of the site: `check` (a check slug), `scope` (a pattern), `note`. At most 100. |

Patterns start with `/`. `*` matches any run of characters, `/` included. Without a `?` a pattern is matched against the path; with one, against the path and the query: `/collections/*?page=*`.

## On a monitored site

**Site policy**, under THIS SITE in the sidebar after Crawls (`G` then `P`), has an **AI rules** tab and an **SEO rules** tab (which, for now, lists what is coming). On the AI tab each kind of bot has an Allow / Block / No preference control with a line comparing it with the latest crawl: **Met**, **N gaps** (each bot named: blocked by robots.txt, blocked by your firewall, or getting in when the policy says Block), **Not checked** (no preference, or nothing to compare with yet) or **Limited effect** (blocking bots that don't follow robots.txt only works at the firewall). The disclosure under it sets one bot differently and shows how each bot fares now. **Answer limits you use on purpose** shows where each directive is used, and flags one in use that you untick. "Set a policy for this bot" on the AI access Bots tab links straight to the bot.

The right rail updates as you edit, before you save: **Your rules vs crawl #N** (rules met and the gaps) and **robots.txt that matches**: your robots.txt as that crawl read it, rewritten so the AI bots get what the policy says (blanket blocks of allowed bots removed, one group for the bots to block, the matching `Content-Signal` line in the `*` group), with a line diff against your file. **Copy file** copies it, **Copy as prompt** copies a prompt for a coding agent. It is a suggestion: a CDN-managed robots.txt may override it.

**Save policy** re-checks the latest crawl at once and rescores it; issues it opens or resolves send no alerts. **Reset to defaults** resets the tab's rules. **Export** downloads the policy as `codoseo.toml`; **Import a codoseo.toml** replaces the whole policy with a file and shows what is wrong with one that isn't valid. **History** lists every change: when, who (an account, or an API key), where (the policy screen, the API, an import, Mark intended on an AI access issue, the upgrade that moved the intent here), the changed keys, and the newest crawl's health and GEO scores before and after, so a score jump can be traced to a policy edit. History is kept as long as the plan keeps crawl history.

The old intent address, `/s/{site}/ai-access/intent`, redirects to the AI tab.

## CLI

| Command | What it does |
|---|---|
| `codoseo policy init [--from URL] [-o FILE] [--force]` | Writes `codoseo.toml` with every key commented out at its default. `--from URL` fetches the URL and fills in the preferred address (www or not from where it redirects; the trailing slash from its path, `without` for a bare home page). Never overwrites a file without `--force`. |
| `codoseo policy validate [FILE]` | Checks `FILE` (default `./codoseo.toml`): exit 0 and its hash when valid, unknown keys as warnings on stderr; exit 1 with one line per problem when not. |
| `codoseo policy export --site ID --api-key KEY [--base-url URL] [-o FILE]` | Downloads a monitored site's policy as TOML. The key can come from `CODOSEO_API_KEY`; `--base-url` points at a self-hosted install. |
| `codoseo policy import --site ID --api-key KEY [--base-url URL] [FILE]` | Validates `FILE` (default `./codoseo.toml`) locally, then replaces the site's policy with it. Recorded in the site's history as made through the API. |

`codoseo crawl` reads `./codoseo.toml` when there is one, or `--policy FILE`; `--no-policy` ignores it. AI access is then judged under the file's AI rules instead of the default intent, so `--fail-on geo` and `bots_not_blocked` gates follow your policy, and the audit JSON gains a `policy` field: `{ file, hash, policy, report }`, the report being the same rule results the web app shows. `format_version` stays `1`. An invalid file stops the command before it crawls (exit 2). `codoseo check` takes the same flags and validates the file; one page has no AI access report, so nothing else changes yet.

## MCP

- Local server (`codoseo mcp`): `get_policy` with an optional `path` (default `codoseo.toml` in the server's working directory) returns `policy` (every default filled in), the file's `hash` and `warnings`. An invalid file fails with one line per problem.
- Hosted server with an API key: `get_policy` with a `site_id` returns `{ site_id, policy, report, crawl_id, updated_at }`, the same JSON as the REST route. It counts as one API call.

See [mcp.md](mcp.md).

## REST

| Route | What it does |
|---|---|
| `GET /api/v1/sites/{site}/policy` | `{ site_id, policy, report, crawl_id, updated_at }`. With `Accept: application/toml`, the policy as a `codoseo.toml` file. |
| `PUT /api/v1/sites/{site}/policy` | Replaces the policy with a `codoseo.toml` (`Content-Type: application/toml`) or the JSON `policy` object, exactly as saving it in the app does. A policy that is not valid is a `400` listing every problem. Answers like `GET`. |

`report` has one entry per rule in `rules` (`id` such as `ai_search` or `ai_answer_limits`, `status` (`met`, `gaps`, `partly`, `not_checked`, `limited_effect`), `gaps`, `text`, sample `urls`), the `gaps` themselves (`rule`, `kind` `ai` or `seo`, `severity`, `text`, `subject`), and the `seo_gaps` and `ai_gaps` counts. See [api.md](api.md).

## Storage

The policy is stored as JSON on the site (`sites.policy`). The 0019 migration filled it from the AI access intent (`sites.ai_intent`) and the fetch check switch (`sites.ai_probes`); those two columns are still written with every save for one release, so rolling back reads the same intent. Changes are kept in `policy_history`.
