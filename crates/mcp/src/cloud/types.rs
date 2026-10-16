//! JSON shapes of the cloud API: what `GET /api/v1/...` and the keyed MCP tools return. Times are
//! RFC 3339 (UTC). Check ids are the stable slugs (`title_missing`).

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use codoseo_core::change::ChangeKind;
use codoseo_core::check::{CheckId, Severity};
use codoseo_core::page::Indexability;

use crate::types::{FailingCheck, UrlRow};

/// One monitored site, as `list_sites` returns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteInfo {
    pub id: Uuid,
    pub domain: String,
    pub start_url: String,
    pub monitoring_active: bool,
    /// `weekly`, `daily`, or none when the site isn't crawled on a schedule.
    pub schedule: Option<String>,
    /// Health score (0 to 100) of the latest finished crawl.
    pub health_score: Option<u8>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_crawled_at: Option<OffsetDateTime>,
}

/// A site's latest finished crawl.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrawlHealth {
    pub crawl_id: Uuid,
    /// 1 for the site's first crawl.
    pub number: i64,
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
    pub health_score: Option<u8>,
    pub checks_passed: Option<u16>,
    pub checks_total: Option<u16>,
    pub pages_crawled: u32,
    /// Human words, e.g. "completed" or "page limit reached".
    pub stop_reason: String,
    /// A stable name to branch on: `completed`, `page_limit`, `time_limit`, `unreachable`,
    /// `blocked` or `robots_blocked`.
    pub stop_code: String,
    /// Most severe first, at most 15, each with 3 example URLs.
    pub failing_checks: Vec<FailingCheck>,
    /// Failing checks beyond the 15 listed.
    pub more_failing_checks: u16,
}

/// A crawl that is queued or running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveCrawl {
    pub crawl_id: Uuid,
    pub number: i64,
    /// `queued` or `running`.
    pub status: String,
    pub pages_done: Option<u32>,
}

/// `get_site_health`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SiteHealth {
    pub id: Uuid,
    pub domain: String,
    pub start_url: String,
    pub monitoring_active: bool,
    pub schedule: Option<String>,
    /// When the scheduler crawls next; none when the site has no schedule or monitoring is off.
    #[serde(with = "time::serde::rfc3339::option")]
    pub next_crawl_at: Option<OffsetDateTime>,
    /// None until the first crawl has finished.
    pub latest_crawl: Option<CrawlHealth>,
    /// Set while a crawl is queued or running.
    pub active_crawl: Option<ActiveCrawl>,
    /// The site's audit screen in the web app.
    pub audit_url: String,
}

/// `get_issue_urls`: one page of the pages that fail a check in the latest finished crawl.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IssueUrlsPage {
    pub site_id: Uuid,
    pub check: CheckId,
    pub title: String,
    /// The crawl read; none when the site has no finished crawl yet (then `urls` is empty).
    pub crawl_number: Option<i64>,
    /// Pages failing the check in that crawl.
    pub total: u32,
    pub limit: u32,
    pub offset: u32,
    pub urls: Vec<UrlRow>,
    /// Pass as `offset` for the next page; none on the last page.
    pub next_offset: Option<u32>,
}

/// One hop of a redirect chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedirectHop {
    pub status: u16,
    /// The URL that answered with `status`.
    pub url: String,
}

/// A page's issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageIssue {
    pub check: CheckId,
    pub title: String,
    pub severity: Severity,
}

/// `get_page`: what the latest finished crawl stored about one URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageInfo {
    pub site_id: Uuid,
    pub crawl_number: i64,
    pub url: String,
    pub status: u16,
    /// Every redirect hop in order.
    pub redirect_chain: Vec<RedirectHop>,
    pub redirect_target: Option<String>,
    pub response_ms: Option<u32>,
    pub size_bytes: Option<u64>,
    pub content_type: Option<String>,
    pub depth: Option<u32>,
    pub in_sitemap: bool,
    pub indexability: Indexability,
    pub title: Option<String>,
    pub meta_description: Option<String>,
    pub meta_robots: Option<String>,
    pub x_robots_tag: Option<String>,
    pub canonical: Option<String>,
    pub h1: Vec<String>,
    pub h2: Vec<String>,
    pub word_count: Option<u32>,
    pub inlinks: u32,
    pub outlinks_internal: u32,
    pub outlinks_external: u32,
    /// The checks this page fails, most severe first.
    pub issues: Vec<PageIssue>,
}

