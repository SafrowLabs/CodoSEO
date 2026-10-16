//! The Bots tab of `/s/{site}/ai-access`: every registry bot in one list (your policy, what
//! robots.txt says, what a fetch as the bot got, the status) and a side panel for one bot that
//! explains each verdict.
//!
//! The list's state lives in the query (`?show=attention|all|training`, `?group=purpose|company`,
//! `?q=` and `?bot=`), so every view can be linked and works without JavaScript; a row asks for
//! just the panel when htmx is there (`hx-get`, target `#aia-panel`).

use codoseo_geo::findings::Evidence;
use codoseo_geo::probe::{ProbeFetch, SkipReason, Verdict};
use codoseo_geo::report::{AccessReport, agent_rules};
use codoseo_geo::robots::{GroupMatch, RobotsAvailability, RobotsTxt};
use codoseo_geo::{Bot, Honours, Intent, Purpose, Stance, registry};
use codoseo_store::geo::Incident;
use serde::Deserialize;

use crate::routes::ai_access::{
    FetchCheck, inconclusive_tip, path_of, plural, purpose_label, size, stance_class, stance_label,
    status_text,
};
use crate::routes::ai_overview::{Fetched, fetched};

/// Purposes in the order the list groups them.
const PURPOSES: [Purpose; 5] = [
    Purpose::Search,
    Purpose::UserFetch,
    Purpose::Agent,
    Purpose::Training,
    Purpose::Ads,
];

/// Lines of the bot's group shown in the panel before "… more on the robots.txt tab".
const EXCERPT_LINES: usize = 24;
/// Key page paths named in the panel's "Key pages" line.
const SAMPLE_PATHS: usize = 3;
/// Longest search the list keeps, in characters.
const MAX_QUERY: usize = 80;

#[derive(Debug, Default, Deserialize)]
pub struct BotsQuery {
    pub show: Option<String>,
    pub group: Option<String>,
    pub bot: Option<String>,
    pub q: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Show {
    Attention,
    All,
    Training,
}

impl Show {
    fn slug(self) -> &'static str {
        match self {
            Show::Attention => "attention",
            Show::All => "all",
            Show::Training => "training",
        }
    }

