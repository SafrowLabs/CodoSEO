# Scores

Every finished crawl of a monitored site has three scores out of 100:

- **SEO**, the health score: how the site does on the 44 SEO checks.
- **GEO**: how ready the site is for AI products, from the [AI access](ai-access.md) checks and a few SEO checks that help AI understand pages.
- **Overall**: the two blended, `round(0.6 × SEO + 0.4 × GEO)`.

No-signup audits have only the SEO score: they have no AI access evaluation. Crawls finished before GEO and Overall were introduced have no GEO or Overall score, so the trend skips them.

## SEO

100 minus the cost of every failing check, never below 0. A failing check costs `cap × (0.25 + 0.75 × share)`: the cap is 15 for a Critical check, 5 for a Warning and 1 for a Notice, and the share is the part of the crawled pages it fails on (all of it for a site-wide check). One failing page costs a quarter of the cap, every page all of it.

## GEO

The GEO score has two tiers.

**Essential, 70 points: can AI products reach and read the site?**

| Check | Passes when |
|---|---|
| robots.txt reachable | robots.txt answers (a 4xx counts: it means "no rules") |
| Search and answer bots allowed | the AI search and answer bots you want are not blocked |
| Other bots match your choice | bots you want kept out (training, for example) are blocked |
| Answers not restricted | no page markup takes your pages out of an engine's AI answers |
| Fetch as each bot works | a fetch with each bot's user agent is not blocked, challenged or diverted |
| Text without JavaScript | the important pages have their text in the HTML as served |

**Recommended, 30 points: is the site easy for AI to understand?**

| Check | Passes when |
|---|---|
| Structured data | the JSON-LD on every page parses |
| Titles | every page has a title |
| Descriptions | every page has a meta description |
| Sitemap | there is a sitemap and its URLs answer 200 |
| llms.txt | the site serves an `/llms.txt` |
| Content Signals | robots.txt has a `Content-Signal` line that sets `search`, `ai-input` or `ai-train` to yes or no ([details](ai-access.md#content-signals-and-markdown)) |
| Markdown for AI agents | the home page answers `Accept: text/markdown` with Markdown, or advertises a Markdown version that does |

Content Signals is skipped when robots.txt can't be read; Markdown when the home page did not load, or on crawls from before the check (as on crawls without site signals).

**Your choices don't cost points.** The checks are judged against your [intent](ai-access.md#intent). A bot you chose to block, or marked as intended, is not a failure.

**Checks that don't apply are skipped, never failed.** A skipped check counts on neither side. Examples: the fetch check when the probes did not run, the bot checks when robots.txt could not be read (that is already the robots.txt failure), the page checks when the crawl found no pages. If every check in a tier is skipped, the other tier counts for all 100 points.

**How points are shared.** Inside a tier, each applicable check gets a weight from the worst severity it can report: 15 for Critical, 5 for Warning, 1 for Notice, the same caps as the SEO score. The tier's points are split in proportion to those weights. When all checks apply, the weights are:

| Tier | Checks and weights | Total |
|---|---|---|
| Essential | robots.txt 15, search bots 15, answers 15, other bots 5, fetch 5, text 5 | 60 |
| Recommended | structured data 5, titles 5, descriptions 5, sitemap 5, llms.txt 1, Content Signals 1, Markdown 1 | 23 |

So with every check applicable, a robots.txt that can't be read costs 70 × 15/60 = 17.5 points, and a missing llms.txt, Content-Signal line or Markdown version costs 30 × 1/23 ≈ 1.3 each. llms.txt, Content Signals and Markdown are nice to have, so they weigh as a Notice; the Content Signals and Markdown findings are Notices that never alert.

A failing check loses `cap × (0.25 + 0.75 × share)` of its weight, at most all of it. The cap is the severity it actually failed with, so a block that only matters a little (a Notice) costs much less than a Critical one. The share is the part of the important pages the problem affects (of all crawled pages for the SEO checks). It is 1 for robots.txt and for blocked fetches, which affect the whole site. The score is 100 minus everything lost, rounded.

**Check counts.** "3 of 13 checks pass" counts the checks that were not skipped.

**What fixing a check gives back.** Each check's line in the breakdown says how many GEO points fixing it alone would give back. That is what it costs now. Overall gains 0.4 times that.

## Overall

`round(0.6 × SEO + 0.4 × GEO)`. Its check count adds the SEO checks and the applicable GEO checks ("39 of 57"). The four SEO checks the Recommended tier reads count in both.

## When the scores are computed

The GEO and Overall scores are written when a crawl finishes, in the same transaction as its AI access report and incidents, under your intent at that moment. When you change your intent ("Mark intended", or the intent settings), the newest finished crawl is scored again straight away. Older crawls keep the score they were finished with. A recheck does not change a crawl's score. It writes no report, so the next crawl scores what it found.