/// One change between the latest finished crawl and the one before it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeInfo {
    pub kind: ChangeKind,
    pub severity: Severity,
    /// The page the change is about; none for site-wide changes (robots.txt, sitemap, spike).
    pub url: Option<String>,
    /// The old value, cut to [`MAX_CHANGE_TEXT`] characters (with a final `…`) when longer.
    pub before: String,
    /// The new value, cut the same way.
    pub after: String,
}

/// Longest `before` / `after` a change carries.
pub const MAX_CHANGE_TEXT: usize = 300;

/// `text`, cut to [`MAX_CHANGE_TEXT`] characters with a final `…` when it is longer.
pub fn clip_change_text(text: &str) -> String {
    clip(text, MAX_CHANGE_TEXT)
}

/// `text`, cut to `max` characters with a final `…` when it is longer.
pub fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        None => text.to_owned(),
        Some((at, _)) => {
            let cut = text[..at].char_indices().last().map_or(0, |(i, _)| i);
            format!("{}…", &text[..cut])
        }
    }
}

/// `get_changes`: the latest finished crawl's changes, most severe first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangesPage {
    pub site_id: Uuid,
    /// The crawl read; none when the site has no finished crawl yet.
    pub crawl_number: Option<i64>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub crawl_finished_at: Option<OffsetDateTime>,
    /// Changes matching the severity filter, which may be more than `changes` holds.
    pub total: u32,
    pub limit: u32,
    pub offset: u32,
    pub changes: Vec<ChangeInfo>,
    /// Pass as `offset` for the next page; none on the last page.
    pub next_offset: Option<u32>,
}

/// `get_ai_access`: the latest AI access report of a site, summarised, with its open incidents.
/// Before the first crawl after AI access shipped there is no report: `checked_at` is none and
/// `note` says when it will appear.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiAccessInfo {
    pub site_id: Uuid,
    /// When the report was written (the crawl that made it); none without a report.
    #[serde(with = "time::serde::rfc3339::option")]
    pub checked_at: Option<OffsetDateTime>,
    pub crawl_id: Option<Uuid>,
    /// Said only when there is no report to show.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub robots: Option<AiRobotsInfo>,
    /// Unresolved incidents, most severe first.
    pub open_incidents: Vec<AiIncidentInfo>,
    /// One entry per known AI bot; empty when robots.txt could not be read.
    pub bots: Vec<AiBotInfo>,
    /// One entry per AI engine, counting the HTML pages among the important ones.
    pub engines: Vec<AiEngineInfo>,
    /// What the site declares about AI use. Declared, not enforced.
    pub declared: codoseo_geo::report::Declared,
    /// Whether the synthetic fetch check runs after each crawl (the owner can switch it off).
    pub probes_enabled: bool,
    /// The latest synthetic fetch check; absent when it is switched off, or the report has none
    /// (an older report, a quick audit, a crawl whose robots.txt gave no verdicts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probes: Option<AiProbesInfo>,
    /// What the important pages hold in their HTML as served; absent when the report has none
    /// (an older report, or no important page is HTML).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<AiContentInfo>,
    /// `/llms.txt`: informational, never scored. Absent when the report has none (an older
    /// report, or the file was not fetched).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llms_txt: Option<AiLlmsTxtInfo>,
    /// Why `/llms.txt` was not fetched when the crawl meant to: `robots_disallowed`, `refused` or
    /// `deadline`. Absent when it was fetched, or the report has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llms_txt_skipped: Option<codoseo_core::output::LlmsSkip>,
    /// The home page asked for with `Accept: text/markdown`: `state` (negotiated, alternate,
    /// not_served or unreachable), what came back and the Markdown alternate it advertises.
    /// Advisory. Absent when the report has none (an older report, or it was not asked).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markdown: Option<codoseo_geo::markdown::MarkdownReport>,
}