    fn from_slug(s: &str) -> Option<Show> {
        [Show::Attention, Show::All, Show::Training]
            .into_iter()
            .find(|v| v.slug() == s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupBy {
    Purpose,
    Company,
}

impl GroupBy {
    fn slug(self) -> &'static str {
        match self {
            GroupBy::Purpose => "purpose",
            GroupBy::Company => "company",
        }
    }
}

pub struct BotsTab {
    pub show: &'static str,
    pub group: &'static str,
    pub q: String,
    pub filters: Vec<SegLink>,
    pub groupings: Vec<SegLink>,
    /// "Clear search", when there is one.
    pub clear_href: Option<String>,
    pub groups: Vec<ListGroup>,
    /// Rows shown, over every group.
    pub shown: usize,
    /// Why nothing shows, when nothing does.
    pub none: Option<&'static str>,
    pub panel: Option<BotPanel>,
    /// A bot was picked (`?bot=`): on a phone the panel comes first.
    pub picked: bool,
}

/// One choice of the toolbar's segmented controls, as a link.
pub struct SegLink {
    pub label: &'static str,
    pub count: String,
    pub href: String,
    pub current: bool,
}

pub struct ListGroup {
    pub label: String,
    /// `index pages so AI answers can cite them · 10 bots`
    pub meta: String,
    /// `2 need attention`, `no conflicts`
    pub flag: String,
    pub flag_class: &'static str,
    pub rows: Vec<ListRow>,
}

#[derive(Clone)]
pub struct ListRow {
    pub token: String,
    pub operator: String,
    pub product: String,
    pub control: bool,
    pub notes: String,
    pub stance: &'static str,
    pub stance_class: &'static str,
    pub overridden: bool,
    pub robots: &'static str,
    pub robots_class: &'static str,
    pub robots_sub: String,
    pub fetch: String,
    pub fetch_class: &'static str,
    pub fetch_sub: String,
    pub status: &'static str,
    pub status_class: &'static str,
    pub status_tip: &'static str,
    pub attention: bool,
    pub selected: bool,
    pub href: String,
    /// Lower-cased `token operator product`, for the search box.
    pub search: String,
}

/// The side panel: one bot, each verdict with its reason.
pub struct BotPanel {
    pub token: String,
    pub operator: String,
    pub product: String,
    pub purpose: &'static str,
    pub stance: &'static str,
    pub stance_class: &'static str,
    pub status: &'static str,
    pub status_class: &'static str,
    pub robots_head: String,
    pub robots_class: &'static str,
    pub robots_why: String,
    /// The group(s) it obeys, line by line; empty when the body isn't stored or there is none.
    pub excerpt: Vec<ExcerptLine>,
    /// `… 12 more lines`, linking to the robots.txt tab.
    pub excerpt_more: Option<String>,
    /// Shown instead of the excerpt when the file's text isn't in this report.
    pub excerpt_note: Option<&'static str>,
    pub robots_href: String,
    pub fetch_head: String,
    pub fetch_class: &'static str,
    pub fetch_why: String,
    pub compare: Option<Compare>,
    pub pages_line: String,
    pub ua: String,
    pub honours: &'static str,
    pub honours_class: &'static str,
    pub verify: Vec<(String, Option<String>)>,
    pub source_url: String,
    pub source_host: String,
    pub reviewed: String,
    pub issue_href: Option<String>,
    pub policy_href: String,
}

pub struct ExcerptLine {
    pub n: u32,
    pub text: String,
    pub comment: bool,
    /// The line that decides the home page: `decides /`.
    pub mark: Option<String>,
    /// A gap between two groups it reads.
    pub gap: bool,
}

/// The home page as CodoSEObot and as the bot got it.
pub struct Compare {
    pub page: String,
    pub rows: Vec<CompareRow>,
}

pub struct CompareRow {
    pub label: &'static str,
    pub baseline: String,
    pub bot: String,
    pub differs: bool,
}

/// What the fetch check says about one bot, for the list and the panel.
struct FetchView {
    text: String,
    class: &'static str,
    sub: String,
    head: String,
    head_class: &'static str,
    why: String,
    /// Kept out (a hard block) or treated worse (a soft one), for a bot you allow.
    problem: Option<bool>,
}

/// Everything about one bot the list and the panel need.
struct Assessed {
    bot: &'static Bot,
    row: ListRow,
    home_allowed: Option<bool>,
    blocked: Vec<u16>,
    fetch: FetchView,
}

/// What the Bots tab needs from the page.
pub struct BotsInput<'a> {
    pub base: &'a str,
    pub report: &'a AccessReport,
    pub intent: &'a Intent,
    pub fetch: &'a FetchCheck,
    pub open: &'a [Incident],
}

fn group_label(group: GroupMatch, token: &str) -> String {
    match group {
        GroupMatch::Named => "own group".to_owned(),
        GroupMatch::Fallback => registry()
            .robots_fallback(token)
            .map_or("another bot's group".to_owned(), |f| format!("{f}'s group")),
        GroupMatch::Wildcard => "group *".to_owned(),
        GroupMatch::None => "no group".to_owned(),
    }
}

/// The verdict word and class of a fetch summary for the list, and the panel's line.
fn fetch_view(report: &AccessReport, fetch: &FetchCheck, bot: &Bot, stance: Stance) -> FetchView {
    let quiet = |text: &str, sub: &str, why: String| FetchView {
        text: text.to_owned(),
        class: "c-muted",
        sub: sub.to_owned(),
        head: "Not checked".to_owned(),
        head_class: "c-muted",
        why,
        problem: None,
    };
    if !bot.crawls {
        return quiet(
            "—",
            "control token",
            "A control token never fetches pages: it only says what the operator may do with them."
                .to_owned(),
        );
    }
    if let Some(e) = &fetch.empty {
        return quiet("—", e.title, format!("{}.", e.title));
    }
    let wanted = stance == Stance::Allow;
    let not_reported = if wanted {
        ""
    } else {
        " You don't ask for this bot, so it isn't reported."
    };
    match fetched(report, &bot.token) {
        Fetched::NotRun => quiet(
            "—",
            "no fetch check yet",
            "Fetch checks run after the next crawl.".to_owned(),
        ),
        Fetched::Skipped(Some(SkipReason::NoUserAgent)) => quiet(
            "Not checked",
            "no full user-agent published",
            "Its operator publishes no full user agent, and CodoSEO never makes one up.".to_owned(),
        ),
        Fetched::Skipped(Some(SkipReason::VerifiedSearchEngine)) => quiet(
            "Not checked",
            "verified search engine",
            "A verified search engine: bot managers check it by address, so its user agent sent from CodoSEO's servers would always look blocked.".to_owned(),
        ),
        Fetched::Skipped(_) => quiet(
            "—",
            "only search and user bots are fetched",
            "Only search crawlers and user-triggered fetchers are fetched: not training or ads crawlers, nor agents.".to_owned(),
        ),
        Fetched::Unjudged => {
            let first = first_result(report, &bot.token, |_| true);
            match first.map(|(_, v, _)| v) {
                Some(Verdict::Inconclusive { reason }) => FetchView {
                    text: "Inconclusive".to_owned(),
                    class: "c-muted",
                    sub: inconclusive_tip(*reason).to_owned(),
                    head: "Inconclusive".to_owned(),
                    head_class: "c-muted",
                    why: format!("{}.", inconclusive_tip(*reason)),
                    problem: None,
                },
                _ => FetchView {
                    text: "Not fetched".to_owned(),
                    class: "c-muted",
                    sub: "robots.txt keeps it out, so we don't".to_owned(),
                    head: "Not fetched".to_owned(),
                    head_class: "c-muted",
                    why: "robots.txt keeps it out of the pages checked, so its user agent wasn't sent.".to_owned(),
                    problem: None,
                },
            }
        }
        Fetched::Same { same, of } => FetchView {
            text: "Same page as CodoSEObot".to_owned(),
            class: "c-ok",
            sub: format!("{same} of {}", plural(of, "page", "pages")),
            head: "Yes — it gets the same page as CodoSEObot".to_owned(),
            head_class: "c-ok",
            why: format!(
                "We fetched {} with this bot's user agent and compared the answers with CodoSEObot's: same status, size and title.",
                plural(of, "key page", "key pages")
            ),
            problem: None,
        },
        Fetched::Hard { worse, of, what } => {
            let first = first_result(report, &bot.token, |v| {
                matches!(v, Verdict::Blocked { .. } | Verdict::Challenged { .. })
            });
            let (text, sub) = match first {
                Some((_, Verdict::Challenged { vendor }, f)) => (
                    f.and_then(|f| f.status)
                        .map_or("Challenged".to_owned(), |s| format!("Blocked · {s}")),
                    format!("{} challenge on {worse} of {of} pages", vendor.label()),
                ),
                Some((_, Verdict::Blocked { status: Some(s) }, _)) => (
                    format!("Blocked · {s}"),
                    format!("on {worse} of {of} pages"),
                ),
                _ => (
                    "Dropped".to_owned(),
                    format!("connection dropped on {worse} of {of} pages"),
                ),
            };
            FetchView {
                text,
                class: if wanted { "c-err" } else { "c-muted" },
                sub,
                head: format!("No — {what} on {worse} of {of} pages"),
                head_class: if wanted { "c-err" } else { "c-muted" },
                why: format!(
                    "Our request with its user agent was {what}. A crawler can't get past that, so {} can't read those pages. The request came from CodoSEO's servers; the real bot comes from {}'s addresses and may be treated differently, so confirm in your CDN or firewall.{not_reported}",
                    bot.product, bot.operator
                ),
                problem: wanted.then_some(true),
            }
        }
        Fetched::Soft { worse, of, what } => {
            let first = first_result(report, &bot.token, Verdict::is_positive);
            let text = match first.map(|(_, v, _)| v) {
                Some(Verdict::Diverted { .. }) => "Redirected",
                Some(Verdict::Throttled) => "Rate limited",
                _ => "Different page",
            };
            FetchView {
                text: text.to_owned(),
                class: if wanted { "c-warn" } else { "c-muted" },
                sub: format!("on {worse} of {of} pages"),
                head: format!("Partly — {what} on {worse} of {of} pages"),
                head_class: if wanted { "c-warn" } else { "c-muted" },
                why: format!(
                    "It got in, but was {what}, unlike CodoSEObot. The request came from CodoSEO's servers; the real bot may be treated differently.{not_reported}"
                ),
                problem: wanted.then_some(false),
            }
        }
    }
}

/// The first probed page where `token`'s verdict passes `pick`: the page's baseline, the verdict
/// and what the bot got.
fn first_result<'a>(
    report: &'a AccessReport,
    token: &str,
    pick: impl Fn(&Verdict) -> bool,
) -> Option<(&'a ProbeFetch, &'a Verdict, Option<&'a ProbeFetch>)> {
    report.probes.as_ref()?.urls.iter().find_map(|u| {
        u.results
            .iter()
            .find(|r| r.token.eq_ignore_ascii_case(token) && pick(&r.verdict))
            .map(|r| (&u.baseline, &r.verdict, r.fetch.as_ref()))
    })
}

