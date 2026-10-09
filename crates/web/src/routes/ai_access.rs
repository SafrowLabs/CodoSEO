//! `/s/{site}/ai-access`: can AI assistants and search engines reach, read and quote the site?
//!
//! The screen reads what the worker stored: the latest access report (one per crawl), the
//! incidents its findings keep open, and the owner's intent, which decides whether a blocked bot
//! is a problem or a choice. `/ai-access/intent` edits the intent; "Mark intended" on an incident
//! changes it just enough for that finding to become a choice. Both re-evaluate the latest report
//! quietly: the owner made the change, so nobody is alerted.

use std::collections::{BTreeMap, BTreeSet};

use askama::Template;
use axum::extract::{Path, State};
use axum::http::{HeaderName, HeaderValue};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use codoseo_core::check::Severity;
use codoseo_geo::eligibility::{Cause, CauseSource, DirectiveSlug, Effect, engines};
use codoseo_geo::findings::{BotEvidence, Evidence, FindingKind, ROBOTS_DOC};
use codoseo_geo::report::{AccessReport, Declared, Reason};
use codoseo_geo::robots::{GroupMatch, MatchedRule, Pair, RobotsAvailability};
use codoseo_geo::{Bot, Honours, Intent, Purpose, Stance, registry};
use codoseo_store::crawls;
use codoseo_store::geo::{self, Incident, IntentError};
use codoseo_store::sites::Site;
use time::OffsetDateTime;
use url::Url;

use crate::auth::{CurrentUser, load_site};
use crate::error::AppError;
use crate::fmt;
use crate::layout::{Screen, Shell};
use crate::render::{Hx, ToastKind, html, toast};
use crate::routes::audit::site_id;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/s/{site}/ai-access", get(page))
        .route(
            "/s/{site}/ai-access/intent",
            get(intent_page).post(save_intent),
        )
        .route("/s/{site}/ai-access/intent/reset", post(reset_intent))
        .route(
            "/s/{site}/ai-access/incidents/{id}/intended",
            post(mark_intended),
        )
}

/// Resolved incidents listed under "Recently resolved".
const RESOLVED_SHOWN: i64 = 10;
/// URLs an incident card lists before "+N more".
const URLS_SHOWN: usize = 5;

/// Purposes in the order the screens list them.
pub const PURPOSES: [Purpose; 5] = [
    Purpose::Search,
    Purpose::UserFetch,
    Purpose::Agent,
    Purpose::Training,
    Purpose::Ads,
];

// ---- labels ----------------------------------------------------------------------------------

pub fn purpose_label(p: Purpose) -> &'static str {
    match p {
        Purpose::Search => "Search",
        Purpose::UserFetch => "User-triggered fetchers",
        Purpose::Agent => "Agents",
        Purpose::Training => "Training",
        Purpose::Ads => "Ads",
    }
}

fn purpose_slug(p: Purpose) -> &'static str {
    match p {
        Purpose::Search => "search",
        Purpose::UserFetch => "user_fetch",
        Purpose::Agent => "agent",
        Purpose::Training => "training",
        Purpose::Ads => "ads",
    }
}

fn purpose_from_slug(s: &str) -> Option<Purpose> {
    PURPOSES.into_iter().find(|p| purpose_slug(*p) == s)
}

fn purpose_blurb(p: Purpose) -> &'static str {
    match p {
        Purpose::Search => "Crawlers that index your pages for AI search answers and citations.",
        Purpose::UserFetch => {
            "Fetch one page when someone asks an assistant about it or pastes your link."
        }
        Purpose::Agent => "Browse and act on your site for a person: compare, book, buy.",
        Purpose::Training => {
            "Collect pages to train AI models. Blocking them doesn't affect search."
        }
        Purpose::Ads => "Check landing pages for AI ad products.",
    }
}

pub fn stance_label(s: Stance) -> &'static str {
    match s {
        Stance::Allow => "Allow",
        Stance::Block => "Block",
        Stance::Any => "No preference",
    }
}

fn stance_class(s: Stance) -> &'static str {
    match s {
        Stance::Allow => "aia-allow",
        Stance::Block => "aia-block",
        Stance::Any => "aia-any",
    }
}

fn stance_slug(s: Stance) -> &'static str {
    match s {
        Stance::Allow => "allow",
        Stance::Block => "block",
        Stance::Any => "any",
    }
}

fn stance_from_slug(s: &str) -> Option<Stance> {
    [Stance::Allow, Stance::Block, Stance::Any]
        .into_iter()
        .find(|x| stance_slug(*x) == s)
}

fn severity_view(s: Severity) -> (&'static str, &'static str) {
    match s {
        Severity::Critical => ("critical", "Critical"),
        Severity::Warning => ("warning", "Warning"),
        Severity::Notice => ("notice", "Notice"),
    }
}

fn directive_label(d: DirectiveSlug) -> &'static str {
    match d {
        DirectiveSlug::Noindex => "noindex",
        DirectiveSlug::Nosnippet => "nosnippet",
        DirectiveSlug::MaxSnippet => "max-snippet",
        DirectiveSlug::Noarchive => "noarchive",
        DirectiveSlug::Nocache => "nocache",
        DirectiveSlug::DataNosnippet => "data-nosnippet",
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!(
        "{} {}",
        fmt::thousands(n as i64),
        if n == 1 { one } else { many }
    )
}

/// `A`, `A and B`, `A, B and C`, and past three `A, B, C and 2 more`.
fn names(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        [a, b, c] => format!("{a}, {b} and {c}"),
        [a, b, c, rest @ ..] => format!("{a}, {b}, {c} and {} more", rest.len()),
    }
}

/// `robots.txt line 5 · Disallow: / · User-agent: OAI-SearchBot`
fn rule_line(rule: &MatchedRule, group: GroupMatch, token: &str) -> String {
    let word = if rule.allow { "Allow" } else { "Disallow" };
    let agent = match (group, registry().robots_fallback(token)) {
        (GroupMatch::Named, _) => format!(" · User-agent: {token}"),
        (GroupMatch::Fallback, Some(fallback)) => {
            format!(" · User-agent: {fallback} (followed by {token})")
        }
        (GroupMatch::Wildcard, _) => " · User-agent: *".to_owned(),
        (GroupMatch::Fallback | GroupMatch::None, _) => String::new(),
    };
    format!(
        "robots.txt line {} · {word}: {}{agent}",
        rule.line, rule.pattern
    )
}

/// The URL's path and query, as the screens print a page of the site.
fn path_of(url: &str) -> String {
    match Url::parse(url) {
        Ok(u) => match u.query() {
            Some(q) => format!("{}?{q}", u.path()),
            None => u.path().to_owned(),
        },
        Err(_) => url.to_owned(),
    }
}

/// The explorer selection for a page of the site.
fn explorer_href(site: &Site, base: &str, url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    let strip = |h: &str| h.trim_start_matches("www.").to_ascii_lowercase();
    let host = parsed.host_str()?;
    (strip(host) == strip(&site.domain)).then(|| {
        format!(
            "{base}/explorer?sel={:016x}",
            codoseo_core::url::url_hash(&parsed)
        )
    })
}