/// What the important HTML pages hold in their HTML as served, before any JavaScript runs. AI
/// crawlers like GPTBot, ClaudeBot and PerplexityBot don't run JavaScript (Vercel and MERJ, 2024),
/// so a client-side app's empty shell is all they read. Evidence grade A-: read from the pages,
/// against a published measurement of how the bots fetch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiContentInfo {
    /// Always `A-`.
    pub grade: codoseo_geo::findings::Grade,
    /// Important HTML pages that were read.
    pub pages_checked: u32,
    /// Pages whose verdict is `shell` or `likely_shell`.
    pub shells: u32,
    /// Shells first, then the other pages in the report's order; at most [`MAX_AI_CONTENT_PAGES`].
    pub pages: Vec<AiContentPage>,
    /// Pages left out of `pages`.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub more: u32,
}

/// The most pages `content.pages` lists.
pub const MAX_AI_CONTENT_PAGES: usize = 30;

fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiContentPage {
    pub url: String,
    /// `shell` (no text in the HTML as served and a script-built app's markers), `likely_shell`,
    /// `thin` (few words, no shell markers; informational) or `present`.
    pub verdict: codoseo_geo::content::ContentVerdict,
    /// The shell markers found: `mount_empty`, `noscript_js`, `script_heavy`, `many_scripts`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signals: Vec<codoseo_geo::content::ShellSignal>,
    /// Words of text in the HTML as served.
    pub words: u32,
    pub title: bool,
    pub h1: bool,
    pub description: bool,
    pub canonical: bool,
    /// At least one valid JSON-LD block.
    pub jsonld: bool,
    pub links_internal: u32,
}

/// `/llms.txt` as the last crawl found it. Informational: the file is optional and never scored,
/// and a missing one (`absent`, `soft_not_found`) is nothing to fix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiLlmsTxtInfo {
    /// Always true.
    pub informational: bool,
    /// `absent` (a 4xx), `soft_not_found` (a web page where the file should be), `available` or
    /// `unavailable` (a 5xx, 429 or no answer).
    #[serde(flatten)]
    pub report: codoseo_geo::llms::LlmsTxtReport,
}