fn assess(input: &BotsInput, bot: &'static Bot) -> Assessed {
    let report = input.report;
    let stance = input.intent.effective(bot);
    let access = report.bot(&bot.token);
    let home_allowed = access.and_then(|a| a.home_allowed);
    let blocked: Vec<u16> =
        access.map_or(Vec::new(), |a| a.blocked.iter().map(|b| b.url).collect());
    let total = report.important.len();
    let honoured = bot.honours_robots == Honours::Yes;
    let fetch = fetch_view(report, input.fetch, bot, stance);

    let (robots, robots_class) = match home_allowed {
        Some(true) => (
            "Allowed",
            match stance {
                Stance::Allow if blocked.is_empty() => "c-ok",
                Stance::Allow => "c-warn",
                Stance::Block if honoured => "c-warn",
                _ => "",
            },
        ),
        Some(false) => (
            "Disallowed",
            match stance {
                Stance::Allow => "c-err",
                Stance::Block => "c-ok",
                Stance::Any => "c-muted",
            },
        ),
        None => ("—", "c-ghost"),
    };
    let robots_sub = match (access, report.robots.availability) {
        (_, RobotsAvailability::Missing) => "no robots.txt".to_owned(),
        (Some(a), _) => {
            let place = match &a.home_rule {
                Some(r) => format!("line {} · {}", r.line, group_label(a.group, &bot.token)),
                None => format!("no rule · {}", group_label(a.group, &bot.token)),
            };
            if !blocked.is_empty() && home_allowed == Some(true) {
                format!("{place} · {} of {total} key pages blocked", blocked.len())
            } else {
                place
            }
        }
        (None, _) => String::new(),
    };

    // The status: robots.txt against the policy first, then the fetch check.
    let (status, status_class, status_tip, attention) = match (stance, home_allowed) {
        (_, None) => ("—", "", "robots.txt gave no verdict", false),
        (Stance::Allow, Some(false)) => (
            "Blocked",
            if honoured { "t-err" } else { "t-muted" },
            "You allow it, but robots.txt blocks it",
            true,
        ),
        (Stance::Allow, Some(true)) if !blocked.is_empty() => (
            "Partly blocked",
            if honoured { "t-warn" } else { "t-muted" },
            "You allow it, but robots.txt blocks it on some key pages",
            true,
        ),
        (Stance::Allow, Some(true)) => match fetch.problem {
            Some(true) => (
                "Firewall blocks",
                "t-err",
                "robots.txt lets it in, but a fetch as this bot was turned away",
                true,
            ),
            Some(false) => (
                "Treated differently",
                "t-warn",
                "robots.txt lets it in, but a fetch as this bot got a different answer",
                true,
            ),
            None => ("OK", "t-ok", "Allowed, as you want", false),
        },
        (Stance::Block, Some(false)) => {
            ("Blocked, as wanted", "t-ok", "Blocked, as you want", false)
        }
        (Stance::Block, Some(true)) if honoured => (
            "Not blocked",
            "t-warn",
            "You block it, but robots.txt lets it in",
            true,
        ),
        (Stance::Block, Some(true)) => (
            "Not blocked",
            "t-muted",
            "Not blocked, and its operator doesn't promise to follow robots.txt anyway",
            false,
        ),
        (Stance::Any, Some(false)) => {
            ("Blocked", "t-muted", "No preference: never reported", false)
        }
        (Stance::Any, Some(true)) => (
            "Allowed (no preference)",
            "t-muted",
            "No preference: never reported",
            false,
        ),
    };

    Assessed {
        bot,
        home_allowed,
        blocked,
        row: ListRow {
            token: bot.token.clone(),
            operator: bot.operator.clone(),
            product: bot.product.clone(),
            control: !bot.crawls,
            notes: bot.notes.clone(),
            stance: stance_label(stance),
            stance_class: stance_class(stance),
            overridden: input.intent.bot_override(&bot.token).is_some(),
            robots,
            robots_class,
            robots_sub,
            fetch: fetch.text.clone(),
            fetch_class: fetch.class,
            fetch_sub: fetch.sub.clone(),
            status,
            status_class,
            status_tip,
            attention,
            selected: false,
            href: String::new(),
            search: format!("{} {} {}", bot.token, bot.operator, bot.product).to_lowercase(),
        },
        fetch,
    }
}