fn host_label(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|u| {
            u.host_str()
                .map(|h| h.trim_start_matches("www.").to_owned())
        })
        .unwrap_or_else(|| url.to_owned())
}

// ---- the overview ----------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "ai_access/index.html")]
pub struct AiAccessPage {
    pub shell: Shell,
    pub base: String,
    /// Set until the site has a report.
    pub empty: Option<EmptyState>,
    pub view: Option<Overview>,
}

pub struct EmptyState {
    pub text: &'static str,
    /// Offer Run crawl (no crawl is queued or running).
    pub can_run: bool,
}

pub struct Overview {
    /// `12m ago`
    pub checked: String,
    /// `Oct 9, 14:05 UTC`
    pub checked_full: String,
    /// `crawl #12`
    pub crawl_label: String,
    /// Set when the report comes from a crawl that failed (only robots.txt was read).
    pub failed: Option<String>,
    pub tiles: Vec<Tile>,
    pub incidents: Vec<IncidentView>,
    /// The empty "Open issues" state: its heading, its text and the mascot's mood.
    pub clear: (&'static str, &'static str, &'static str),
    /// Shown instead of the bots table when robots.txt gave no verdicts.
    pub robots_callout: Option<Callout>,
    /// `robots.txt · HTTP 200`
    pub robots_aside: String,
    pub bot_groups: Vec<BotGroup>,
    /// Bots whose status conflicts with the intent ("Needs attention").
    pub bot_attention: usize,
    pub bot_total: usize,
    pub engines: Vec<EngineRow>,
    pub declared: Vec<DeclaredItem>,
    pub important: Vec<ImportantRow>,
    pub resolved: Vec<ResolvedRow>,
}

pub struct Tile {
    pub label: &'static str,
    pub value: String,
    /// `/10`, or empty.
    pub of: String,
    pub sub: String,
    /// Text colour class for the value, or empty.
    pub class: &'static str,
    pub sub_class: &'static str,
}

pub struct Callout {
    pub title: String,
    pub text: &'static str,
}

/// A run of an incident's summary: plain text, or a `code` span (backticks in the finding).
pub struct Seg {
    pub text: String,
    pub code: bool,
}

pub struct SourceLink {
    pub href: String,
    pub label: String,
}

pub struct BotChip {
    pub token: String,
    pub operator: String,
    pub control: bool,
}

/// Bots of one finding that share what happens to them and the rule that does it, so eight
/// training bots allowed in the same way are one line, not eight.
pub struct BotEv {
    pub bots: Vec<BotChip>,
    pub detail: String,
    /// `robots.txt line 5 · Disallow: / · User-agent: OAI-SearchBot`
    pub rule: Option<String>,
}

pub struct EngineEv {
    pub name: String,
    pub effect: &'static str,
    pub effect_class: &'static str,
    /// `on 12 of 20 important pages`
    pub detail: String,
    /// `40 crawled pages carry it`, or empty.
    pub site: String,
}

pub struct UrlLink {
    pub path: String,
    pub href: Option<String>,
}

pub struct IncidentView {
    pub id: i64,
    pub severity: &'static str,
    pub severity_label: &'static str,
    pub title: String,
    pub summary: Vec<Seg>,
    pub opened: String,
    pub opened_full: String,
    pub last_seen: String,
    pub last_seen_full: String,
    /// `Found on first check` or `After an intent change`, for incidents opened without an alert.
    pub quiet: Option<(&'static str, &'static str)>,
    pub sources: Vec<SourceLink>,
    /// `robots.txt answered HTTP 503`
    pub status_line: Option<String>,
    pub bots: Vec<BotEv>,
    pub engines: Vec<EngineEv>,
    pub urls: Vec<UrlLink>,
    pub urls_more: u32,
    /// robots.txt lines that would apply the owner's block.
    pub fix: Option<String>,
    /// The confirmation for "Mark intended"; `None` when the finding can't be a choice.
    pub intend: Option<String>,
}

pub struct BotGroup {
    pub label: &'static str,
    pub stance: &'static str,
    pub stance_class: &'static str,
    pub count: String,
    /// `2 conflicts`, or empty.
    pub conflicts: String,
    pub rows: Vec<BotRow>,
}

pub struct BotRow {
    pub token: String,
    pub operator: String,
    pub product: String,
    /// A control token: never crawls, only says what may be done with the content.
    pub control: bool,
    pub notes: String,
    pub stance: &'static str,
    pub stance_class: &'static str,
    pub overridden: bool,
    pub home: String,
    pub home_class: &'static str,
    pub home_tip: String,
    pub blocked: String,
    pub blocked_class: &'static str,
    pub honours: &'static str,
    pub honours_class: &'static str,
    pub status: &'static str,
    pub status_class: &'static str,
    pub status_tip: &'static str,
    /// The status conflicts with the intent: the row stays under "Needs attention".
    pub attention: bool,
    /// The last such row of its group (it draws no bottom border while the others are hidden).
    pub last_attention: bool,
}

pub struct CauseChip {
    pub label: String,
    pub tip: String,
}

pub struct EngineRow {
    pub name: &'static str,
    pub crawler: &'static str,
    /// `intent: Allow`
    pub stance: &'static str,
    pub page_controls: bool,
    pub reach: String,
    pub reach_class: &'static str,
    pub eligible: String,
    pub eligible_class: &'static str,
    pub limited: String,
    pub limited_class: &'static str,
    pub excluded: String,
    pub excluded_class: &'static str,
    pub causes: Vec<CauseChip>,
    pub site: Vec<String>,
}

pub struct PairChip {
    pub text: String,
    pub class: &'static str,
    pub tip: &'static str,
}

pub struct DeclaredItem {
    pub name: &'static str,
    /// `robots.txt line 4 · all bots`
    pub place: String,
    pub pairs: Vec<PairChip>,
    pub note: Option<String>,
}

pub struct ImportantRow {
    pub path: String,
    pub href: Option<String>,
    pub reason: &'static str,
    pub reason_icon: &'static str,
    pub status: String,
    pub status_class: &'static str,
}

pub struct ResolvedRow {
    pub title: String,
    pub when: String,
    pub when_full: String,
    pub resolution: &'static str,
    pub resolution_class: &'static str,
    pub resolution_tip: &'static str,
}

async fn page(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
) -> Result<Response, AppError> {
    let site = load_site(&state, &user, site_id(&site)?).await?;
    Ok(render_page(&state, &user, &site).await?.into_response())
}

async fn render_page(
    state: &AppState,
    user: &CurrentUser,
    site: &Site,
) -> Result<Html<String>, AppError> {
    let shell = Shell::load(state, user, Some(site), Screen::AiAccess).await?;
    let base = format!("/s/{}", site.id);
    let pool = &state.pool;
    let Some(latest) = geo::latest_report(pool, site.id).await? else {
        let can_run = crawls::active(pool, site.id).await?.is_none();
        return html(&AiAccessPage {
            shell,
            base,
            empty: Some(EmptyState {
                text: if can_run {
                    "Each crawl checks robots.txt for every known AI bot and the page markup each AI engine honours. Run a crawl to see yours."
                } else {
                    "A crawl is on its way. AI access shows up here when it finishes."
                },
                can_run,
            }),
            view: None,
        });
    };
    let intent = geo::get_intent(pool, site.id).await?;
    let open = geo::open_incidents(pool, site.id).await?;
    let resolved = geo::recent_resolved(pool, site.id, RESOLVED_SHOWN).await?;
    let crawl = crawls::get(pool, site.id, latest.crawl_id).await?;

    // When each stored report was written, to tell a first check from an intent change.
    let reported = geo::report_times(pool, site.id).await?;

    let report = &latest.report;
    let incidents: Vec<IncidentView> = open
        .iter()
        .map(|i| {
            let report_at = i.opened_crawl_id.and_then(|id| reported.get(&id).copied());
            incident_view(site, &base, report, i, report_at)
        })
        .collect();
    let critical = open
        .iter()
        .filter(|i| i.severity == Severity::Critical)
        .count();

    let bot_groups = bot_groups(report, &intent);
    let view = Overview {
        checked: fmt::ago(latest.created_at),
        checked_full: fmt::datetime(latest.created_at),
        crawl_label: crawl
            .as_ref()
            .map(|c| format!("crawl #{}", c.number))
            .unwrap_or_else(|| "latest crawl".to_owned()),
        failed: (latest.crawl_status != "done").then(|| {
            "The latest crawl couldn't read any pages, so only robots.txt was checked. The rest is from what that crawl could see.".to_owned()
        }),
        tiles: tiles(report, &intent, open.len(), critical),
        incidents,
        clear: if report.robots.availability == RobotsAvailability::Unknown
            && report.html_important() == 0
        {
            (
                "Nothing to check yet",
                "The latest crawl could read neither robots.txt nor an HTML page. Check that the site is up, then run a crawl.",
                "idle",
            )
        } else {
            (
                "No AI access issues",
                "Watching robots.txt for every known AI bot, and the page markup each AI engine honours, on every crawl.",
                "ok",
            )
        },
        robots_callout: robots_callout(report),
        robots_aside: robots_aside(report),
        bot_attention: bot_groups
            .iter()
            .flat_map(|g| &g.rows)
            .filter(|r| r.attention)
            .count(),
        bot_total: registry().bots.len(),
        bot_groups,
        engines: engine_rows(report, &intent),
        declared: declared_items(&report.declared),
        important: report
            .important
            .iter()
            .map(|u| important_row(site, &base, u))
            .collect(),
        resolved: resolved.iter().map(resolved_row).collect(),
    };
    html(&AiAccessPage {
        shell,
        base,
        empty: None,
        view: Some(view),
    })
}

fn tiles(report: &AccessReport, intent: &Intent, open: usize, critical: usize) -> Vec<Tile> {
    let of = |bots: &[&Bot]| format!("/{}", bots.len());
    let in_purpose = |p: Purpose| -> Vec<&'static Bot> {
        registry().bots.iter().filter(|b| b.purpose == p).collect()
    };
    let mut out = vec![Tile {
        label: "Open issues",
        value: fmt::thousands(open as i64),
        of: String::new(),
        sub: match (open, critical) {
            (0, _) => "nothing to fix".to_owned(),
            (_, 0) => "none critical".to_owned(),
            (_, c) => format!("{c} critical"),
        },
        class: if critical > 0 { "c-err" } else { "" },
        sub_class: if critical > 0 { "c-err" } else { "" },
    }];