/// The synthetic fetch check: a few important pages fetched with each AI search and user-triggered
/// bot's documented user agent, from CodoSEO's address, and compared with CodoSEObot fetching the
/// same page at the same moment. Synthetic: real bots come from their own addresses and may be
/// treated differently, so what it finds is evidence grade B.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiProbesInfo {
    /// Always true.
    pub synthetic: bool,
    /// Always `B`: observed by CodoSEO against its own baseline, not documented by the operator.
    pub grade: codoseo_geo::findings::Grade,
    /// Pages probed, that is fetched as CodoSEObot; each bot's own `urls_probed` counts the ones
    /// its user agent was actually sent to.
    pub urls_probed: u32,
    /// The `From` header every probe carried.
    pub from: String,
    /// Why the check stopped before its last planned page (`budget`: the time ran out, or `request_cap`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<codoseo_geo::probe::ProbeStop>,
    /// One entry per probed bot, in probe order.
    pub bots: Vec<AiProbeBotInfo>,
    /// Crawling AI bots that were not probed, with why.
    pub not_probed: Vec<AiNotProbedInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiProbeBotInfo {
    pub token: String,
    pub purpose: codoseo_geo::Purpose,
    /// What the owner wants from this bot; only `allow` bots are reported as incidents.
    pub intent: codoseo_geo::Stance,
    /// Compared with CodoSEObot: `same`, `blocked`, `challenged`, `diverted` (redirected
    /// elsewhere), `degraded` (a much smaller, different page), `throttled`, `inconclusive` or
    /// `skipped_robots`. A bot treated worse on any page shows that page's result.
    pub result: String,
    /// The incident subject a worse result groups under (`challenge_cloudflare`, `blocked_403`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    /// The bot manager behind a `challenged` result (`cloudflare`, `datadome`, `generic`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    /// The status of a `blocked` result; absent when the connection was dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Probed pages where it was treated worse than CodoSEObot.
    pub urls_affected: u32,
    /// Pages actually requested with its user agent: robots.txt skips, and pages whose baseline
    /// gave nothing to compare with, left out.
    pub urls_probed: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiNotProbedInfo {
    pub token: String,
    /// `no_user_agent` (its operator documents no full user agent), `verified_search_engine` or
    /// `not_probed_purpose` (training and ads crawlers, agents).
    pub reason: codoseo_geo::probe::SkipReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRobotsInfo {
    /// The HTTP status of /robots.txt; none when it could not be fetched.
    pub status: Option<u16>,
    pub availability: codoseo_geo::robots::RobotsAvailability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiIncidentInfo {
    pub id: i64,
    /// One of `bots_blocked`, `bots_not_blocked`, `robots_unavailable`, `answers_restricted`,
    /// `fetch_blocked` (the synthetic fetch check: a firewall or CDN blocks or challenges bots
    /// the owner wants; evidence grade B), `content_missing` (important pages are empty app
    /// shells in their HTML as served; evidence grade A-), `llms_txt_unavailable` (an llms.txt
    /// the site served before now fails; informational), `content_signals_missing` (robots.txt
    /// declares no Content-Signal; advisory, never alerted) or `markdown_unavailable` (the home
    /// page has no Markdown version for AI agents; advisory, never alerted).
    pub kind: String,
    /// The purpose or directive the incident is about, or for `fetch_blocked` the cause
    /// (`challenge_cloudflare`, `blocked_403`, ...); empty for the robots.txt one.
    pub subject: String,
    pub severity: Severity,
    pub title: String,
    pub summary: String,
    #[serde(with = "time::serde::rfc3339")]
    pub opened_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub last_seen_at: OffsetDateTime,
    /// The same time as `opened_at`, under the name the incident export uses.
    #[serde(with = "time::serde::rfc3339")]
    pub first_seen: OffsetDateTime,
    /// How strong the evidence for this kind of finding is: `A` (the operator documents it),
    /// `A-` (read from the site's own HTML as served), `B` (CodoSEO's synthetic check), `C`
    /// (a heuristic) or `X` (undocumented).
    pub grade: codoseo_geo::findings::Grade,
    /// The documents the finding rests on.
    pub sources: Vec<String>,
    /// What the owner and CodoSEO are doing about it now.
    pub state: AiIncidentState,
    /// Until when it is snoozed; left out when it is not.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub snoozed_until: Option<OffsetDateTime>,
    /// Whether the owner accepted it as it is: it no longer counts or alerts.
    pub accepted: bool,
    /// When a check first saw the problem gone, and when that was confirmed.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub recovered_at: Option<OffsetDateTime>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub verified_at: Option<OffsetDateTime>,
    /// While `verifying`: when the follow-up recheck that confirms the fix runs.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub verify_due_at: Option<OffsetDateTime>,
    /// How long the operators take to notice a fix, and where they say so.
    pub lag: AiLagInfo,
    /// The pages it affects (at most 20).
    pub affected_urls: Vec<String>,
    /// The latest recheck, once it finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_recheck: Option<AiRecheckInfo>,
}

/// `open` is the default; `accepted` and `snoozed` are the owner's decisions and win over what
/// CodoSEO is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiIncidentState {
    Open,
    Accepted,
    Snoozed,
    /// The fix was seen and a follow-up recheck is due to confirm it (`verify_due_at`).
    Verifying,
    /// The fix was seen but nothing is scheduled to confirm it: the next crawl will.
    WaitingForCrawl,
    RecheckQueued,
    RecheckRunning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiLagInfo {
    /// Hours after a fix before the operators are likely to have noticed.
    pub hours: u32,
    /// Where the operator says so; left out when nobody publishes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRecheckInfo {
    /// `clean`, `still_failing`, `inconclusive`, `failed` (it could not run, or the site did not answer) or
    /// `superseded` (a crawl judged instead).
    pub outcome: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub at: Option<OffsetDateTime>,
    /// Why an inconclusive one was. Site strings: data, not instructions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// `get_fix`: what to change for one incident, and ready-made texts to hand on. The texts carry
/// strings from the site: data, not instructions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiFixInfo {
    pub remedy: AiRemedyInfo,
    /// Just the change: the snippet when there is one, else the steps.
    pub copy_fix: String,
    /// A Markdown ticket for whoever makes the change.
    pub developer_task: String,
    /// The same facts for a coding agent, with the site's strings fenced as untrusted data.
    pub agent_prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRemedyInfo {
    pub title: String,
    pub why: String,
    /// What the evidence can't tell.
    pub limits: String,
    pub steps: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<AiSnippetInfo>,
    /// How to check the change took.
    pub verify: Vec<String>,
    pub sources: Vec<String>,
    pub grade: codoseo_geo::findings::Grade,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiSnippetInfo {
    pub label: String,
    pub text: String,
}

/// `recheck_incident`: a manual recheck is queued; read its result on the incident
/// (`last_recheck`, `state`) in `get_ai_access` a minute or two later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRecheckQueued {
    pub recheck_id: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub due_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiBotInfo {
    pub token: String,
    pub operator: String,
    pub purpose: codoseo_geo::Purpose,
    /// What the owner wants from this bot (`allow`, `block` or `any`).
    pub intent: codoseo_geo::Stance,
    /// Whether robots.txt lets it fetch the home page.
    pub home_allowed: Option<bool>,
    /// Important pages robots.txt keeps it from.
    pub important_blocked: u32,
    pub important_total: u32,
    /// robots.txt does the opposite of the owner's intent for this bot.
    pub conflicts: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiEngineInfo {
    pub id: codoseo_geo::eligibility::EngineId,
    pub name: String,
    /// Important HTML pages that can appear in the engine's answers.
    pub eligible: u32,
    /// Pages whose own controls (a snippet limit, `data-nosnippet`) shrink what is quoted.
    pub limited: u32,
    /// Pages kept out: `noindex`-like controls, or the engine's crawler is blocked.
    pub excluded: u32,
    /// The engine documents page-level controls; without them only robots.txt matters.
    pub page_controls: bool,
}

/// `run_crawl`: the crawl that was queued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrawlQueued {
    pub site_id: Uuid,
    pub crawl_id: Uuid,
    pub number: i64,
    /// Always `queued`; poll `get_site_health` (`active_crawl`) to follow it.
    pub status: String,
}

/// `GET /api/v1/usage`: today's (UTC) API calls against the plan's allowance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub calls_today: u32,
    /// None on plans without a limit (self-hosted).
    pub limit: Option<u32>,
    pub remaining: Option<u32>,
    /// The next 00:00 UTC, when the count starts again.
    #[serde(with = "time::serde::rfc3339")]
    pub resets_at: OffsetDateTime,
}

/// The no-key tools' view of a quick audit (`quick_audit` and `get_audit`): the audit is still
/// going, it finished with a report, or it could not produce one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum QuickAuditState {
    /// Queued or crawling. Call `get_audit` with the id again in a few seconds.
    Running {
        audit_id: Uuid,
        /// Pages crawled so far, once the crawl has started.
        pages_done: Option<u32>,
        message: String,
    },
    Done(Box<QuickAuditSummary>),
    /// The audit ended without a report (site unreachable or blocked, nothing to audit).
    Failed {
        audit_id: Uuid,
        reason: String,
    },
}

/// A finished quick audit, agent-sized: [`QuickAuditSummary::fit`] keeps it under
/// [`MAX_SUMMARY_BYTES`] however many checks fail and however long their URLs are.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuickAuditSummary {
    pub audit_id: Uuid,
    pub domain: String,
    pub start_url: String,
    pub health_score: Option<u8>,
    pub checks_passed: Option<u16>,
    pub checks_total: Option<u16>,
    pub pages_crawled: u32,
    /// Human words, e.g. "completed" or "page limit reached" (the audit crawls up to 100 pages).
    pub stop_reason: String,
    /// A stable name to branch on: `completed`, `page_limit`, `time_limit`, ...
    pub stop_code: String,
    /// Most severe first, each with a count and up to 3 example URLs.
    pub failing_checks: Vec<AuditFailingCheck>,
    /// Failing checks not listed (cut by the size cap). `get_issue_urls` still takes any check.
    pub more_failing_checks: u16,
    /// The same report as a web page, to hand to the user.
    pub report_url: String,
    /// One line about monitoring this site, pointing at `start_monitoring`.
    pub note: String,
}

/// A failing check in a quick audit summary. The example URLs are text, since the size cap may
/// shorten a very long one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditFailingCheck {
    pub check: CheckId,
    pub title: String,
    pub severity: Severity,
    pub count: u32,
    pub example_urls: Vec<String>,
}

impl From<FailingCheck> for AuditFailingCheck {
    fn from(f: FailingCheck) -> AuditFailingCheck {
        AuditFailingCheck {
            check: f.check,
            title: f.title,
            severity: f.severity,
            count: f.count,
            example_urls: f.example_urls.iter().map(|u| u.to_string()).collect(),
        }
    }
}

/// The longest an example URL, the start URL or a stop reason may be in a summary.
pub const MAX_SUMMARY_TEXT: usize = 300;

/// The most JSON bytes a quick audit summary may take (spec section 9: an agent-sized answer).
pub const MAX_SUMMARY_BYTES: usize = 4096;

impl QuickAuditSummary {
    /// Keeps the summary under [`MAX_SUMMARY_BYTES`] whatever the site looks like: long texts
    /// (each example URL, the start URL, the stop reason) are cut to [`MAX_SUMMARY_TEXT`]
    /// characters, then the least severe failing checks are dropped (counted in
    /// `more_failing_checks`), all of them if need be. What is left (ids, a domain of at most
    /// 253 characters, the score and counts) is far below the cap.
    pub fn fit(mut self) -> QuickAuditSummary {
        self.start_url = clip(&self.start_url, MAX_SUMMARY_TEXT);
        self.stop_reason = clip(&self.stop_reason, MAX_SUMMARY_TEXT);
        for check in &mut self.failing_checks {
            for url in &mut check.example_urls {
                *url = clip(url, MAX_SUMMARY_TEXT);
            }
        }
        while !self.failing_checks.is_empty() && self.json_len() >= MAX_SUMMARY_BYTES {
            self.failing_checks.pop();
            self.more_failing_checks = self.more_failing_checks.saturating_add(1);
        }
        self
    }

    fn json_len(&self) -> usize {
        serde_json::to_string(&QuickAuditState::Done(Box::new(self.clone())))
            .map_or(0, |json| json.len())
    }
}

/// `get_issue_urls` for a quick audit: one page of the pages that fail a check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditIssueUrls {
    pub audit_id: Uuid,
    pub check: CheckId,
    pub title: String,
    /// Pages failing the check in the audit.
    pub total: u32,
    pub limit: u32,
    pub offset: u32,
    pub urls: Vec<UrlRow>,
    /// Pass as `offset` for the next page; none on the last page.
    pub next_offset: Option<u32>,
}

