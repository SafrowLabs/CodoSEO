//! `/s/{site}/graph/reach`: which AI answers can read the site, drawn as a flow from robots.txt and the CDN through each AI crawler to the answer engine it feeds.
//!
//! Everything comes from the site's latest access report ([`geo::latest_report`]) and the owner's
//! intent: robots.txt verdicts per registry bot, the synthetic fetch checks (a CDN or firewall
//! that blocks a bot) and each engine's verdict as the AI access Overview gives it
//! ([`ai_overview::product_verdict`]). The flow is an inline SVG laid out here.

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use codoseo_geo::eligibility::{Engine, engines};
use codoseo_geo::probe::Verdict;
use codoseo_geo::report::AccessReport;
use codoseo_geo::robots::RobotsAvailability;
use codoseo_geo::{Bot, Intent, Purpose, Stance, registry};
use codoseo_store::geo;
use codoseo_store::sites::Site;

use super::llms::short;
use super::{Empty, Tab, TabLink};
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::layout::Shell;
use crate::render::html;
use crate::routes::ai_access::{names, plural};
use crate::routes::ai_overview::{self, Fetched, Fit, Reach};
use crate::routes::audit_ai::engine_short;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/s/{site}/graph/reach", get(reach_page))
}

/// The bot groups, top to bottom.
const GROUPS: [(Purpose, &str); 3] = [
    (Purpose::Search, "SEARCH"),
    (Purpose::UserFetch, "USER FETCH"),
    (Purpose::Training, "TRAINING"),
];

// The flow's geometry, in viewBox units.
const WIDTH: u32 = 1000;
const SITE_X: u32 = 10;
const SITE_W: u32 = 180;
const SITE_H: u32 = 88;
const BOT_X: u32 = 330;
const BOT_W: u32 = 236;
const BOT_ROW: u32 = 25;
const GROUP_GAP: u32 = 47;
const ENGINE_X: u32 = 786;
const ENGINE_ROW: u32 = 50;
const ENGINE_TOP: u32 = 60;
const BOX_X: u32 = 770;
const BOX_W: u32 = 180;

#[derive(Template)]
#[template(path = "graph/reach.html")]
pub struct ReachTab {
    pub shell: Shell,
    pub base: String,
    pub tabs: Vec<TabLink>,
    pub empty: Option<Empty>,
    /// `None` until a crawl has stored an access report.
    pub view: Option<Box<ReachView>>,
}

pub struct ReachView {
    pub svg: FlowSvg,
    /// Engines that can find and quote the site, of all of them.
    pub reach: usize,
    pub engines: usize,
    pub bar: Vec<BarPart>,
    pub problems: Vec<ProblemCard>,
    pub opted_out: Option<OptOutCard>,
    pub robots_href: String,
    pub access_href: String,
}

pub struct BarPart {
    pub n: usize,
    /// `is-ok`, `is-warn`, `is-err`, `is-off`
    pub class: &'static str,
}

pub struct ProblemCard {
    /// `is-err` or `is-warn`
    pub class: &'static str,
    pub title: String,
    pub why: String,
    pub fix: String,
    /// `robots.txt:22  User-agent: meta-webindexer`
    pub lines: Vec<String>,
}

pub struct OptOutCard {
    pub title: String,
    pub text: String,
}

pub struct FlowSvg {
    pub width: u32,
    pub height: u32,
    pub label: String,
    pub site: SiteCard,
    pub groups: Vec<GroupLabel>,
    pub bots: Vec<BotPill>,
    pub engines: Vec<EngineNode>,
    pub training: Option<TrainingBox>,
    pub edges: Vec<FlowEdge>,
}

pub struct SiteCard {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub name: String,
    /// `robots.txt · 14 rules`
    pub robots: String,
    /// `CDN · bot challenge on`
    pub cdn: String,
    /// The same at length, for the tooltip: `Cloudflare challenges some bots.`
    pub cdn_note: String,
}

pub struct GroupLabel {
    pub y: u32,
    pub text: &'static str,
}

pub struct BotPill {
    /// The pill's centre line.
    pub y: u32,
    pub token: String,
    pub title: String,
    /// `CDN 403`, `robots.txt`, `3 of 9 pages`
    pub reason: Option<String>,
    /// `is-ok`, `is-warn`, `is-err`, `is-off`, `is-unknown`
    pub class: &'static str,
}