    let search = in_purpose(Purpose::Search);
    let training = in_purpose(Purpose::Training);
    if report.has_verdicts() {
        let blocked: Vec<String> = search
            .iter()
            .filter(|b| report.bot(&b.token).is_some_and(|a| !a.blocked.is_empty()))
            .map(|b| b.token.clone())
            .collect();
        let wanted = intent.purpose_stance(Purpose::Search) == Stance::Allow;
        out.push(Tile {
            label: "AI search bots allowed",
            value: fmt::thousands((search.len() - blocked.len()) as i64),
            of: of(&search),
            sub: if blocked.is_empty() {
                "on every important page".to_owned()
            } else {
                format!("blocked: {}", names(&blocked))
            },
            class: if !blocked.is_empty() && wanted {
                "c-warn"
            } else {
                ""
            },
            sub_class: "",
        });
        let stopped = training
            .iter()
            .filter(|b| report.bot(&b.token).and_then(|a| a.home_allowed) == Some(false))
            .count();
        let stance = intent.purpose_stance(Purpose::Training);
        out.push(Tile {
            label: "Training bots blocked",
            value: fmt::thousands(stopped as i64),
            of: of(&training),
            sub: format!("your intent: {}", stance_label(stance)),
            class: if stance == Stance::Block && stopped < training.len() {
                "c-warn"
            } else {
                ""
            },
            sub_class: "",
        });
    } else {
        let why = match report.robots.availability {
            RobotsAvailability::Unknown => "robots.txt not checked",
            _ => "robots.txt not readable",
        };
        for (label, bots) in [
            ("AI search bots allowed", &search),
            ("Training bots blocked", &training),
        ] {
            out.push(Tile {
                label,
                value: "—".to_owned(),
                of: of(bots),
                sub: why.to_owned(),
                class: "c-ghost",
                sub_class: "",
            });
        }
    }

    let google = report.engine(codoseo_geo::eligibility::EngineId::Google);
    let html_pages = google.map_or(0, |e| usize::from(e.html_pages));
    let affected = google.map_or(0, |e| e.affected());
    let mut excluded = 0;
    let mut limited = 0;
    for g in google.map(|e| e.groups.as_slice()).unwrap_or_default() {
        if g.robots_blocked || g.effect == Effect::Excluded {
            excluded += g.urls.len();
        } else if g.effect == Effect::Limited {
            limited += g.urls.len();
        }
    }
    out.push(Tile {
        label: "Quotable in Google AI",
        value: if html_pages == 0 {
            "—".to_owned()
        } else {
            fmt::thousands(html_pages.saturating_sub(affected) as i64)
        },
        of: format!("/{html_pages}"),
        sub: if html_pages == 0 {
            "no HTML pages checked".to_owned()
        } else if affected == 0 {
            "every important page".to_owned()
        } else {
            let mut parts = Vec::new();
            if excluded > 0 {
                parts.push(format!("{excluded} excluded"));
            }
            if limited > 0 {
                parts.push(format!("{limited} limited"));
            }
            parts.join(" · ")
        },
        class: if excluded > 0 {
            "c-err"
        } else if limited > 0 {
            "c-warn"
        } else if html_pages == 0 {
            "c-ghost"
        } else {
            ""
        },
        sub_class: "",
    });
    out
}

/// What robots.txt answered, beside the bots table's filter (which carries the bot count).
fn robots_aside(report: &AccessReport) -> String {
    match (report.robots.availability, report.robots.status) {
        (RobotsAvailability::Ok, Some(s)) => format!("robots.txt · HTTP {s}"),
        (RobotsAvailability::Missing, Some(s)) => {
            format!("no robots.txt (HTTP {s}): every bot may crawl")
        }
        _ => plural(registry().bots.len(), "bot", "bots"),
    }
}