fn purpose_meta(p: Purpose) -> &'static str {
    match p {
        Purpose::Search => "index pages so AI answers can cite them",
        Purpose::UserFetch => "fetch a page when a person asks",
        Purpose::Agent => "act on pages for a person",
        Purpose::Training => "no effect on AI search",
        Purpose::Ads => "check ad landing pages",
    }
}

fn group_of(label: String, meta: &str, rows: Vec<ListRow>) -> ListGroup {
    let bad = rows.iter().filter(|r| r.attention).count();
    ListGroup {
        label,
        meta: format!("{meta} · {}", plural(rows.len(), "bot", "bots")),
        flag: if bad == 0 {
            "no conflicts".to_owned()
        } else if bad == 1 {
            "1 needs attention".to_owned()
        } else {
            format!("{bad} need attention")
        },
        flag_class: if bad == 0 { "c-ok" } else { "c-err" },
        rows,
    }
}

/// A clean search: trimmed, at most [`MAX_QUERY`] characters.
fn clean_query(q: Option<&str>) -> String {
    q.unwrap_or("").trim().chars().take(MAX_QUERY).collect()
}

fn encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

fn list_href(base: &str, show: Show, group: GroupBy, q: &str, bot: Option<&str>) -> String {
    let mut href = format!(
        "{base}/ai-access/bots?show={}&group={}",
        show.slug(),
        group.slug()
    );
    if !q.is_empty() {
        href.push_str(&format!("&q={}", encode(q)));
    }
    if let Some(b) = bot {
        href.push_str(&format!("&bot={}", encode(b)));
    }
    href
}