pub struct EngineNode {
    pub y: u32,
    pub name: &'static str,
    /// `reachable`, `user fetch only`, `can't reach you`
    pub label: &'static str,
    /// `is-ok`, `is-warn`, `is-err`, `is-off`, `is-unknown`
    pub class: &'static str,
    pub logo: Option<&'static str>,
    pub monogram: String,
    pub why: String,
}

pub struct TrainingBox {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    /// `3 of 8 in`
    pub count: String,
}

pub struct FlowEdge {
    pub d: String,
    pub class: String,
}

async fn reach_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
) -> Result<Response, AppError> {
    let ctx = super::load(&state, &user, &site, Tab::Reach).await?;
    let view = match ctx.latest {
        Some(_) => match geo::latest_report(&state.pool, ctx.site.id).await? {
            Some(latest) => {
                let intent = geo::get_intent(&state.pool, ctx.site.id).await?;
                Some(Box::new(build(
                    &ctx.site,
                    &ctx.base,
                    &latest.report,
                    &intent,
                )))
            }
            None => None,
        },
        None => None,
    };
    Ok(html(&ReachTab {
        shell: ctx.shell,
        base: ctx.base,
        tabs: ctx.tabs,
        empty: ctx.empty,
        view,
    })?
    .into_response())
}

/// Where one bot stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotState {
    Allowed,
    /// Kept off some key pages only.
    Partly,
    /// Kept out where it costs reach.
    Blocked,
    /// Kept out by choice: a training bot, or one the intent says to block.
    OptedOut,
    /// robots.txt wasn't checked.
    Unknown,
}

impl BotState {
    fn class(self) -> &'static str {
        match self {
            BotState::Allowed => "is-ok",
            BotState::Partly => "is-warn",
            BotState::Blocked => "is-err",
            BotState::OptedOut => "is-off",
            BotState::Unknown => "is-unknown",
        }
    }

    fn words(self) -> &'static str {
        match self {
            BotState::Allowed => "allowed",
            BotState::Partly => "blocked on some pages",
            BotState::Blocked => "blocked",
            BotState::OptedOut => "opted out",
            BotState::Unknown => "not checked",
        }
    }
}

/// How the fetch as `token` was kept out, in a pill's few words: `CDN 403`, `CDN challenge`,
/// `CDN drop`. `None` when it wasn't.
pub fn fetch_block(report: &AccessReport, token: &str) -> Option<String> {
    let p = report.probes.as_ref()?;
    p.urls
        .iter()
        .flat_map(|u| &u.results)
        .filter(|r| r.token.eq_ignore_ascii_case(token))
        .find_map(|r| match &r.verdict {
            Verdict::Blocked { status: Some(s) } => Some(format!("CDN {s}")),
            Verdict::Blocked { status: None } => Some("CDN drop".to_owned()),
            Verdict::Challenged { .. } => Some("CDN challenge".to_owned()),
            _ => None,
        })
}

/// Where `bot` stands, and the reason a pill shows.
pub fn bot_state(report: &AccessReport, intent: &Intent, bot: &Bot) -> (BotState, Option<String>) {
    let by_choice = bot.purpose == Purpose::Training || intent.effective(bot) == Stance::Block;
    if !report.has_verdicts() {
        return match report.robots.availability {
            RobotsAvailability::Unavailable if by_choice => (BotState::OptedOut, None),
            RobotsAvailability::Unavailable => (BotState::Blocked, Some("robots.txt".to_owned())),
            _ => (BotState::Unknown, None),
        };
    }
    let access = report.bot(&bot.token);
    let home_blocked = access.and_then(|a| a.home_allowed) == Some(false);
    let cdn = fetch_block(report, &bot.token);
    if home_blocked || cdn.is_some() {
        if by_choice {
            return (BotState::OptedOut, None);
        }
        let reason = if home_blocked {
            "robots.txt".to_owned()
        } else {
            cdn.unwrap_or_default()
        };
        return (BotState::Blocked, Some(reason));
    }
    let blocked = access.map_or(0, |a| a.blocked.len());
    if blocked > 0 && bot.purpose != Purpose::Training {
        return (
            BotState::Partly,
            Some(format!("{blocked} of {} pages", report.important.len())),
        );
    }
    (BotState::Allowed, None)
}