fn robots_callout(report: &AccessReport) -> Option<Callout> {
    match (report.robots.availability, report.robots.status) {
        (RobotsAvailability::Unavailable, status) => Some(Callout {
            title: match status {
                Some(s) => format!("robots.txt answered HTTP {s}"),
                None => "robots.txt couldn't be read".to_owned(),
            },
            text: "While robots.txt fails, bots that follow it treat the whole site as off limits, so there is nothing to check per bot. Make /robots.txt answer 200, or 404 if you have none.",
        }),
        (RobotsAvailability::Unknown, _) => Some(Callout {
            title: "robots.txt wasn't checked".to_owned(),
            text: "The latest crawl didn't fetch robots.txt, so there are no per-bot verdicts yet. They appear after the next crawl.",
        }),
        _ => None,
    }
}

fn summary_segments(s: &str) -> Vec<Seg> {
    s.split('`')
        .enumerate()
        .filter(|(_, t)| !t.is_empty())
        .map(|(i, t)| Seg {
            text: t.to_owned(),
            code: i % 2 == 1,
        })
        .collect()
}

fn sources(kind: FindingKind, ev: &Evidence) -> Vec<SourceLink> {
    let mut urls: Vec<String> = Vec::new();
    let mut push = |u: &str| {
        if !urls.iter().any(|x| x == u) {
            urls.push(u.to_owned());
        }
    };
    if kind == FindingKind::RobotsUnavailable {
        push(ROBOTS_DOC);
    }
    for b in &ev.bots {
        if let Some(bot) = registry().bot(&b.token) {
            push(&bot.source_url);
        }
    }
    for e in &ev.engines {
        if let Some(engine) = engines().iter().find(|x| x.id == e.engine) {
            push(engine.source_url);
        }
    }
    urls.into_iter()
        .map(|href| SourceLink {
            label: host_label(&href),
            href,
        })
        .collect()
}

fn bot_detail(kind: FindingKind, b: &BotEvidence) -> String {
    let total = b.urls_total as usize;
    let n = b.urls_blocked as usize;
    let mut out = match kind {
        FindingKind::BotsNotBlocked => {
            if registry().bot(&b.token).is_some_and(|x| !x.crawls) {
                "Not disallowed, so your content may be used".to_owned()
            } else {
                "Allowed on the home page".to_owned()
            }
        }
        _ if total <= 1 && b.home_blocked => "Blocked on the home page".to_owned(),
        _ if n == total && total > 0 => format!("Blocked on all {total} important pages"),
        _ if b.home_blocked => {
            format!("Blocked on {n} of {total} important pages, including the home page")
        }
        _ => format!("Blocked on {n} of {total} important pages; the home page is allowed"),
    };
    if b.honours != Honours::Yes {
        out.push_str(" · may ignore robots.txt");
    }
    out
}

/// The owner's intent change that turns this finding into a choice: the bots it names and the
/// stance they get.
fn intended_change(kind: FindingKind, ev: &Evidence) -> Option<(Vec<String>, Stance)> {
    let tokens = |v: &[BotEvidence]| v.iter().map(|b| b.token.clone()).collect::<Vec<_>>();
    match kind {
        FindingKind::RobotsUnavailable => None,
        FindingKind::BotsBlocked if !ev.bots.is_empty() => Some((tokens(&ev.bots), Stance::Block)),
        FindingKind::BotsNotBlocked if !ev.bots.is_empty() => Some((tokens(&ev.bots), Stance::Any)),
        FindingKind::AnswersRestricted if !ev.engines.is_empty() => {
            let mut crawlers: Vec<String> = Vec::new();
            for e in &ev.engines {
                if let Some(engine) = engines().iter().find(|x| x.id == e.engine)
                    && !crawlers.iter().any(|c| c == engine.crawler)
                {
                    crawlers.push(engine.crawler.to_owned());
                }
            }
            Some((crawlers, Stance::Any))
        }
        _ => None,
    }
}

fn intend_confirm(tokens: &[String], stance: Stance) -> String {
    let who = names(tokens);
    let them = if tokens.len() == 1 { "it" } else { "they" };
    match stance {
        Stance::Block => format!(
            "Set {who} to Block in your intent? This issue closes, and you'll hear if {them} can crawl again."
        ),
        _ => format!(
            "Set {who} to No preference in your intent? This issue closes, and {them} won't be reported either way."
        ),
    }
}

fn incident_view(
    site: &Site,
    base: &str,
    report: &AccessReport,
    i: &Incident,
    report_at: Option<OffsetDateTime>,
) -> IncidentView {
    let ev: Evidence = serde_json::from_value(i.evidence.clone()).unwrap_or_default();
    let (severity, severity_label) = severity_view(i.severity);
    let html_total = report.html_important();
    let mut bots: Vec<BotEv> = Vec::new();
    for b in &ev.bots {
        let detail = bot_detail(i.kind, b);
        let rule = b
            .rule
            .as_ref()
            .filter(|_| i.kind == FindingKind::BotsBlocked)
            .map(|r| rule_line(r, b.group, &b.token));
        let chip = BotChip {
            token: b.token.clone(),
            operator: b.operator.clone(),
            control: registry().bot(&b.token).is_some_and(|x| !x.crawls),
        };
        match bots
            .iter_mut()
            .find(|g| g.detail == detail && g.rule == rule)
        {
            Some(g) => g.bots.push(chip),
            None => bots.push(BotEv {
                bots: vec![chip],
                detail,
                rule,
            }),
        }
    }
    let engines_ev = ev
        .engines
        .iter()
        .map(|e| {
            let (effect, effect_class, n) = match e.effect {
                Effect::Excluded => ("Excluded", "t-err", e.pages_excluded),
                _ => ("Limited", "t-warn", e.pages_limited),
            };
            let mut detail = if html_total > 0 {
                format!("on {n} of {html_total} important pages")
            } else {
                format!(
                    "on {}",
                    plural(n as usize, "important page", "important pages")
                )
            };
            if e.effect == Effect::Excluded && e.pages_limited > 0 {
                detail.push_str(&format!(", limited on {}", e.pages_limited));
            }
            EngineEv {
                name: e.name.clone(),
                effect,
                effect_class,
                detail,
                site: match e.site_pages {
                    0 => String::new(),
                    1 => "1 crawled page carries it".to_owned(),
                    n => format!("{} crawled pages carry it", fmt::thousands(n)),
                },
            }
        })
        .collect();
    let urls: Vec<UrlLink> = ev
        .urls
        .sample
        .iter()
        .take(URLS_SHOWN)
        .map(|u| UrlLink {
            path: path_of(u),
            href: explorer_href(site, base, u),
        })
        .collect();
    let urls_more = ev.urls.total.saturating_sub(urls.len() as u32);
    let fix = (i.kind == FindingKind::BotsNotBlocked && !ev.bots.is_empty()).then(|| {
        let mut lines: Vec<String> = ev
            .bots
            .iter()
            .map(|b| format!("User-agent: {}", b.token))
            .collect();
        lines.push("Disallow: /".to_owned());
        lines.join("\n")
    });
    let quiet = i.quiet.then(|| match report_at {
        // The same transaction: Postgres' now() is the same instant for both rows.
        Some(at) if at == i.opened_at => (
            "Found on first check",
            "Part of the site's first AI access check, so no alert was sent",
        ),
        Some(_) => (
            "After an intent change",
            "Opened when your intent changed, so no alert was sent",
        ),
        None => (
            "No alert sent",
            "Opened by the first check or an intent change",
        ),
    });
    IncidentView {
        id: i.id,
        severity,
        severity_label,
        title: i.title.clone(),
        summary: summary_segments(&i.summary),
        opened: fmt::ago(i.opened_at),
        opened_full: fmt::datetime(i.opened_at),
        last_seen: fmt::ago(i.last_seen_at),
        last_seen_full: fmt::datetime(i.last_seen_at),
        quiet,
        sources: sources(i.kind, &ev),
        status_line: ev
            .robots_status
            .map(|s| format!("GET /robots.txt → HTTP {s}")),
        bots,
        engines: engines_ev,
        urls,
        urls_more,
        fix,
        intend: intended_change(i.kind, &ev).map(|(t, s)| intend_confirm(&t, s)),
    }
}