/// Bots whose status needs attention: robots.txt or the fetch check against the policy.
pub fn attention(input: &BotsInput) -> usize {
    if !input.report.has_verdicts() {
        return 0;
    }
    registry()
        .bots
        .iter()
        .filter(|b| assess(input, b).row.attention)
        .count()
}

pub fn bots_tab(input: &BotsInput, query: &BotsQuery) -> BotsTab {
    let assessed: Vec<Assessed> = registry().bots.iter().map(|b| assess(input, b)).collect();
    let attention = assessed.iter().filter(|a| a.row.attention).count();
    let training = assessed
        .iter()
        .filter(|a| a.bot.purpose == Purpose::Training)
        .count();
    let show = query
        .show
        .as_deref()
        .and_then(Show::from_slug)
        .unwrap_or(if attention > 0 {
            Show::Attention
        } else {
            Show::All
        });
    let group = match query.group.as_deref() {
        Some("company") => GroupBy::Company,
        _ => GroupBy::Purpose,
    };
    let q = clean_query(query.q.as_deref());
    let needle = q.to_lowercase();
    let picked = query
        .bot
        .as_deref()
        .and_then(|t| registry().bot(t))
        .map(|b| b.token.clone());

    let visible = |a: &Assessed| {
        (match show {
            Show::Attention => a.row.attention,
            Show::All => true,
            Show::Training => a.bot.purpose == Purpose::Training,
        }) && (needle.is_empty() || a.row.search.contains(&needle))
    };
    let shown: Vec<&Assessed> = assessed.iter().filter(|a| visible(a)).collect();
    // The panel's bot: the one asked for, else the first that needs attention, else the first.
    let selected: Option<&Assessed> = picked
        .as_deref()
        .and_then(|t| assessed.iter().find(|a| a.bot.token == t))
        .or_else(|| shown.iter().copied().find(|a| a.row.attention))
        .or_else(|| shown.first().copied())
        .or_else(|| assessed.iter().find(|a| a.row.attention))
        .or_else(|| assessed.first());
    let selected_token = selected.map(|a| a.bot.token.clone());

    let row_of = |a: &Assessed| ListRow {
        selected: selected_token.as_deref() == Some(a.bot.token.as_str()),
        href: list_href(input.base, show, group, &q, Some(&a.bot.token)),
        ..a.row.clone()
    };
    let groups: Vec<ListGroup> = match group {
        GroupBy::Purpose => PURPOSES
            .iter()
            .map(|&p| {
                group_of(
                    purpose_label(p).to_owned(),
                    purpose_meta(p),
                    shown
                        .iter()
                        .filter(|a| a.bot.purpose == p)
                        .map(|a| row_of(a))
                        .collect(),
                )
            })
            .collect::<Vec<_>>(),
        GroupBy::Company => {
            let mut operators: Vec<&str> = Vec::new();
            for b in &registry().bots {
                if !operators.contains(&b.operator.as_str()) {
                    operators.push(&b.operator);
                }
            }
            operators
                .into_iter()
                .map(|op| {
                    group_of(
                        op.to_owned(),
                        "every purpose",
                        shown
                            .iter()
                            .filter(|a| a.bot.operator == op)
                            .map(|a| row_of(a))
                            .collect(),
                    )
                })
                .collect::<Vec<_>>()
        }
    }
    .into_iter()
    .filter(|g| !g.rows.is_empty())
    .collect();

    let seg = |label, count: String, href: String, current| SegLink {
        label,
        count,
        href,
        current,
    };
    let filters = vec![
        seg(
            "Needs attention",
            attention.to_string(),
            list_href(input.base, Show::Attention, group, &q, None),
            show == Show::Attention,
        ),
        seg(
            "All bots",
            assessed.len().to_string(),
            list_href(input.base, Show::All, group, &q, None),
            show == Show::All,
        ),
        seg(
            "Training only",
            training.to_string(),
            list_href(input.base, Show::Training, group, &q, None),
            show == Show::Training,
        ),
    ];
    let groupings = vec![
        seg(
            "By purpose",
            String::new(),
            list_href(input.base, show, GroupBy::Purpose, &q, None),
            group == GroupBy::Purpose,
        ),
        seg(
            "By company",
            String::new(),
            list_href(input.base, show, GroupBy::Company, &q, None),
            group == GroupBy::Company,
        ),
    ];
    let none = shown.is_empty().then_some(match (show, q.is_empty()) {
        (_, false) => "No bot matches the search.",
        (Show::Attention, true) => {
            "Nothing needs attention: every bot gets what your policy asks for."
        }
        _ => "No bots to show.",
    });
    BotsTab {
        show: show.slug(),
        group: group.slug(),
        clear_href: (!q.is_empty()).then(|| list_href(input.base, show, group, "", None)),
        q,
        filters,
        groupings,
        shown: shown.len(),
        groups,
        none,
        panel: selected.map(|a| panel(input, a)),
        picked: picked.is_some(),
    }
}