/// `robots.txt · 14 rules`, or why there are none.
fn robots_words(report: &AccessReport) -> String {
    match (report.robots.availability, &report.robots.file) {
        (RobotsAvailability::Ok, Some(f)) => {
            let rules = f
                .body
                .lines()
                .filter(|l| {
                    let l = l.trim_start().to_ascii_lowercase();
                    l.starts_with("allow") || l.starts_with("disallow")
                })
                .count();
            format!("robots.txt · {}", plural(rules, "rule", "rules"))
        }
        (RobotsAvailability::Ok, None) => "robots.txt · read".to_owned(),
        (RobotsAvailability::Missing, _) => "robots.txt · none".to_owned(),
        (RobotsAvailability::Unavailable, _) => match report.robots.status {
            Some(s) => format!("robots.txt · HTTP {s}"),
            None => "robots.txt · no answer".to_owned(),
        },
        _ => "robots.txt · not checked".to_owned(),
    }
}

/// `CDN · bot challenge on` and the same at length, from the fetch checks.
fn cdn_words(report: &AccessReport) -> (String, String) {
    let Some(p) = &report.probes else {
        return (
            "Fetch as bots · not run".to_owned(),
            "No fetch as AI bots ran on the latest crawl.".to_owned(),
        );
    };
    let verdicts = || p.urls.iter().flat_map(|u| &u.results).map(|r| &r.verdict);
    if let Some(vendor) = verdicts().find_map(|v| match v {
        Verdict::Challenged { vendor } => Some(vendor),
        _ => None,
    }) {
        return (
            "CDN · bot challenge on".to_owned(),
            format!("{} challenges some AI bots.", vendor.label()),
        );
    }
    if verdicts().any(|v| matches!(v, Verdict::Blocked { .. })) {
        return (
            "CDN · blocks some bots".to_owned(),
            "Your CDN or firewall refuses some AI bots.".to_owned(),
        );
    }
    (
        "CDN · lets bots in".to_owned(),
        "Every AI bot we fetched as got the same page as CodoSEObot.".to_owned(),
    )
}