fn rule_tip(rule: Option<&MatchedRule>, group: GroupMatch, token: &str) -> String {
    match (rule, group) {
        (Some(r), g) => rule_line(r, g, token),
        (None, GroupMatch::None) => "No group in robots.txt applies to this bot".to_owned(),
        (None, _) => "No rule in its group matches the home page".to_owned(),
    }
}

fn bot_groups(report: &AccessReport, intent: &Intent) -> Vec<BotGroup> {
    PURPOSES
        .iter()
        .filter_map(|&p| {
            let bots: Vec<&Bot> = registry().bots.iter().filter(|b| b.purpose == p).collect();
            if bots.is_empty() {
                return None;
            }
            let stance = intent.purpose_stance(p);
            let mut rows: Vec<BotRow> = bots.iter().map(|b| bot_row(report, intent, b)).collect();
            if let Some(last) = rows.iter_mut().rev().find(|r| r.attention) {
                last.last_attention = true;
            }
            let conflicts = rows.iter().filter(|r| r.attention).count();
            Some(BotGroup {
                label: purpose_label(p),
                stance: stance_label(stance),
                stance_class: stance_class(stance),
                count: plural(bots.len(), "bot", "bots"),
                conflicts: if conflicts == 0 {
                    String::new()
                } else {
                    plural(conflicts, "conflict", "conflicts")
                },
                rows,
            })
        })
        .collect()
}

fn bot_row(report: &AccessReport, intent: &Intent, bot: &Bot) -> BotRow {
    let access = report.bot(&bot.token);
    let stance = intent.effective(bot);
    let total = report.important.len();
    let home_allowed = access.and_then(|a| a.home_allowed);
    let honoured = bot.honours_robots == Honours::Yes;
    // Coloured by the intent: green where robots.txt does what you want, red or amber where it
    // doesn't, plain where you have no preference.
    let (home, home_class, home_tip) = match (access, home_allowed) {
        (Some(a), Some(true)) => (
            "Allowed".to_owned(),
            match stance {
                Stance::Allow => "c-ok",
                Stance::Block if honoured => "c-warn",
                _ => "",
            },
            rule_tip(a.home_rule.as_ref(), a.group, &bot.token),
        ),
        (Some(a), Some(false)) => (
            match &a.home_rule {
                Some(r) => format!("Blocked · line {}", r.line),
                None => "Blocked".to_owned(),
            },
            match stance {
                Stance::Allow => "c-err",
                Stance::Block => "c-ok",
                Stance::Any => "",
            },
            rule_tip(a.home_rule.as_ref(), a.group, &bot.token),
        ),
        _ => ("—".to_owned(), "c-ghost", String::new()),
    };
    let blocked_n = access.map_or(0, |a| a.blocked.len());
    let (honours, honours_class) = match bot.honours_robots {
        Honours::Yes => ("Yes", ""),
        Honours::Partial | Honours::No => ("May ignore", "c-muted"),
        Honours::Unknown => ("Not stated", "c-muted"),
    };
    let (status, status_class, status_tip) = match stance {
        Stance::Allow if blocked_n > 0 => (
            "Conflicts",
            if !honoured {
                "t-muted"
            } else if home_allowed == Some(false) {
                "t-err"
            } else {
                "t-warn"
            },
            "You allow it, but robots.txt blocks it",
        ),
        Stance::Allow => ("OK", "t-ok", "Allowed, as you want"),
        Stance::Block if home_allowed == Some(false) => ("OK", "t-ok", "Blocked, as you want"),
        Stance::Block if honoured && home_allowed == Some(true) => (
            "Conflicts",
            "t-warn",
            "You block it, but robots.txt lets it in",
        ),
        Stance::Block => (
            "—",
            "",
            "Not blocked, but its operator doesn't promise to follow robots.txt",
        ),
        Stance::Any => ("—", "", "No preference: never reported"),
    };
    BotRow {
        token: bot.token.clone(),
        operator: bot.operator.clone(),
        product: bot.product.clone(),
        control: !bot.crawls,
        notes: bot.notes.clone(),
        stance: stance_label(stance),
        stance_class: stance_class(stance),
        overridden: intent.bot_override(&bot.token).is_some(),
        home,
        home_class,
        home_tip,
        blocked: if total == 0 {
            "—".to_owned()
        } else {
            format!("{blocked_n}/{total}")
        },
        blocked_class: match (blocked_n, stance) {
            (0, _) => "c-muted",
            (_, Stance::Allow) => "c-err",
            _ => "",
        },
        honours,
        honours_class,
        attention: status == "Conflicts",
        last_attention: false,
        status,
        status_class,
        status_tip,
    }
}

fn cause_tip(c: &Cause) -> String {
    let place = match &c.source {
        CauseSource::Meta { name } => format!("<meta name=\"{name}\">"),
        CauseSource::Header { scope } if scope == "all" => "X-Robots-Tag header".to_owned(),
        CauseSource::Header { scope } => format!("X-Robots-Tag header ({scope}:)"),
        CauseSource::Attribute => "data-nosnippet attributes".to_owned(),
    };
    format!("{} in {place}", c.detail)
}