/// The panel for `token` alone, for the htmx swap: `None` for a token not in the registry.
pub fn panel_for(input: &BotsInput, token: &str) -> Option<BotPanel> {
    let bot = registry().bot(token)?;
    Some(panel(input, &assess(input, bot)))
}

fn honours_text(h: Honours) -> (&'static str, &'static str) {
    match h {
        Honours::Yes => ("Yes", "c-ok"),
        Honours::Partial => ("Partly — not for every request", "c-muted"),
        Honours::No => ("No — robots.txt can't stop it", "c-warn"),
        Honours::Unknown => ("Not confirmed by the operator", "c-muted"),
    }
}

/// `2026-10-11` → `11 Oct 2026`; anything else as written.
fn reviewed(date: &str) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let parts: Vec<&str> = date.split('-').collect();
    match parts.as_slice() {
        [y, m, d] => match (m.parse::<usize>(), d.parse::<u32>()) {
            (Ok(m @ 1..=12), Ok(d)) => format!("{d} {} {y}", MONTHS[m - 1]),
            _ => date.to_owned(),
        },
        _ => date.to_owned(),
    }
}

fn host_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| {
            u.host_str()
                .map(|h| h.trim_start_matches("www.").to_owned())
        })
        .unwrap_or_else(|| url.to_owned())
}

/// The lines of the groups `rules` reads, the home page's deciding line marked.
pub(crate) fn excerpt(
    body: &str,
    robots: &RobotsTxt,
    token: &str,
    deciding: Option<u32>,
    path: &str,
) -> (Vec<ExcerptLine>, usize) {
    let lines: Vec<&str> = body
        .strip_prefix('\u{feff}')
        .unwrap_or(body)
        .lines()
        .collect();
    let outline = robots.outline();
    let agent = agent_rules(robots, token);
    let mut out: Vec<ExcerptLine> = Vec::new();
    let mut total = 0usize;
    for (i, &g) in agent.groups().iter().enumerate() {
        let Some(group) = outline.get(g as usize) else {
            continue;
        };
        for n in group.first_line..=group.last_line {
            total += 1;
            if out.len() >= EXCERPT_LINES {
                continue;
            }
            let text = lines.get(n as usize - 1).copied().unwrap_or("");
            out.push(ExcerptLine {
                n,
                text: text.to_owned(),
                comment: text.trim_start().starts_with('#'),
                mark: (Some(n) == deciding).then(|| format!("decides {path}")),
                gap: i > 0 && n == group.first_line,
            });
        }
    }
    let more = total.saturating_sub(out.len());
    (out, more)
}