/// The robots.txt lines behind a rule at `line`: the nearest `User-agent` line above it, then
/// the rule, as `robots.txt:22  User-agent: x`.
fn robots_lines(report: &AccessReport, line: u32) -> Vec<String> {
    let Some(f) = &report.robots.file else {
        return Vec::new();
    };
    let lines: Vec<&str> = f.body.lines().collect();
    let at = line as usize;
    let Some(rule) = at.checked_sub(1).and_then(|i| lines.get(i)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some((i, agent)) = lines[..at - 1].iter().enumerate().rev().find(|(_, l)| {
        l.trim_start()
            .to_ascii_lowercase()
            .starts_with("user-agent")
    }) {
        out.push(format!("robots.txt:{}  {}", i + 1, agent.trim()));
    }
    out.push(format!("robots.txt:{line}  {}", rule.trim()));
    out
}

/// What to change so `engine` can read the site, and the robots.txt lines to change.
fn fix(report: &AccessReport, engine: &Engine) -> (String, Vec<String>) {
    let crawler = engine.crawler;
    if !report.has_verdicts() {
        return (
            "Fix: make robots.txt answer 200 (or 404 if you have none).".to_owned(),
            Vec::new(),
        );
    }
    let access = report.bot(crawler);
    if access.and_then(|a| a.home_allowed) == Some(false) {
        let lines = access
            .and_then(|a| a.home_rule.as_ref())
            .map_or_else(Vec::new, |r| robots_lines(report, r.line));
        return (
            format!("Fix: remove the Disallow for {crawler} in robots.txt."),
            lines,
        );
    }
    if fetch_block(report, crawler).is_some() {
        let operator = registry()
            .bot(crawler)
            .map_or(crawler, |b| b.operator.as_str());
        return (
            format!(
                "Fix: allow {operator}'s published IP ranges in your CDN or firewall bot rules."
            ),
            Vec::new(),
        );
    }
    if let Some((a, b)) = access.and_then(|a| a.blocked.first().map(|b| (a, b))) {
        let lines = report
            .rule_of(a, b)
            .map_or_else(Vec::new, |r| robots_lines(report, r.line));
        return (
            format!("Fix: check the Disallow rules for {crawler} in robots.txt."),
            lines,
        );
    }
    if let Fetched::Soft { .. } = ai_overview::fetched(report, crawler) {
        return (
            format!("Fix: serve {crawler} the same page other visitors get."),
            Vec::new(),
        );
    }
    (
        "Fix: remove the page markup that limits it on those pages.".to_owned(),
        Vec::new(),
    )
}

/// A connector from `(x1, y1)` to `(x2, y2)`, curving halfway across.
fn curve(x1: u32, y1: u32, x2: u32, y2: u32) -> String {
    let mid = (x1 + x2) / 2;
    format!("M{x1} {y1}C{mid} {y1} {mid} {y2} {x2} {y2}")
}

pub fn build(site: &Site, base: &str, report: &AccessReport, intent: &Intent) -> ReachView {
    let mut bots = Vec::new();
    let mut groups = Vec::new();
    let mut y = 36;
    for (purpose, text) in GROUPS {
        groups.push(GroupLabel { y, text });
        y += 20;
        for b in registry().bots.iter().filter(|b| b.purpose == purpose) {
            let (state, reason) = bot_state(report, intent, b);
            bots.push((b, state, reason, y));
            y += BOT_ROW;
        }
        y += GROUP_GAP - BOT_ROW;
    }
    let last = bots.last().map_or(y, |b| b.3);
    let first = bots.first().map_or(y, |b| b.3);
    let site_mid = (first + last) / 2;
    let (cdn, cdn_note) = cdn_words(report);
    let site_card = SiteCard {
        x: SITE_X,
        y: site_mid - SITE_H / 2,
        w: SITE_W,
        h: SITE_H,
        name: short(&site.domain, 20),
        robots: robots_words(report),
        cdn,
        cdn_note,
    };
    let y_of = |token: &str| {
        bots.iter()
            .find(|b| b.0.token.eq_ignore_ascii_case(token))
            .map(|b| b.3)
    };
    let state_of = |token: &str| {
        bots.iter()
            .find(|b| b.0.token.eq_ignore_ascii_case(token))
            .map(|b| b.1)
    };

    // Engines in the order of their crawlers, so most connectors run straight across.
    let mut order: Vec<&Engine> = engines().iter().collect();
    order.sort_by_key(|e| y_of(e.crawler).unwrap_or(u32::MAX));
    let mut edges = Vec::new();
    for b in &bots {
        let class = match b.1 {
            BotState::OptedOut => "gr-edge is-off".to_owned(),
            s => format!("gr-edge {}", s.class()),
        };
        edges.push(FlowEdge {
            d: curve(SITE_X + SITE_W, site_mid, BOT_X - 8, b.3),
            class,
        });
    }
    let mut nodes = Vec::new();
    let mut bar = [0usize; 4];
    let mut problems = Vec::new();
    let mut reach = 0;
    for (i, e) in order.iter().enumerate() {
        let ey = ENGINE_TOP + ENGINE_ROW * u32::try_from(i).unwrap_or(0);
        let v = ai_overview::product_verdict(report, intent, e);
        let fetcher = ai_overview::user_fetcher(e);
        let crawler_out = matches!(
            state_of(e.crawler),
            Some(BotState::Blocked | BotState::OptedOut)
        );
        let (class, label) = match (v.fit, v.reach) {
            (Fit::PerPolicy, _) => ("is-off", "blocked, per your policy"),
            (Fit::NoPreference, Reach::Blocked) => ("is-off", "blocked"),
            (_, Reach::Reachable | Reach::Allowed) => ("is-ok", "reachable"),
            (_, Reach::Limited) if crawler_out => ("is-warn", "user fetch only"),
            (_, Reach::Limited) => ("is-warn", "partly reachable"),
            (_, Reach::Blocked) => ("is-err", "can't reach you"),
            (_, Reach::Unknown) => ("is-unknown", "not checked"),
        };
        if v.finds() {
            reach += 1;
        }
        bar[match class {
            "is-ok" => 0,
            "is-warn" => 1,
            "is-err" => 2,
            _ => 3,
        }] += 1;
        let name = engine_short(e.id);
        let operator = registry()
            .bot(e.crawler)
            .map_or_else(|| e.name.to_owned(), |b| b.operator.clone());
        if matches!(class, "is-err" | "is-warn") {
            let (fix, lines) = fix(report, e);
            problems.push(ProblemCard {
                class,
                title: match label {
                    "user fetch only" => format!("{name} search is blocked"),
                    "partly reachable" => format!("{name} is partly blocked"),
                    _ => format!("{name} can't reach you"),
                },
                why: v.why.clone(),
                fix,
                lines,
            });
        }
        for token in std::iter::once(e.crawler).chain(fetcher.map(|f| f.token.as_str())) {
            let Some(b) = bots.iter().find(|b| b.0.token.eq_ignore_ascii_case(token)) else {
                continue;
            };
            let class = match b.1 {
                BotState::Blocked => "gr-edge is-err is-dashed".to_owned(),
                s => format!("gr-edge {}", s.class()),
            };
            edges.push(FlowEdge {
                d: curve(BOT_X + BOT_W, b.3, ENGINE_X - 16, ey),
                class,
            });
        }
        nodes.push(EngineNode {
            y: ey,
            name,
            label,
            class,
            logo: crate::operator_logo::logo(&operator),
            monogram: crate::operator_logo::monogram(&operator),
            why: v.why,
        });
    }
    // Errors first, then partly reachable ones.
    problems.sort_by_key(|p| p.class != "is-err");

    let training: Vec<_> = bots
        .iter()
        .filter(|b| b.0.purpose == Purpose::Training)
        .collect();
    let training_box = (!training.is_empty()).then(|| {
        let mid = (training[0].3 + training[training.len() - 1].3) / 2;
        let after = ENGINE_TOP + ENGINE_ROW * u32::try_from(order.len()).unwrap_or(0) + 22;
        let by = mid.saturating_sub(18).max(after);
        let inside = training
            .iter()
            .filter(|b| !matches!(b.1, BotState::OptedOut | BotState::Blocked))
            .count();
        for b in &training {
            let class = match b.1 {
                BotState::OptedOut | BotState::Blocked => "gr-edge is-off",
                _ => "gr-edge is-ok",
            };
            edges.push(FlowEdge {
                d: curve(BOT_X + BOT_W, b.3, BOX_X, by + 18),
                class: class.to_owned(),
            });
        }
        TrainingBox {
            x: BOX_X,
            y: by,
            w: BOX_W,
            count: format!("{inside} of {} in", training.len()),
        }
    });
    let opted: Vec<String> = training
        .iter()
        .filter(|b| b.1 == BotState::OptedOut)
        .map(|b| b.0.token.clone())
        .collect();
    let opted_out = (!opted.is_empty()).then(|| {
        let mut text = format!(
            "A choice, not a problem. Blocking a training crawler does not take you out of AI answers: {}.",
            names(&opted)
        );
        if opted.iter().any(|t| t == "Google-Extended") {
            text.push_str(" Google-Extended also covers Gemini grounding.");
        }
        OptOutCard {
            title: format!(
                "{} opted out",
                plural(opted.len(), "training crawler", "training crawlers")
            ),
            text,
        }
    });

    let height = last
        .max(training_box.as_ref().map_or(0, |b| b.y + 36))
        .max(site_card.y + SITE_H)
        + 24;
    let blocked: Vec<&str> = nodes
        .iter()
        .filter(|n| matches!(n.class, "is-err" | "is-warn"))
        .map(|n| n.name)
        .collect();
    let mut label = format!(
        "Flow from robots.txt and the CDN through {} to {}. {} of {} can read the site.",
        plural(bots.len(), "AI crawler", "AI crawlers"),
        plural(nodes.len(), "answer engine", "answer engines"),
        reach,
        nodes.len()
    );
    if !blocked.is_empty() {
        label.push_str(&format!(
            " Blocked or partly blocked: {}.",
            blocked.join(", ")
        ));
    }
    if !opted.is_empty() {
        label.push_str(&format!(
            " {} opted out.",
            plural(opted.len(), "training crawler", "training crawlers")
        ));
    }
    let pills = bots
        .iter()
        .map(|(b, state, reason, y)| BotPill {
            y: *y,
            token: b.token.clone(),
            title: format!("{} · {}: {}", b.token, b.product, state.words()),
            reason: match state {
                BotState::Blocked | BotState::Partly => reason.clone(),
                _ => None,
            },
            class: state.class(),
        })
        .collect();
    ReachView {
        svg: FlowSvg {
            width: WIDTH,
            height,
            label,
            site: site_card,
            groups,
            bots: pills,
            engines: nodes,
            training: training_box,
            edges,
        },
        reach,
        engines: engines().len(),
        bar: ["is-ok", "is-warn", "is-err", "is-off"]
            .into_iter()
            .zip(bar)
            .filter(|(_, n)| *n > 0)
            .map(|(class, n)| BarPart { n, class })
            .collect(),
        problems,
        opted_out,
        robots_href: format!("{base}/ai-access/robots"),
        access_href: format!("{base}/ai-access"),
    }
}