fn engine_rows(report: &AccessReport, intent: &Intent) -> Vec<EngineRow> {
    let total = report.important.len();
    engines()
        .iter()
        .map(|e| {
            let access = report.engine(e.id);
            let stance = registry()
                .bot(e.crawler)
                .map_or(Stance::Any, |b| intent.effective(b));
            let blocked = report.bot(e.crawler).map_or(0, |a| a.blocked.len());
            let (reach, reach_class) = if !report.has_verdicts() {
                ("Unknown".to_owned(), "c-muted")
            } else if blocked == 0 {
                ("Allowed".to_owned(), "c-ok")
            } else if blocked == total {
                ("Blocked".to_owned(), "c-err")
            } else {
                (format!("Blocked on {blocked} of {total}"), "c-err")
            };

            let html_pages = access.map_or(0, |a| usize::from(a.html_pages));
            let mut limited = 0usize;
            let mut excluded = 0usize;
            // directive label -> (pages, where it was found)
            let mut causes: BTreeMap<&'static str, (usize, BTreeSet<String>)> = BTreeMap::new();
            for g in access.map(|a| a.groups.as_slice()).unwrap_or_default() {
                let n = g.urls.len();
                if g.robots_blocked {
                    excluded += n;
                    let entry = causes.entry("robots.txt").or_default();
                    entry.0 += n;
                    entry.1.insert(format!("{} blocks these pages", e.crawler));
                    continue;
                }
                match g.effect {
                    Effect::Excluded => excluded += n,
                    Effect::Limited => limited += n,
                    Effect::Eligible => {}
                }
                let mut seen: BTreeSet<&'static str> = BTreeSet::new();
                for c in &g.causes {
                    let label = directive_label(c.directive);
                    let entry = causes.entry(label).or_default();
                    if seen.insert(label) {
                        entry.0 += n;
                    }
                    entry.1.insert(cause_tip(c));
                }
            }
            let mut causes: Vec<(&'static str, (usize, BTreeSet<String>))> =
                causes.into_iter().collect();
            causes.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(b.0)));
            let affected = access.map_or(0, |a| a.affected());
            let count = |n: usize, class: &'static str| {
                (
                    fmt::thousands(n as i64),
                    if n > 0 { class } else { "c-ghost" },
                )
            };
            let (eligible, eligible_class) = count(html_pages.saturating_sub(affected), "");
            let (limited, limited_class) = count(limited, "c-warn");
            let (excluded, excluded_class) = count(excluded, "c-err");
            let site_pages = access.map_or(0, |a| a.site.pages);
            EngineRow {
                name: e.name,
                crawler: e.crawler,
                stance: stance_label(stance),
                page_controls: e.page_controls,
                reach,
                reach_class,
                eligible,
                eligible_class,
                limited,
                limited_class,
                excluded,
                excluded_class,
                causes: causes
                    .into_iter()
                    .map(|(label, (n, tips))| CauseChip {
                        label: format!("{label} ×{n}"),
                        tip: tips.into_iter().collect::<Vec<_>>().join("; "),
                    })
                    .collect(),
                site: access
                    .map(|a| {
                        a.site
                            .by_cause
                            .iter()
                            .take(3)
                            .map(|(slug, n)| {
                                format!("{} on {n} of {site_pages}", directive_label(*slug))
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            }
        })
        .collect()
}

fn pair_chips(pairs: &[Pair]) -> Vec<PairChip> {
    pairs
        .iter()
        .map(|p| {
            let v = p.value.to_ascii_lowercase();
            let class = if !p.known {
                "aia-kv-unknown"
            } else if matches!(v.as_str(), "yes" | "y") {
                "aia-kv-yes"
            } else if matches!(v.as_str(), "no" | "n") {
                "aia-kv-no"
            } else {
                ""
            };
            PairChip {
                text: format!("{}={}", p.key, p.value),
                class,
                tip: if p.known {
                    ""
                } else {
                    "Not a key the specification defines"
                },
            }
        })
        .collect()
}

fn agents_label(agents: &[String], more: u32) -> String {
    if agents.is_empty() || agents.iter().any(|a| a == "*") {
        "all bots".to_owned()
    } else if more > 0 {
        format!("User-agent: {} and {more} more", agents.join(", "))
    } else {
        format!("User-agent: {}", agents.join(", "))
    }
}

fn single(value: &str) -> Vec<PairChip> {
    vec![PairChip {
        text: value.to_owned(),
        class: "",
        tip: "",
    }]
}

fn declared_items(d: &Declared) -> Vec<DeclaredItem> {
    let mut out = Vec::new();
    for s in &d.content_signals {
        out.push(DeclaredItem {
            name: "Content-Signal",
            place: format!(
                "robots.txt line {} · {}",
                s.line,
                agents_label(&s.agents, s.more_agents)
            ),
            pairs: pair_chips(&s.pairs),
            note: None,
        });
    }
    for u in &d.content_usage {
        out.push(DeclaredItem {
            name: "Content-Usage",
            place: format!(
                "robots.txt line {} · {}{}",
                u.line,
                agents_label(&u.agents, u.more_agents),
                u.path
                    .as_deref()
                    .map(|p| format!(" · {p}"))
                    .unwrap_or_default()
            ),
            pairs: pair_chips(&u.pairs),
            note: None,
        });
    }
    if !d.headers.content_signal.is_empty() {
        out.push(DeclaredItem {
            name: "Content-Signal",
            place: "HTTP header on the home page".to_owned(),
            pairs: pair_chips(&d.headers.content_signal),
            note: None,
        });
    }
    for u in &d.headers.content_usage {
        out.push(DeclaredItem {
            name: "Content-Usage",
            place: format!(
                "HTTP header on the home page{}",
                u.path
                    .as_deref()
                    .map(|p| format!(" · {p}"))
                    .unwrap_or_default()
            ),
            pairs: pair_chips(&u.pairs),
            note: None,
        });
    }
    let tdm = |v: &str| match v.trim() {
        "1" => "1 · rights reserved".to_owned(),
        "0" => "0 · not reserved".to_owned(),
        other => other.to_owned(),
    };
    if let Some(v) = &d.headers.tdm_reservation {
        out.push(DeclaredItem {
            name: "TDM-Reservation",
            place: "HTTP header on the home page".to_owned(),
            pairs: single(&tdm(v)),
            note: None,
        });
    }
    if let Some(v) = &d.headers.tdm_policy {
        out.push(DeclaredItem {
            name: "TDM-Policy",
            place: "HTTP header on the home page".to_owned(),
            pairs: single(v),
            note: None,
        });
    }
    if let Some(v) = &d.tdm_meta.reservation {
        out.push(DeclaredItem {
            name: "tdm-reservation",
            place: "meta tag on the home page".to_owned(),
            pairs: single(&tdm(v)),
            note: None,
        });
    }
    if let Some(v) = &d.tdm_meta.policy {
        out.push(DeclaredItem {
            name: "tdm-policy",
            place: "meta tag on the home page".to_owned(),
            pairs: single(v),
            note: None,
        });
    }
    if let Some(f) = &d.tdmrep {
        if let Some(err) = &f.error {
            out.push(DeclaredItem {
                name: "TDMRep",
                place: format!("/.well-known/tdmrep.json · HTTP {}", f.status),
                pairs: Vec::new(),
                note: Some(format!("Not valid TDMRep JSON: {err}")),
            });
        }
        for e in &f.entries {
            let mut pairs = vec![PairChip {
                text: match e.reservation {
                    Some(r) => format!("tdm-reservation={r}"),
                    None => "tdm-reservation unset".to_owned(),
                },
                class: "",
                tip: "",
            }];
            if let Some(p) = &e.policy {
                pairs.push(PairChip {
                    text: format!("tdm-policy={p}"),
                    class: "",
                    tip: "",
                });
            }
            out.push(DeclaredItem {
                name: "TDMRep",
                place: format!("/.well-known/tdmrep.json · {}", e.location),
                pairs,
                note: None,
            });
        }
    }
    out
}

fn important_row(site: &Site, base: &str, u: &codoseo_geo::report::ImportantUrl) -> ImportantRow {
    let (reason, reason_icon) = match u.reason {
        Reason::Home => ("Home", "i-globe"),
        Reason::MostLinked => ("Most linked", "i-redirect"),
        Reason::Starred => ("Starred", "i-star"),
    };
    let (status, status_class) = match u.status {
        Some(s @ 200..=299) => (s.to_string(), "t-ok"),
        Some(s @ 300..=399) => (s.to_string(), "t-muted"),
        Some(s @ 400..=499) => (s.to_string(), "t-warn"),
        Some(s) => (s.to_string(), "t-err"),
        None => ("not crawled".to_owned(), "t-muted"),
    };
    ImportantRow {
        path: path_of(&u.url),
        href: u
            .status
            .is_some()
            .then(|| explorer_href(site, base, &u.url))
            .flatten(),
        reason,
        reason_icon,
        status,
        status_class,
    }
}

fn resolved_row(i: &Incident) -> ResolvedRow {
    let at = i.resolved_at.unwrap_or(i.last_seen_at);
    let (resolution, resolution_class, resolution_tip) = match i.resolution.as_deref() {
        Some("intent") => (
            "intended",
            "t-muted",
            "Your intent changed, so this is a choice now",
        ),
        _ => ("fixed", "t-ok", "A later crawl no longer found it"),
    };
    ResolvedRow {
        title: i.title.clone(),
        when: fmt::ago(at),
        when_full: fmt::datetime(at),
        resolution,
        resolution_class,
        resolution_tip,
    }
}

// ---- the intent form -------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "ai_access/intent.html")]
pub struct IntentPage {
    pub shell: Shell,
    pub base: String,
    pub cards: Vec<PurposeCard>,
    /// The saved intent differs from the defaults.
    pub custom: bool,
}

pub struct Choice {
    pub id: String,
    pub value: &'static str,
    pub label: &'static str,
    pub checked: bool,
}

pub struct SelectOpt {
    pub value: &'static str,
    pub label: &'static str,
    pub selected: bool,
}

pub struct BotPick {
    pub token: String,
    pub operator: String,
    pub product: String,
    /// The form field: `b.{token}`.
    pub name: String,
    pub id: String,
    pub control: bool,
    /// The bot has an override (anything but "Inherit").
    pub overridden: bool,
    pub options: Vec<SelectOpt>,
}

pub struct PurposeCard {
    pub slug: &'static str,
    pub label: &'static str,
    pub blurb: &'static str,
    /// `Default: Allow`
    pub default_label: &'static str,
    pub choices: Vec<Choice>,
    pub bots: Vec<BotPick>,
    /// Bots of this purpose with an override.
    pub overrides: usize,
}

fn intent_cards(intent: &Intent) -> Vec<PurposeCard> {
    PURPOSES
        .iter()
        .map(|&p| {
            let slug = purpose_slug(p);
            let current = intent.purpose_stance(p);
            let bots: Vec<&Bot> = registry().bots.iter().filter(|b| b.purpose == p).collect();
            let picks: Vec<BotPick> = bots
                .iter()
                .map(|b| {
                    let over = intent.bot_override(&b.token);
                    let opt = |value, label, selected| SelectOpt {
                        value,
                        label,
                        selected,
                    };
                    BotPick {
                        token: b.token.clone(),
                        operator: b.operator.clone(),
                        product: b.product.clone(),
                        name: format!("b.{}", b.token),
                        id: format!("bot-{}", b.token),
                        control: !b.crawls,
                        overridden: over.is_some(),
                        options: vec![
                            opt("inherit", "Inherit", over.is_none()),
                            opt("allow", "Allow", over == Some(Stance::Allow)),
                            opt("block", "Block", over == Some(Stance::Block)),
                            opt("any", "No preference", over == Some(Stance::Any)),
                        ],
                    }
                })
                .collect();
            PurposeCard {
                slug,
                label: purpose_label(p),
                blurb: purpose_blurb(p),
                default_label: match Intent::default_stance(p) {
                    Stance::Allow => "Default: Allow",
                    Stance::Block => "Default: Block",
                    Stance::Any => "Default: No preference",
                },
                choices: [Stance::Allow, Stance::Block, Stance::Any]
                    .into_iter()
                    .map(|s| Choice {
                        id: format!("p-{slug}-{}", stance_slug(s)),
                        value: stance_slug(s),
                        label: stance_label(s),
                        checked: s == current,
                    })
                    .collect(),
                overrides: bots
                    .iter()
                    .filter(|b| intent.bot_override(&b.token).is_some())
                    .count(),
                bots: picks,
            }
        })
        .collect()
}

async fn intent_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
) -> Result<Response, AppError> {
    let site = load_site(&state, &user, site_id(&site)?).await?;
    let shell = Shell::load(&state, &user, Some(&site), Screen::AiAccess).await?;
    let intent = geo::get_intent(&state.pool, site.id).await?;
    Ok(html(&IntentPage {
        shell,
        base: format!("/s/{}", site.id),
        cards: intent_cards(&intent),
        custom: intent != Intent::default(),
    })?
    .into_response())
}

/// The intent the form describes. Purposes left at their default and bots left on "Inherit"
/// are not stored, so a later change of default reaches them. Overrides for tokens the registry
/// no longer lists (the form can't show them) are kept as they were.
pub fn intent_from_form(current: &Intent, fields: &[(String, String)]) -> Result<Intent, String> {
    // The form sets stances only; accepted page directives are kept as they are.
    let mut intent = Intent {
        accepted_directives: current.accepted_directives.clone(),
        ..Intent::default()
    };
    for (token, stance) in &current.bots {
        if registry().bot(token).is_none() {
            intent.bots.insert(token.clone(), *stance);
        }
    }
    for (key, value) in fields {
        if let Some(slug) = key.strip_prefix("p.") {
            let purpose =
                purpose_from_slug(slug).ok_or_else(|| format!("\"{slug}\" is not a purpose."))?;
            let stance = stance_from_slug(value)
                .ok_or_else(|| format!("\"{value}\" is not a choice for {slug}."))?;
            if stance != Intent::default_stance(purpose) {
                intent.purposes.insert(purpose, stance);
            } else {
                intent.purposes.remove(&purpose);
            }
        } else if let Some(token) = key.strip_prefix("b.") {
            let bot = registry()
                .bot(token)
                .ok_or_else(|| format!("\"{token}\" is not a bot in the registry."))?;
            match value.as_str() {
                "inherit" | "" => {
                    intent.bots.remove(&bot.token);
                }
                other => {
                    let stance = stance_from_slug(other)
                        .ok_or_else(|| format!("\"{other}\" is not a choice for {token}."))?;
                    intent.bots.insert(bot.token.clone(), stance);
                }
            }
        }
    }
    Ok(intent)
}

async fn save_intent(
    State(state): State<AppState>,
    user: CurrentUser,
    hx: Hx,
    Path(site): Path<String>,
    Form(fields): Form<Vec<(String, String)>>,
) -> Result<Response, AppError> {
    let site = load_site(&state, &user, site_id(&site)?).await?;
    let current = geo::get_intent(&state.pool, site.id).await?;
    let intent = intent_from_form(&current, &fields).map_err(AppError::BadRequest)?;
    let outcome = apply_intent(&state, &site, &intent).await?;
    respond(
        &state,
        &user,
        &site,
        hx,
        &format!("Intent saved · {outcome}"),
        "hx-push-url",
    )
    .await
}

async fn reset_intent(
    State(state): State<AppState>,
    user: CurrentUser,
    hx: Hx,
    Path(site): Path<String>,
) -> Result<Response, AppError> {
    let site = load_site(&state, &user, site_id(&site)?).await?;
    let outcome = apply_intent(&state, &site, &Intent::default()).await?;
    respond(
        &state,
        &user,
        &site,
        hx,
        &format!("Intent reset to the defaults · {outcome}"),
        "hx-push-url",
    )
    .await
}

/// "Mark intended": the intent change that makes an open incident a choice, then the quiet
/// re-evaluation that resolves it.
async fn mark_intended(
    State(state): State<AppState>,
    user: CurrentUser,
    hx: Hx,
    Path((site, id)): Path<(String, i64)>,
) -> Result<Response, AppError> {
    let site = load_site(&state, &user, site_id(&site)?).await?;
    let incident = geo::incident(&state.pool, site.id, id)
        .await?
        .ok_or(AppError::NotFound)?;
    if incident.resolved_at.is_some() {
        return Err(AppError::Conflict(
            "That issue is already resolved.".to_owned(),
        ));
    }
    let ev: Evidence = serde_json::from_value(incident.evidence.clone()).unwrap_or_default();
    let (tokens, stance) = intended_change(incident.kind, &ev).ok_or_else(|| {
        AppError::BadRequest("This issue can't be marked as intended: fix robots.txt.".to_owned())
    })?;
    let mut intent = geo::get_intent(&state.pool, site.id).await?;
    for token in &tokens {
        // Replace any override spelled in another case.
        intent.bots.retain(|t, _| !t.eq_ignore_ascii_case(token));
        intent.bots.insert(token.clone(), stance);
    }
    apply_intent(&state, &site, &intent).await?;
    respond(
        &state,
        &user,
        &site,
        hx,
        &format!(
            "Marked as intended · {} set to {}",
            names(&tokens),
            stance_label(stance)
        ),
        "hx-replace-url",
    )
    .await
}

/// Saves the intent and re-evaluates the latest report quietly. Returns what happened to the
/// open issues, as the toast says it.
async fn apply_intent(state: &AppState, site: &Site, intent: &Intent) -> Result<String, AppError> {
    geo::set_intent(&state.pool, site.id, intent)
        .await
        .map_err(|e| match e {
            IntentError::Invalid(m) => AppError::BadRequest(m),
            IntentError::Db(e) => e.into(),
        })?;
    let has_report = geo::has_report(&state.pool, site.id).await?;
    let (opened, resolved) = geo::reevaluate_quietly(&state.pool, site.id).await?;
    Ok(outcome(has_report, opened, resolved))
}

fn outcome(has_report: bool, opened: usize, resolved: usize) -> String {
    let issues = |n: usize| plural(n, "issue", "issues");
    match (has_report, resolved, opened) {
        (false, ..) => "it applies from the next crawl".to_owned(),
        (true, 0, 0) => "no issues opened or resolved".to_owned(),
        (true, r, 0) => format!("{} resolved", issues(r)),
        (true, 0, o) => format!("{} opened", issues(o)),
        (true, r, o) => format!("{} resolved, {o} opened", issues(r)),
    }
}

/// After a change from a form: htmx gets the AI access page with a toast and the address bar
/// set to it (`history` is `hx-push-url` or `hx-replace-url`); a plain form post is redirected.
async fn respond(
    state: &AppState,
    user: &CurrentUser,
    site: &Site,
    hx: Hx,
    message: &str,
    history: &'static str,
) -> Result<Response, AppError> {
    let to = format!("/s/{}/ai-access", site.id);
    if !hx.request {
        return Ok(Redirect::to(&to).into_response());
    }
    let page = render_page(state, user, site).await?;
    let url = HeaderValue::from_str(&to).map_err(AppError::internal)?;
    Ok((
        [
            toast(ToastKind::Ok, message),
            (HeaderName::from_static(history), url),
        ],
        page,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_form_stores_only_what_differs_from_the_defaults() {
        let intent = intent_from_form(
            &Intent::default(),
            &pairs(&[
                ("p.search", "allow"),
                ("p.training", "block"),
                ("b.GPTBot", "inherit"),
                ("b.oai-searchbot", "any"),
            ]),
        )
        .unwrap();
        assert_eq!(intent.purposes.len(), 1);
        assert_eq!(intent.purposes[&Purpose::Training], Stance::Block);
        // The registry's spelling is stored.
        assert_eq!(intent.bots.get("OAI-SearchBot"), Some(&Stance::Any));
        assert!(!intent.bots.contains_key("GPTBot"));
    }

    #[test]
    fn unknown_tokens_already_stored_survive_and_bad_values_are_refused() {
        let mut current = Intent::default();
        current.bots.insert("FutureBot".to_owned(), Stance::Block);
        current.bots.insert("GPTBot".to_owned(), Stance::Block);
        let intent = intent_from_form(&current, &pairs(&[("b.GPTBot", "inherit")])).unwrap();
        assert_eq!(intent.bots.get("FutureBot"), Some(&Stance::Block));
        assert!(!intent.bots.contains_key("GPTBot"));

        assert!(intent_from_form(&current, &pairs(&[("p.search", "maybe")])).is_err());
        assert!(intent_from_form(&current, &pairs(&[("p.nothing", "allow")])).is_err());
        assert!(intent_from_form(&current, &pairs(&[("b.NoSuchBot", "allow")])).is_err());
    }

    #[test]
    fn the_form_keeps_accepted_directives() {
        let current = Intent {
            accepted_directives: [DirectiveSlug::Nosnippet].into(),
            ..Intent::default()
        };
        let intent = intent_from_form(&current, &pairs(&[("p.training", "block")])).unwrap();
        assert_eq!(intent.accepted_directives, current.accepted_directives);
    }

    #[test]
    fn summaries_split_on_backticks() {
        let segs = summary_segments("Add `User-agent: GPTBot` and `Disallow: /` to robots.txt.");
        let shape: Vec<(bool, &str)> = segs.iter().map(|s| (s.code, s.text.as_str())).collect();
        assert_eq!(
            shape,
            [
                (false, "Add "),
                (true, "User-agent: GPTBot"),
                (false, " and "),
                (true, "Disallow: /"),
                (false, " to robots.txt.")
            ]
        );
    }

    #[test]
    fn outcomes_read_as_the_toast_says_them() {
        assert_eq!(outcome(false, 0, 0), "it applies from the next crawl");
        assert_eq!(outcome(true, 0, 0), "no issues opened or resolved");
        assert_eq!(outcome(true, 0, 2), "2 issues resolved");
        assert_eq!(outcome(true, 1, 0), "1 issue opened");
        assert_eq!(outcome(true, 1, 2), "2 issues resolved, 1 opened");
    }

    #[test]
    fn names_are_capped() {
        let n = |v: &[&str]| names(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(n(&["A"]), "A");
        assert_eq!(n(&["A", "B"]), "A and B");
        assert_eq!(n(&["A", "B", "C", "D", "E"]), "A, B, C and 2 more");
    }
}