fn panel(input: &BotsInput, a: &Assessed) -> BotPanel {
    let report = input.report;
    let bot = a.bot;
    let token = &bot.token;
    let stance = input.intent.effective(bot);
    let access = report.bot(token);
    let total = report.important.len();
    let group = access.map(|a| a.group);
    let rule = access.and_then(|a| a.home_rule.as_ref());
    let group_name = match group {
        Some(GroupMatch::Fallback) => registry()
            .robots_fallback(token)
            .map_or("another bot's".to_owned(), |f| format!("{f}'s")),
        Some(GroupMatch::Wildcard) => "User-agent: *".to_owned(),
        _ => "its own".to_owned(),
    };
    let blocked_paths = |n: usize| -> String {
        let paths: Vec<String> = a
            .blocked
            .iter()
            .filter_map(|i| report.important.get(usize::from(*i)))
            .take(n)
            .map(|u| path_of(&u.url))
            .collect();
        let more = a.blocked.len().saturating_sub(paths.len());
        let mut s = paths.join(", ");
        if more > 0 {
            s.push_str(&format!(" +{more}"));
        }
        s
    };
    let (robots_head, robots_class, robots_why) = match (report.robots.availability, a.home_allowed)
    {
        (RobotsAvailability::Missing, _) => (
            "Yes — there is no robots.txt".to_owned(),
            if stance == Stance::Block {
                "c-warn"
            } else {
                "c-ok"
            },
            "Without a robots.txt every bot that follows it may crawl every page.".to_owned(),
        ),
        (_, None) => (
            "No verdict".to_owned(),
            "c-muted",
            "robots.txt couldn't be read on the latest crawl, so there is no verdict for any bot."
                .to_owned(),
        ),
        (_, Some(false)) => {
            let head = match group {
                Some(GroupMatch::Named) => format!("No — a group names {token}"),
                Some(GroupMatch::Fallback) => format!("No — it follows {group_name} group"),
                _ => "No — the User-agent: * group keeps it out".to_owned(),
            };
            let why = match rule {
                Some(r) => format!(
                    "Line {} “{}: {}” decides the home page, so {} of {} off limits.",
                    r.line,
                    if r.allow { "Allow" } else { "Disallow" },
                    r.pattern,
                    a.blocked.len(),
                    plural(total, "key page is", "key pages are")
                ),
                None => "The home page is off limits.".to_owned(),
            };
            (
                head,
                match stance {
                    Stance::Allow => "c-err",
                    Stance::Block => "c-ok",
                    Stance::Any => "c-muted",
                },
                why,
            )
        }
        (_, Some(true)) => {
            let head = match group {
                Some(GroupMatch::Named) => "Yes — its own group allows it".to_owned(),
                Some(GroupMatch::Fallback) => format!("Yes — it follows {group_name} group"),
                Some(GroupMatch::Wildcard) => {
                    "Yes — no group names it, so it uses User-agent: *".to_owned()
                }
                _ => "Yes — no group applies to it".to_owned(),
            };
            let mut why = match group {
                Some(GroupMatch::Named) => {
                    "Bots follow the most specific group that names them, and one names this bot."
                        .to_owned()
                }
                Some(GroupMatch::Fallback) => format!(
                    "No group names it, and its operator says it then follows {group_name} rules."
                ),
                Some(GroupMatch::Wildcard) => {
                    "Bots follow the most specific group that names them. With none, the * group applies."
                        .to_owned()
                }
                _ => "Nothing in robots.txt applies to it, so everything is allowed.".to_owned(),
            };
            if a.blocked.is_empty() {
                why.push_str(&format!(
                    " Every key page is allowed ({} of {total}).",
                    total
                ));
            } else {
                why.push_str(&format!(
                    " The home page is allowed, but {} of {total} key pages are off limits: {}.",
                    a.blocked.len(),
                    blocked_paths(SAMPLE_PATHS)
                ));
            }
            (
                head,
                match stance {
                    Stance::Block if bot.honours_robots == Honours::Yes => "c-warn",
                    Stance::Allow if !a.blocked.is_empty() => "c-warn",
                    Stance::Allow => "c-ok",
                    _ => "c-muted",
                },
                why,
            )
        }
    };

    let (excerpt_lines, excerpt_more, excerpt_note) = match (
        report.robots.availability,
        report.robots.file.as_ref(),
    ) {
        (RobotsAvailability::Ok, Some(f)) => {
            let robots = RobotsTxt::parse(f.body.as_bytes());
            let (lines, more) = excerpt(&f.body, &robots, token, rule.map(|r| r.line), "/");
            (
                lines,
                (more > 0).then(|| format!("… {}", plural(more, "more line", "more lines"))),
                None,
            )
        }
        (RobotsAvailability::Ok, None) => (
            Vec::new(),
            None,
            Some("The file's text wasn't stored for this check. It appears after the next crawl."),
        ),
        _ => (Vec::new(), None, None),
    };

    let pages_line = if total == 0 {
        "No key pages in this report.".to_owned()
    } else if a.blocked.is_empty() {
        let mut s = format!("{total} of {total} key pages allowed");
        if a.fetch.problem == Some(true) {
            s.push_str(", but a fetch as this bot was turned away");
        }
        s
    } else {
        format!(
            "{} of {total} key pages allowed; blocked: {}",
            total - a.blocked.len(),
            blocked_paths(SAMPLE_PATHS)
        )
    };

    let compare = report.probes.as_ref().and_then(|p| {
        let u = p
            .urls
            .iter()
            .filter(|u| {
                u.results
                    .iter()
                    .any(|r| r.token.eq_ignore_ascii_case(token) && r.fetch.is_some())
            })
            .min_by_key(|u| u.url)?;
        let r = u
            .results
            .iter()
            .find(|r| r.token.eq_ignore_ascii_case(token))?;
        let f = r.fetch.as_ref()?;
        let b = &u.baseline;
        let signal = |f: &ProbeFetch| match (&f.challenge, &f.final_url) {
            (Some(c), _) => c.signal.clone(),
            (None, Some(to)) => format!("→ {to}"),
            _ => "—".to_owned(),
        };
        let title = |f: &ProbeFetch| f.title.clone().unwrap_or_else(|| "—".to_owned());
        let sz = |f: &ProbeFetch| {
            if f.status.is_some() {
                size(f)
            } else {
                "—".to_owned()
            }
        };
        let row = |label, baseline: String, bot: String| CompareRow {
            label,
            differs: baseline != bot,
            baseline,
            bot,
        };
        Some(Compare {
            page: report
                .important
                .get(usize::from(u.url))
                .map_or("/".to_owned(), |i| path_of(&i.url)),
            rows: vec![
                row("Status", status_text(b), status_text(f)),
                row("Size", sz(b), sz(f)),
                row("Title", title(b), title(f)),
                row("Signal", signal(b), signal(f)),
            ],
        })
    });

    let (honours, honours_class) = honours_text(bot.honours_robots);
    let mut verify: Vec<(String, Option<String>)> = Vec::new();
    if !bot.crawls {
        verify.push((
            "Nothing to verify: it never makes requests".to_owned(),
            None,
        ));
    } else {
        if let Some(u) = &bot.ip_ranges_url {
            verify.push((
                format!("{}'s published IP ranges", bot.operator),
                Some(u.clone()),
            ));
        }
        if !bot.reverse_dns.is_empty() {
            verify.push((format!("reverse DNS: {}", bot.reverse_dns.join(", ")), None));
        }
        if let Some(s) = &bot.signature_agent {
            verify.push((format!("Web Bot Auth: {s}"), None));
        }
        if verify.is_empty() {
            verify.push(("No published IP ranges or reverse DNS".to_owned(), None));
        }
    }
    let issue_href = input
        .open
        .iter()
        .find(|i| {
            serde_json::from_value::<Evidence>(i.evidence.clone())
                .is_ok_and(|e| e.members().contains(&token.to_ascii_lowercase()))
        })
        .map(|i| format!("{}/ai-access#issue-{}", input.base, i.id));

    BotPanel {
        token: token.clone(),
        operator: bot.operator.clone(),
        product: bot.product.clone(),
        purpose: purpose_label(bot.purpose),
        stance: stance_label(stance),
        stance_class: stance_class(stance),
        status: a.row.status,
        status_class: a.row.status_class,
        robots_head,
        robots_class,
        robots_why,
        excerpt: excerpt_lines,
        excerpt_more,
        excerpt_note,
        robots_href: format!("{}/ai-access/robots?bot={}", input.base, encode(token)),
        fetch_head: a.fetch.head.clone(),
        fetch_class: a.fetch.head_class,
        fetch_why: a.fetch.why.clone(),
        compare,
        pages_line,
        ua: if bot.crawls {
            bot.user_agent_contains
                .clone()
                .unwrap_or_else(|| token.clone())
        } else {
            "— (no crawler)".to_owned()
        },
        honours,
        honours_class,
        verify,
        source_host: host_of(&bot.source_url),
        source_url: bot.source_url.clone(),
        reviewed: reviewed(&bot.last_reviewed),
        issue_href,
        policy_href: format!("{}/policy?tab=ai#bot-{}", input.base, token),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_excerpt_marks_the_deciding_line_of_each_group_read() {
        let body = "# top\nUser-agent: *\nDisallow: /a/\n\nUser-agent: GPTBot\nUser-agent: CCBot\n# why\nDisallow: /\n\nSitemap: https://x/s.xml\n";
        let robots = RobotsTxt::parse(body.as_bytes());
        let (lines, more) = excerpt(body, &robots, "GPTBot", Some(8), "/");
        assert_eq!(more, 0);
        assert_eq!(
            lines.iter().map(|l| l.n).collect::<Vec<_>>(),
            vec![5, 6, 7, 8]
        );
        assert!(lines[2].comment);
        assert_eq!(lines[3].mark.as_deref(), Some("decides /"));
        assert!(lines[..3].iter().all(|l| l.mark.is_none()));
        // A bot no group names reads the * group.
        let (lines, _) = excerpt(body, &robots, "OAI-SearchBot", None, "/");
        assert_eq!(lines.iter().map(|l| l.n).collect::<Vec<_>>(), vec![2, 3]);
    }

    #[test]
    fn reviewed_dates_read_as_words() {
        assert_eq!(reviewed("2026-10-11"), "11 Oct 2026");
        assert_eq!(reviewed("soon"), "soon");
    }
}