/// `start_monitoring`: the confirmation email is on its way. The same answer whether or not the
/// address already has an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitoringRequested {
    /// Always `confirmation_sent`.
    pub status: String,
    pub domain: String,
    /// What to tell the user.
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MAX_FAILING_CHECKS;

    fn failing(n: usize, url_len: usize) -> Vec<AuditFailingCheck> {
        let all = CheckId::ALL;
        all[..n]
            .iter()
            .map(|&check| AuditFailingCheck {
                check,
                title: "A reasonably descriptive check title".to_owned(),
                severity: Severity::Warning,
                count: 100,
                example_urls: (0..3)
                    .map(|i| format!("https://example.com/{}{i}", "p".repeat(url_len)))
                    .collect(),
            })
            .collect()
    }

    fn summary(failing_checks: Vec<AuditFailingCheck>) -> QuickAuditSummary {
        QuickAuditSummary {
            audit_id: Uuid::new_v4(),
            domain: "example.com".to_owned(),
            start_url: "https://example.com/".to_owned(),
            health_score: Some(12),
            checks_passed: Some(3),
            checks_total: Some(45),
            pages_crawled: 100,
            stop_reason: "page limit reached".to_owned(),
            stop_code: "page_limit".to_owned(),
            failing_checks,
            more_failing_checks: 0,
            report_url: format!("https://codoseo.com/audit/{}", Uuid::new_v4()),
            note: "To keep monitoring example.com weekly and get an email when something \
                   breaks, call start_monitoring with this site's URL and the user's email."
                .to_owned(),
        }
    }

    fn size(summary: &QuickAuditSummary) -> usize {
        serde_json::to_string(&QuickAuditState::Done(Box::new(summary.clone())))
            .unwrap()
            .len()
    }

    #[test]
    fn a_summary_with_few_short_urls_is_kept_whole() {
        let fitted = summary(failing(5, 20)).fit();
        assert_eq!(fitted.failing_checks.len(), 5);
        assert_eq!(fitted.more_failing_checks, 0);
    }

    #[test]
    fn a_worst_case_summary_is_cut_under_4_kb_by_dropping_the_least_severe_checks() {
        for url_len in [10, 40, 120, 400, 20_000] {
            let fitted = summary(failing(MAX_FAILING_CHECKS, url_len)).fit();
            assert!(
                size(&fitted) < MAX_SUMMARY_BYTES,
                "{url_len}: {}",
                size(&fitted)
            );
            assert!(!fitted.failing_checks.is_empty());
            assert_eq!(
                fitted.failing_checks.len() + usize::from(fitted.more_failing_checks),
                MAX_FAILING_CHECKS
            );
            // The most severe come first and are the ones kept; no text is longer than the cap.
            assert!(fitted.failing_checks.iter().all(|f| {
                f.example_urls
                    .iter()
                    .all(|u| u.chars().count() <= MAX_SUMMARY_TEXT)
            }));
            let kept: Vec<CheckId> = fitted.failing_checks.iter().map(|f| f.check).collect();
            assert_eq!(kept, CheckId::ALL[..kept.len()]);
        }
    }

    #[test]
    fn nothing_a_site_controls_can_push_a_summary_past_the_cap() {
        let mut huge = summary(failing(MAX_FAILING_CHECKS, 50_000));
        huge.start_url = format!("https://example.com/{}", "s".repeat(100_000));
        huge.stop_reason = "site unreachable: ".to_owned() + &"x".repeat(100_000);
        huge.domain = "d".repeat(253);
        let fitted = huge.fit();
        assert!(size(&fitted) < MAX_SUMMARY_BYTES, "{}", size(&fitted));
        assert!(fitted.start_url.chars().count() <= MAX_SUMMARY_TEXT);
        assert!(fitted.start_url.ends_with('…'));
        assert!(fitted.stop_reason.chars().count() <= MAX_SUMMARY_TEXT);
        // Even checks with nothing but huge URLs are dropped down to none when they must be.
        let mut one = summary(failing(1, 10));
        one.failing_checks[0].title = "t".repeat(10_000);
        let fitted = one.fit();
        assert!(fitted.failing_checks.is_empty());
        assert_eq!(fitted.more_failing_checks, 1);
        assert!(size(&fitted) < MAX_SUMMARY_BYTES);
    }

    #[test]
    fn the_audit_state_is_tagged_by_status() {
        let id = Uuid::new_v4();
        let running = QuickAuditState::Running {
            audit_id: id,
            pages_done: None,
            message: "wait".to_owned(),
        };
        let json = serde_json::to_value(&running).unwrap();
        assert_eq!(json["status"], "running");
        assert_eq!(json["audit_id"], id.to_string());
        let done = serde_json::to_value(QuickAuditState::Done(Box::new(summary(vec![])))).unwrap();
        assert_eq!(done["status"], "done");
        assert_eq!(done["domain"], "example.com");
        let back: QuickAuditState = serde_json::from_value(done).unwrap();
        assert!(matches!(back, QuickAuditState::Done(_)));
    }

    #[test]
    fn change_text_is_cut_to_300_characters_with_an_ellipsis() {
        assert_eq!(clip_change_text("short"), "short");
        let exact = "a".repeat(MAX_CHANGE_TEXT);
        assert_eq!(clip_change_text(&exact), exact);
        let long = "é".repeat(MAX_CHANGE_TEXT + 50);
        let clipped = clip_change_text(&long);
        assert_eq!(clipped.chars().count(), MAX_CHANGE_TEXT);
        assert!(clipped.ends_with('…'));
        assert!(clipped.starts_with("éé"));
    }
}

/// A site's policy, as `get_policy` and `GET /api/v1/sites/{site}/policy` return it: the policy
/// (`codoseo_policy::SitePolicy`, the same keys as a `codoseo.toml`'s `[policy]` table), the
/// latest crawl measured against it, and when it last changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyInfo {
    pub site_id: Uuid,
    pub policy: codoseo_policy::SitePolicy,
    /// The rules against the latest crawl's AI access report. Only the AI rules are evaluated
    /// so far; without a report every rule is `not_checked`.
    pub report: codoseo_policy::PolicyReport,
    /// The crawl the report compares with; none before the first.
    pub crawl_id: Option<Uuid>,
    /// When the policy last changed; none while it never has.
    #[serde(with = "time::serde::rfc3339::option")]
    pub updated_at: Option<OffsetDateTime>,
}
