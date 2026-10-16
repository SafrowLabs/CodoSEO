//! The AI access screen's tabs and its Overview: the score tiles, one verdict per AI product,
//! your policy against what bots get, and the "Look closer" links to the other tabs.
//!
//! Everything here reads what the latest access report, the registry, the probes and the owner's
//! intent already hold: robots.txt verdicts, synthetic fetch checks and page markup, never
//! traffic. The handlers and the per-tab sections live in [`crate::routes::ai_access`].

use codoseo_geo::content::ContentReport;
use codoseo_geo::eligibility::{Effect, Engine, engines};
use codoseo_geo::llms::LlmsState;
use codoseo_geo::probe::{SkipReason, Verdict};
use codoseo_geo::report::AccessReport;
use codoseo_geo::robots::RobotsAvailability;
use codoseo_geo::score::{GeoScore, Status, Tier};
use codoseo_geo::{Bot, Honours, Intent, Purpose, Stance, registry};

use crate::fmt;
use crate::routes::ai_access::{directive_label, names, plural, stance_label};
use crate::routes::audit_ai::engine_short;

// ---- tabs ------------------------------------------------------------------------------------

/// The AI access screen's tabs, each its own URL under `/s/{site}/ai-access`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Bots,
    Pages,
    Robots,
    Files,
    Visits,
}

impl Tab {
    pub const ALL: [Tab; 6] = [
        Tab::Overview,
        Tab::Bots,
        Tab::Pages,
        Tab::Robots,
        Tab::Files,
        Tab::Visits,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Bots => "Bots",
            Tab::Pages => "Key pages",
            Tab::Robots => "robots.txt",
            Tab::Files => "Agent files & signals",
            Tab::Visits => "Bot visits",
        }
    }

    /// The path under `/s/{site}/ai-access`: empty for the Overview.
    pub fn slug(self) -> &'static str {
        match self {
            Tab::Overview => "",
            Tab::Bots => "bots",
            Tab::Pages => "pages",
            Tab::Robots => "robots",
            Tab::Files => "files",
            Tab::Visits => "visits",
        }
    }

    pub fn href(self, base: &str) -> String {
        match self.slug() {
            "" => format!("{base}/ai-access"),
            s => format!("{base}/ai-access/{s}"),
        }
    }

    /// Used by the template to pick the tab's sections.
    pub fn key(self) -> &'static str {
        match self {
            Tab::Overview => "overview",
            s => s.slug(),
        }
    }
}

/// The checks the "Agent files & signals" tab lists: llms.txt, Content Signals, Markdown for
/// agents and the other declared preferences.
pub const FILE_CHECKS: usize = 4;

pub struct TabLink {
    pub label: &'static str,
    pub href: String,
    /// `27`, `soon`, or empty.
    pub count: String,
    pub current: bool,
}

pub fn tab_links(base: &str, report: Option<&AccessReport>, current: Tab) -> Vec<TabLink> {
    Tab::ALL
        .into_iter()
        .map(|t| TabLink {
            label: t.label(),
            href: t.href(base),
            count: match t {
                Tab::Bots => registry().bots.len().to_string(),
                Tab::Pages => report.map_or(String::new(), |r| r.important.len().to_string()),
                Tab::Files => FILE_CHECKS.to_string(),
                Tab::Visits => "soon".to_owned(),
                Tab::Overview | Tab::Robots => String::new(),
            },
            current: t == current,
        })
        .collect()
}

// ---- what a fetch as one bot got --------------------------------------------------------------

/// What the synthetic fetch checks say about one bot, over every probed page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched {
    /// No fetch check in this report.
    NotRun,
    /// Fetch checks ran but this bot isn't one they send.
    Skipped(Option<SkipReason>),
    /// robots.txt kept it off every probed page, or every result was inconclusive.
    Unjudged,
    /// The same page as CodoSEObot on `same` of `of` pages (none worse).
    Same { same: usize, of: usize },
    /// Kept out: an error status, a dropped connection or a bot-manager challenge.
    Hard {
        worse: usize,
        of: usize,
        what: String,
    },
    /// Let in, but redirected, served a different page or rate limited.
    Soft {
        worse: usize,
        of: usize,
        what: String,
    },
}

impl Fetched {
    pub fn kept_out(&self) -> bool {
        matches!(self, Fetched::Hard { .. })
    }

    /// The fetch gave a verdict (same or worse).
    pub fn judged(&self) -> bool {
        matches!(
            self,
            Fetched::Same { .. } | Fetched::Hard { .. } | Fetched::Soft { .. }
        )
    }
}

fn verdict_words(v: &Verdict) -> String {
    match v {
        Verdict::Challenged { vendor } => format!("challenged ({})", vendor.label()),
        Verdict::Blocked { status: Some(s) } => format!("blocked with HTTP {s}"),
        Verdict::Blocked { status: None } => "dropped".to_owned(),
        Verdict::Diverted { .. } => "redirected elsewhere".to_owned(),
        Verdict::Degraded => "a different, smaller page".to_owned(),
        Verdict::Throttled => "rate limited".to_owned(),
        _ => "treated differently".to_owned(),
    }
}

pub fn fetched(report: &AccessReport, token: &str) -> Fetched {
    let Some(p) = &report.probes else {
        return Fetched::NotRun;
    };
    if !p.bots.iter().any(|b| b.eq_ignore_ascii_case(token)) {
        return Fetched::Skipped(
            p.skipped_bots
                .iter()
                .find(|s| s.token.eq_ignore_ascii_case(token))
                .map(|s| s.reason),
        );
    }
    let verdicts: Vec<&Verdict> = p
        .urls
        .iter()
        .flat_map(|u| &u.results)
        .filter(|r| r.token.eq_ignore_ascii_case(token))
        .map(|r| &r.verdict)
        .collect();
    let compared = verdicts
        .iter()
        .filter(|v| !matches!(v, Verdict::Inconclusive { .. } | Verdict::SkippedRobots))
        .count();
    let worse: Vec<&&Verdict> = verdicts.iter().filter(|v| v.is_positive()).collect();
    if let Some(first) = worse
        .iter()
        .find(|v| matches!(v, Verdict::Blocked { .. } | Verdict::Challenged { .. }))
    {
        let hard = worse
            .iter()
            .filter(|v| matches!(v, Verdict::Blocked { .. } | Verdict::Challenged { .. }))
            .count();
        return Fetched::Hard {
            worse: hard,
            of: compared,
            what: verdict_words(first),
        };
    }
    if let Some(first) = worse.first() {
        return Fetched::Soft {
            worse: worse.len(),
            of: compared,
            what: verdict_words(first),
        };
    }
    let same = verdicts.iter().filter(|v| ***v == Verdict::Same).count();
    if same > 0 {
        Fetched::Same { same, of: compared }
    } else {
        Fetched::Unjudged
    }
}

// ---- the product verdict -------------------------------------------------------------------

/// Can an AI product find and quote the site, from robots.txt, the fetch checks and page markup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// robots.txt allows its search crawler on every key page and a fetch as it got the same
    /// page as CodoSEObot.
    Reachable,
    /// robots.txt allows it, but it wasn't fetched as that bot (no full user agent, a verified
    /// search engine, or no fetch check).
    Allowed,
    /// Partly kept out: some key pages blocked or limited by page markup, or the search crawler
    /// kept out while its user-triggered fetcher still gets in.
    Limited,
    /// Its search crawler may not fetch the home page, or the fetch as it was blocked or
    /// challenged.
    Blocked,
    /// robots.txt wasn't checked.
    Unknown,
}

/// How the verdict sits with the owner's intent for the product's search crawler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Allow: the verdict stands as it is.
    Wanted,
    /// Block, and it is kept out.
    PerPolicy,
    /// Block, but it still gets in.
    PolicySaysBlock,
    /// No preference.
    NoPreference,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductVerdict {
    pub reach: Reach,
    pub fit: Fit,
    /// One line: why.
    pub why: String,
}

impl ProductVerdict {
    /// The label, the text class and the dot class.
    pub fn look(&self) -> (&'static str, &'static str, &'static str) {
        match (self.fit, self.reach) {
            (Fit::PerPolicy, _) => ("Blocked · per your policy", "c-muted", "is-note"),
            (Fit::PolicySaysBlock, _) => ("Allowed · policy says Block", "c-warn", "is-warn"),
            (_, Reach::Unknown) => ("Unknown", "c-muted", "is-note"),
            (Fit::NoPreference, Reach::Blocked) => ("Blocked", "c-muted", "is-note"),
            (_, Reach::Reachable) => ("Reachable", "c-ok", "is-ok"),
            (_, Reach::Allowed) => ("Allowed", "c-ok", "is-ok"),
            (_, Reach::Limited) => ("Limited", "c-warn", "is-warn"),
            (_, Reach::Blocked) => ("Blocked", "c-err", "is-crit"),
        }
    }

    /// It can find and quote the site on every key page (Reachable or Allowed).
    pub fn finds(&self) -> bool {
        matches!(self.reach, Reach::Reachable | Reach::Allowed)
    }

    /// A Limited or Blocked verdict that goes against the intent.
    pub fn problem(&self) -> bool {
        self.fit == Fit::Wanted && matches!(self.reach, Reach::Limited | Reach::Blocked)
    }
}

/// The product's user-triggered fetcher: the operator's `user_fetch` bot, if any.
pub fn user_fetcher(engine: &Engine) -> Option<&'static Bot> {
    let op = &registry().bot(engine.crawler)?.operator;
    registry()
        .bots
        .iter()
        .find(|b| b.purpose == Purpose::UserFetch && &b.operator == op)
}

fn skip_words(r: Option<SkipReason>) -> &'static str {
    match r {
        Some(SkipReason::NoUserAgent) => "its operator publishes no full user agent",
        Some(SkipReason::VerifiedSearchEngine) => "a verified search engine, checked by address",
        Some(SkipReason::NotProbedPurpose) | None => "it isn't one the fetch check sends",
    }
}

pub fn product_verdict(report: &AccessReport, intent: &Intent, engine: &Engine) -> ProductVerdict {
    let crawler = engine.crawler;
    let stance = registry()
        .bot(crawler)
        .map_or(Stance::Any, |b| intent.effective(b));
    if !report.has_verdicts() {
        let (reach, why) = match (report.robots.availability, report.robots.status) {
            (RobotsAvailability::Unavailable, status) => (
                Reach::Blocked,
                format!(
                    "robots.txt {}: crawlers that follow it stay off the whole site.",
                    status.map_or("couldn't be read".to_owned(), |s| format!(
                        "answered HTTP {s}"
                    ))
                ),
            ),
            _ => (
                Reach::Unknown,
                "robots.txt wasn't checked on the latest crawl.".to_owned(),
            ),
        };
        let fit = match (stance, reach) {
            (Stance::Block, Reach::Blocked) => Fit::PerPolicy,
            (Stance::Any, _) => Fit::NoPreference,
            _ => Fit::Wanted,
        };
        return ProductVerdict { reach, fit, why };
    }

    let total = report.important.len();
    let access = report.bot(crawler);
    let home_blocked = access.and_then(|a| a.home_allowed) == Some(false);
    let blocked_n = access.map_or(0, |a| a.blocked.len());
    let fetch = fetched(report, crawler);
    let kept_out = home_blocked || fetch.kept_out();
    let fit = match stance {
        Stance::Allow => Fit::Wanted,
        Stance::Block if kept_out => Fit::PerPolicy,
        Stance::Block => Fit::PolicySaysBlock,
        Stance::Any => Fit::NoPreference,
    };

    let (reach, mut why) = if kept_out {
        let how = if home_blocked {
            match access.and_then(|a| a.home_rule.as_ref()) {
                Some(r) => format!(
                    "robots.txt blocks {crawler} on the home page (line {})",
                    r.line
                ),
                None => format!("robots.txt blocks {crawler} on the home page"),
            }
        } else if let Fetched::Hard { worse, of, what } = &fetch {
            format!("our fetch as {crawler} was {what} on {worse} of {of} pages")
        } else {
            format!("{crawler} is kept out")
        };
        let fetcher = user_fetcher(engine).filter(|u| {
            report.bot(&u.token).and_then(|a| a.home_allowed) == Some(true)
                && !fetched(report, &u.token).kept_out()
        });
        match fetcher {
            Some(u) => (
                Reach::Limited,
                format!(
                    "{}. {} can still open a page someone asks about.",
                    capitalise(&how),
                    u.token
                ),
            ),
            None => (Reach::Blocked, format!("{}.", capitalise(&how))),
        }
    } else if blocked_n > 0 {
        (
            Reach::Limited,
            format!("robots.txt blocks {crawler} on {blocked_n} of {total} key pages."),
        )
    } else if let Fetched::Soft { worse, of, what } = &fetch {
        (
            Reach::Limited,
            format!("Our fetch as {crawler} got {what} on {worse} of {of} pages."),
        )
    } else if let Some((n, html, how)) = answer_limits(report, engine) {
        (
            Reach::Limited,
            format!("Page markup limits it on {n} of {html} key pages ({how})."),
        )
    } else if let Fetched::Same { same, of } = fetch {
        (
            Reach::Reachable,
            format!(
                "Allowed by robots.txt; our fetch as {crawler} got the same page as CodoSEObot on {same} of {of}."
            ),
        )
    } else {
        let not = match &fetch {
            Fetched::NotRun => "no fetch check ran".to_owned(),
            Fetched::Skipped(r) => skip_words(*r).to_owned(),
            _ => "the fetch check was inconclusive".to_owned(),
        };
        (
            Reach::Allowed,
            format!("Allowed by robots.txt. Not fetched as {crawler}: {not}."),
        )
    };
    match fit {
        Fit::PolicySaysBlock => why.push_str(&format!(" Your policy says Block {crawler}.")),
        Fit::NoPreference if reach == Reach::Blocked => {
            why.push_str(" You have no preference, so it isn't reported.")
        }
        _ => {}
    }
    ProductVerdict { reach, fit, why }
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) if f.is_lowercase() && !s.starts_with("robots.txt") => {
            f.to_uppercase().collect::<String>() + c.as_str()
        }
        _ => s.to_owned(),
    }
}

/// Key pages the engine's documented page controls limit or exclude (not robots.txt): how many,
/// of how many HTML pages, and the directives.
fn answer_limits(report: &AccessReport, engine: &Engine) -> Option<(usize, usize, String)> {
    let access = report.engine(engine.id)?;
    let mut n = 0;
    let mut how: Vec<&'static str> = Vec::new();
    for g in &access.groups {
        if g.robots_blocked || g.effect == Effect::Eligible {
            continue;
        }
        n += g.urls.len();
        for c in &g.causes {
            let l = directive_label(c.directive);
            if !how.contains(&l) {
                how.push(l);
            }
        }
    }
    (n > 0).then(|| (n, usize::from(access.html_pages), how.join(", ")))
}

pub struct ProductCard {
    pub name: &'static str,
    pub operator: String,
    /// `OAI-SearchBot · ChatGPT-User`
    pub bots: String,
    pub verdict: &'static str,
    pub class: &'static str,
    /// `is-ok`, `is-warn`, `is-crit`, `is-note`: the dot and the card's edge.
    pub tone: &'static str,
    pub why: String,
}

pub fn product_cards(
    report: &AccessReport,
    intent: &Intent,
) -> (Vec<ProductCard>, Vec<ProductVerdict>) {
    let mut cards = Vec::new();
    let mut verdicts = Vec::new();
    for e in engines() {
        let v = product_verdict(report, intent, e);
        let (verdict, class, tone) = v.look();
        let mut bots = vec![e.crawler.to_owned()];
        if let Some(u) = user_fetcher(e) {
            bots.push(u.token.clone());
        }
        cards.push(ProductCard {
            name: engine_short(e.id),
            operator: registry()
                .bot(e.crawler)
                .map_or_else(|| e.name.to_owned(), |b| b.operator.clone()),
            bots: bots.join(" · "),
            verdict,
            class,
            tone,
            why: v.why.clone(),
        });
        verdicts.push(v);
    }
    (cards, verdicts)
}

// ---- the tiles -------------------------------------------------------------------------------

pub struct Kpi {
    pub label: &'static str,
    pub value: String,
    /// `/ 100`, `/10`, or empty.
    pub of: String,
    pub d1: String,
    pub d1_class: &'static str,
    pub d2: String,
}

/// What the GEO score tile needs from the report's crawl and the one before it.
pub struct GeoInput<'a> {
    pub score: Option<i16>,
    pub breakdown: Option<&'a GeoScore>,
    /// The previous finished crawl's number and GEO score.
    pub previous: Option<(i64, Option<i16>)>,
}

/// Essential and Recommended points scored and on offer, from the score's breakdown.
pub fn tier_points(g: &GeoScore) -> [(Tier, f64, f64); 2] {
    let applies = |t: Tier| {
        g.checks
            .iter()
            .any(|c| c.tier == t && c.status != Status::Skipped)
    };
    let available: f64 = [Tier::Essential, Tier::Recommended]
        .into_iter()
        .filter(|t| applies(*t))
        .map(|t| t.points())
        .sum();
    [Tier::Essential, Tier::Recommended].map(|t| {
        let on_offer = if applies(t) && available > 0.0 {
            t.points() * 100.0 / available
        } else {
            0.0
        };
        let lost: f64 = g
            .checks
            .iter()
            .filter(|c| c.tier == t)
            .map(|c| c.points_lost)
            .sum();
        (t, (on_offer - lost).max(0.0), on_offer)
    })
}

fn geo_tile(geo: &GeoInput<'_>) -> Kpi {
    let Some(score) = geo.score else {
        return Kpi {
            label: "GEO score",
            value: "—".to_owned(),
            of: "/ 100".to_owned(),
            d1: "Comes with the next AI check".to_owned(),
            d1_class: "",
            d2: "Essential 70 · Extras 30".to_owned(),
        };
    };
    let (d1, d1_class) = match geo.previous {
        None => ("First scored crawl".to_owned(), ""),
        Some((_, None)) => ("No earlier GEO score".to_owned(), ""),
        Some((n, Some(p))) => match score - p {
            0 => (format!("No change since crawl #{n}"), ""),
            d if d > 0 => (format!("▲ {d} since crawl #{n}"), "c-ok"),
            d => (format!("▼ {} since crawl #{n}", -d), "c-err"),
        },
    };
    let d2 = match geo.breakdown {
        Some(g) => {
            let [(_, e, eo), (_, r, ro)] = tier_points(g);
            format!(
                "Essential {}/{} · Extras {}/{}",
                e.round(),
                eo.round(),
                r.round(),
                ro.round()
            )
        }
        None => "Essential 70 · Extras 30".to_owned(),
    };
    Kpi {
        label: "GEO score",
        value: score.to_string(),
        of: "/ 100".to_owned(),
        d1,
        d1_class,
        d2,
    }
}

fn first_names(items: &[&str], shown: usize) -> String {
    let mut out: Vec<String> = items.iter().take(shown).map(|s| (*s).to_owned()).collect();
    if items.len() > shown {
        out.push(format!("+{}", items.len() - shown));
    }
    out.join(", ")
}

pub fn kpis(
    report: &AccessReport,
    intent: &Intent,
    cards: &[ProductCard],
    verdicts: &[ProductVerdict],
    geo: &GeoInput<'_>,
) -> Vec<Kpi> {
    let mut out = vec![geo_tile(geo)];
    let unread = |label: &'static str, of: String| Kpi {
        label,
        value: "—".to_owned(),
        of,
        d1: match report.robots.availability {
            RobotsAvailability::Unknown => "robots.txt not checked".to_owned(),
            _ => "robots.txt not readable".to_owned(),
        },
        d1_class: "c-muted",
        d2: String::new(),
    };

    // AI products.
    let n = verdicts.len();
    if report.has_verdicts() {
        let finds: Vec<&str> = cards
            .iter()
            .zip(verdicts)
            .filter(|(_, v)| v.finds())
            .map(|(c, _)| c.name)
            .collect();
        let problems: Vec<String> = cards
            .iter()
            .zip(verdicts)
            .filter(|(_, v)| v.problem())
            .map(|(c, v)| {
                format!(
                    "{} {}",
                    c.name,
                    if v.reach == Reach::Blocked {
                        "blocked"
                    } else {
                        "limited"
                    }
                )
            })
            .collect();
        let chosen = verdicts.iter().filter(|v| v.fit == Fit::PerPolicy).count();
        let (d1, d1_class) = if !problems.is_empty() {
            (
                problems.join(" · "),
                if verdicts
                    .iter()
                    .any(|v| v.problem() && v.reach == Reach::Blocked)
                {
                    "c-err"
                } else {
                    "c-warn"
                },
            )
        } else if chosen > 0 {
            (format!("{chosen} blocked per your policy"), "")
        } else if finds.len() == n {
            ("All reach your key pages".to_owned(), "c-ok")
        } else {
            ("None needs a fix".to_owned(), "")
        };
        out.push(Kpi {
            label: "AI products that can find you",
            value: finds.len().to_string(),
            of: format!("/{n}"),
            d1,
            d1_class,
            d2: if finds.is_empty() {
                "From robots.txt and fetch checks".to_owned()
            } else {
                format!("{} OK", first_names(&finds, 3))
            },
        });
    } else {
        out.push(unread("AI products that can find you", format!("/{n}")));
    }

    // Search and user-triggered bots.
    let reachers: Vec<&Bot> = registry()
        .bots
        .iter()
        .filter(|b| b.crawls && matches!(b.purpose, Purpose::Search | Purpose::UserFetch))
        .collect();
    if report.has_verdicts() {
        let mut by_robots = 0;
        let mut by_fetch = 0;
        let mut unwanted = false;
        let mut fetched_n = 0;
        for b in &reachers {
            let home = report.bot(&b.token).and_then(|a| a.home_allowed);
            let f = fetched(report, &b.token);
            if f.judged() {
                fetched_n += 1;
            }
            let out = if home == Some(false) {
                by_robots += 1;
                true
            } else if f.kept_out() {
                by_fetch += 1;
                true
            } else {
                false
            };
            if out && intent.effective(b) == Stance::Allow {
                unwanted = true;
            }
        }
        let reach = reachers.len() - by_robots - by_fetch;
        let mut parts = Vec::new();
        if by_robots > 0 {
            parts.push(format!("{by_robots} by robots.txt"));
        }
        if by_fetch > 0 {
            parts.push(format!("{by_fetch} by firewall"));
        }
        out.push(Kpi {
            label: "Search & user bots reachable",
            value: reach.to_string(),
            of: format!("/{}", reachers.len()),
            d1: if parts.is_empty() {
                "robots.txt and fetch checks agree".to_owned()
            } else {
                format!("Kept out: {}", parts.join(" · "))
            },
            d1_class: if unwanted { "c-err" } else { "" },
            d2: match report.probes {
                None => "No fetch check in this report".to_owned(),
                Some(_) => format!("{fetched_n} fetched as the bot"),
            },
        });
    } else {
        out.push(unread(
            "Search & user bots reachable",
            format!("/{}", reachers.len()),
        ));
    }

    // Training.
    let training: Vec<&Bot> = registry()
        .bots
        .iter()
        .filter(|b| b.purpose == Purpose::Training)
        .collect();
    if report.has_verdicts() {
        let blocked = |b: &Bot| report.bot(&b.token).and_then(|a| a.home_allowed) == Some(false);
        let n_blocked = training.iter().filter(|b| blocked(b)).count();
        let wanted: Vec<&&Bot> = training
            .iter()
            .filter(|b| intent.effective(b) == Stance::Block)
            .collect();
        let missing = wanted.iter().filter(|b| !blocked(b)).count();
        let (d1, d1_class) = if wanted.is_empty() {
            ("Policy: no preference".to_owned(), "")
        } else if missing == 0 {
            ("Matches your policy".to_owned(), "c-ok")
        } else {
            (
                format!("Policy says Block {}: {missing} get in", wanted.len()),
                "c-warn",
            )
        };
        out.push(Kpi {
            label: "Training bots blocked",
            value: n_blocked.to_string(),
            of: format!("/{}", training.len()),
            d1,
            d1_class,
            d2: "No effect on AI search".to_owned(),
        });
    } else {
        out.push(unread(
            "Training bots blocked",
            format!("/{}", training.len()),
        ));
    }

    out.push(readable_tile(
        report.content.as_ref(),
        report.html_important(),
    ));
    out
}

fn readable_tile(content: Option<&ContentReport>, html: usize) -> Kpi {
    let Some(c) = content.filter(|c| !c.pages.is_empty()) else {
        return Kpi {
            label: "Readable without JS",
            value: "—".to_owned(),
            of: format!("/{html}"),
            d1: if html == 0 {
                "No HTML key pages".to_owned()
            } else {
                "Checked after the next crawl".to_owned()
            },
            d1_class: "c-muted",
            d2: String::new(),
        };
    };
    let shells = c.pages.iter().filter(|p| p.verdict.is_shell()).count();
    let mut words: Vec<u32> = c.pages.iter().map(|p| p.words).collect();
    words.sort_unstable();
    let median = words[words.len() / 2];
    Kpi {
        label: "Readable without JS",
        value: (c.pages.len() - shells).to_string(),
        of: format!("/{}", c.pages.len()),
        d1: match shells {
            0 => "Every key page has its text".to_owned(),
            1 => "1 empty app shell".to_owned(),
            n => format!("{n} empty app shells"),
        },
        d1_class: if shells > 0 { "c-warn" } else { "" },
        d2: format!("median {} words", fmt::thousands(median)),
    }
}

// ---- your policy against what bots get -------------------------------------------------------

pub struct PolicyRow {
    pub label: &'static str,
    pub stance: String,
    pub stance_class: &'static str,
    pub actual: String,
    /// `is-ok`, `is-warn`, `is-crit`, `is-note`.
    pub tone: &'static str,
}

fn policy_label(p: Purpose) -> &'static str {
    match p {
        Purpose::Search => "Search crawlers",
        Purpose::UserFetch => "User fetchers",
        Purpose::Agent => "Agents",
        Purpose::Training => "Training crawlers",
        Purpose::Ads => "Ads crawlers",
    }
}

fn stance_tag(s: Stance) -> &'static str {
    match s {
        Stance::Allow => "t-ok",
        Stance::Block => "t-err",
        Stance::Any => "t-muted",
    }
}

pub fn policy_rows(report: &AccessReport, intent: &Intent) -> Vec<PolicyRow> {
    let mut out: Vec<PolicyRow> = [
        Purpose::Search,
        Purpose::UserFetch,
        Purpose::Agent,
        Purpose::Training,
        Purpose::Ads,
    ]
    .into_iter()
    .map(|p| {
        let bots: Vec<&Bot> = registry().bots.iter().filter(|b| b.purpose == p).collect();
        let stance = intent.purpose_stance(p);
        let overrides = bots
            .iter()
            .filter(|b| intent.bot_override(&b.token).is_some())
            .count();
        let stance_text = if overrides > 0 {
            format!("{} · {overrides} set per bot", stance_label(stance))
        } else {
            stance_label(stance).to_owned()
        };
        if !report.has_verdicts() {
            return PolicyRow {
                label: policy_label(p),
                stance: stance_text,
                stance_class: stance_tag(stance),
                actual: "robots.txt not readable, so nothing to compare".to_owned(),
                tone: "is-note",
            };
        }
        let mut by_robots: Vec<String> = Vec::new();
        let mut by_fetch: Vec<String> = Vec::new();
        let mut gets_in_blocked: Vec<String> = Vec::new();
        let mut crit = false;
        let mut warn = false;
        for b in &bots {
            let access = report.bot(&b.token);
            let home = access.and_then(|a| a.home_allowed);
            let partly = access.is_some_and(|a| !a.blocked.is_empty());
            let fetch_out = b.crawls && fetched(report, &b.token).kept_out();
            let kept_out = home == Some(false) || fetch_out;
            if home == Some(false) {
                by_robots.push(b.token.clone());
            } else if fetch_out {
                by_fetch.push(b.token.clone());
            }
            match intent.effective(b) {
                Stance::Allow if kept_out && b.honours_robots == Honours::Yes => crit = true,
                Stance::Allow if kept_out || partly => warn = true,
                Stance::Block if !kept_out => {
                    gets_in_blocked.push(b.token.clone());
                    if b.honours_robots == Honours::Yes {
                        warn = true;
                    }
                }
                _ => {}
            }
        }
        let m = bots.len();
        let out_n = by_robots.len() + by_fetch.len();
        let mut actual = if stance == Stance::Block {
            format!("{out_n} of {m} blocked")
        } else {
            format!("{} of {m} can reach you", m - out_n)
        };
        if !gets_in_blocked.is_empty() {
            actual.push_str(&format!(" · {} get in", names(&gets_in_blocked)));
        } else if stance != Stance::Block {
            let mut why = Vec::new();
            if !by_robots.is_empty() {
                why.push(format!("{} blocked by robots.txt", names(&by_robots)));
            }
            if !by_fetch.is_empty() {
                why.push(format!("{} by firewall", names(&by_fetch)));
            }
            if !why.is_empty() {
                actual.push_str(&format!(" · {}", why.join(", ")));
            }
        }
        PolicyRow {
            label: policy_label(p),
            stance: stance_text,
            stance_class: stance_tag(stance),
            actual,
            tone: if crit {
                "is-crit"
            } else if warn {
                "is-warn"
            } else if stance == Stance::Any && overrides == 0 {
                "is-note"
            } else {
                "is-ok"
            },
        }
    })
    .collect();

    // Page directives that limit answers.
    let accepted = &intent.accepted_directives;
    let mut limited: std::collections::BTreeSet<u16> = Default::default();
    let mut unaccepted = false;
    for e in &report.engines {
        for g in &e.groups {
            if g.robots_blocked || g.effect == Effect::Eligible {
                continue;
            }
            limited.extend(g.urls.iter().copied());
            if g.causes.iter().any(|c| !accepted.contains(&c.directive)) {
                unaccepted = true;
            }
        }
    }
    let accepted_names: Vec<String> = accepted
        .iter()
        .map(|d| directive_label(*d).to_owned())
        .collect();
    out.push(PolicyRow {
        label: "Answer limits (nosnippet, max-snippet)",
        stance: if accepted.is_empty() {
            "None accepted".to_owned()
        } else {
            format!("{} accepted", accepted.len())
        },
        stance_class: "t-muted",
        actual: match (limited.len(), accepted_names.is_empty()) {
            (0, _) => "No key page is limited in AI answers".to_owned(),
            (n, true) => format!(
                "{} limited in some AI answers",
                plural(n, "key page", "key pages")
            ),
            (n, false) => format!(
                "{} limited in some AI answers · {} as you set",
                plural(n, "key page", "key pages"),
                names(&accepted_names)
            ),
        },
        tone: if unaccepted { "is-warn" } else { "is-ok" },
    });
    out
}

// ---- look closer -----------------------------------------------------------------------------

pub struct LookCard {
    pub label: &'static str,
    pub href: String,
    pub count: String,
    pub status: String,
    pub status_class: &'static str,
    pub what: &'static str,
}

pub struct LookInput {
    /// Bots whose status conflicts with the intent, and bots the fetch check treats worse.
    pub bot_attention: usize,
    pub shells: Option<usize>,
    /// Files and signals recommended and missing.
    pub files_missing: usize,
}

pub fn look_cards(base: &str, report: &AccessReport, input: &LookInput) -> Vec<LookCard> {
    let (robots, robots_class) = robots_status(report);
    vec![
        LookCard {
            label: Tab::Bots.label(),
            href: Tab::Bots.href(base),
            count: registry().bots.len().to_string(),
            status: if !report.has_verdicts() {
                "robots.txt not readable".to_owned()
            } else if input.bot_attention == 0 {
                "No conflicts".to_owned()
            } else {
                format!("{} need attention", input.bot_attention)
            },
            status_class: if !report.has_verdicts() {
                "c-muted"
            } else if input.bot_attention == 0 {
                "c-ok"
            } else {
                "c-err"
            },
            what: "Every AI bot: your policy, its robots.txt rule and what our fetch got.",
        },
        LookCard {
            label: Tab::Pages.label(),
            href: Tab::Pages.href(base),
            count: report.important.len().to_string(),
            status: match input.shells {
                None => "Content not checked yet".to_owned(),
                Some(0) => "All readable without JavaScript".to_owned(),
                Some(n) => format!("{n} empty without JavaScript"),
            },
            status_class: match input.shells {
                None => "c-muted",
                Some(0) => "c-ok",
                Some(_) => "c-warn",
            },
            what: "Words in the raw HTML, each page's status and what each AI engine may quote.",
        },
        LookCard {
            label: Tab::Robots.label(),
            href: Tab::Robots.href(base),
            count: String::new(),
            status: robots,
            status_class: robots_class,
            what: "What robots.txt answered and which group each bot follows.",
        },
        LookCard {
            label: Tab::Files.label(),
            href: Tab::Files.href(base),
            count: FILE_CHECKS.to_string(),
            status: match input.files_missing {
                0 => "Nothing recommended is missing".to_owned(),
                n => format!("{n} recommended missing"),
            },
            status_class: if input.files_missing == 0 {
                "c-ok"
            } else {
                "c-muted"
            },
            what: "llms.txt, Content Signals, Markdown for agents and declared preferences.",
        },
        LookCard {
            label: Tab::Visits.label(),
            href: Tab::Visits.href(base),
            count: "soon".to_owned(),
            status: "Needs server logs".to_owned(),
            status_class: "c-ghost",
            what: "Real visits per bot, verified or claimed, and rule breakers.",
        },
    ]
}

/// robots.txt in a few words, and its text class.
pub fn robots_status(report: &AccessReport) -> (String, &'static str) {
    match (report.robots.availability, report.robots.status) {
        (RobotsAvailability::Ok, Some(s)) => (format!("Fetched · HTTP {s}"), "c-ok"),
        (RobotsAvailability::Ok, None) => ("Fetched".to_owned(), "c-ok"),
        (RobotsAvailability::Missing, Some(s)) => {
            (format!("None (HTTP {s}) · every bot may crawl"), "c-muted")
        }
        (RobotsAvailability::Missing, None) => ("None · every bot may crawl".to_owned(), "c-muted"),
        (RobotsAvailability::Unavailable, Some(s)) => (format!("Failing · HTTP {s}"), "c-err"),
        (RobotsAvailability::Unavailable, None) => ("Couldn't be read".to_owned(), "c-err"),
        (RobotsAvailability::Unknown, _) => ("Not checked".to_owned(), "c-muted"),
    }
}

/// Files and signals the Files tab recommends that the report finds missing: llms.txt, a
/// Content-Signal line, Markdown for agents.
pub fn files_missing(report: &AccessReport) -> usize {
    let llms = report
        .llms_txt
        .as_ref()
        .is_some_and(|l| matches!(l.state, LlmsState::Absent | LlmsState::SoftNotFound));
    let signals = report.has_verdicts() && report.declared.content_signals.is_empty();
    let markdown = report.markdown.as_ref().is_some_and(|m| {
        !m.served()
            && m.state != codoseo_geo::markdown::MarkdownState::Unreachable
            && report.declared.home_read
    });
    [llms, signals, markdown].into_iter().filter(|b| *b).count()
}

// ---- the robots.txt tab ----------------------------------------------------------------------

/// The site's `/robots.txt` URL.
pub fn robots_url(start_url: &str) -> String {
    match url::Url::parse(start_url) {
        Ok(mut u) => {
            u.set_path("/robots.txt");
            u.set_query(None);
            u.set_fragment(None);
            u.to_string()
        }
        Err(_) => format!("{}/robots.txt", start_url.trim_end_matches('/')),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codoseo_geo::eligibility::EngineId;
    use codoseo_geo::probe::{
        InconclusiveReason, ProbeFetch, ProbeReport, ProbeResult, ProbedUrl, SkippedBot, Vendor,
    };
    use codoseo_geo::report::{
        BlockedUrl, BotAccess, EngineAccess, EngineGroup, ImportantUrl, Reason, RobotsInfo,
        SiteCounts,
    };
    use codoseo_geo::robots::{GroupMatch, MatchedRule};

    /// Three key pages; `blocked` lists, per token, the page indexes robots.txt blocks.
    fn report(blocked: &[(&str, &[u16])]) -> AccessReport {
        let important = ["/", "/pricing", "/about"]
            .iter()
            .enumerate()
            .map(|(i, p)| ImportantUrl {
                url: format!("https://example.com{p}"),
                reason: if i == 0 {
                    Reason::Home
                } else {
                    Reason::MostLinked
                },
                status: Some(200),
                html_ok: true,
            })
            .collect();
        AccessReport {
            version: 1,
            registry_version: 2,
            robots: RobotsInfo {
                status: Some(200),
                availability: RobotsAvailability::Ok,
                file: None,
            },
            important,
            bots: registry()
                .bots
                .iter()
                .map(|b| {
                    let urls: &[u16] = blocked
                        .iter()
                        .find(|(t, _)| *t == b.token)
                        .map_or(&[], |(_, u)| u);
                    BotAccess {
                        token: b.token.clone(),
                        home_allowed: Some(!urls.contains(&0)),
                        group: if urls.is_empty() {
                            GroupMatch::Wildcard
                        } else {
                            GroupMatch::Named
                        },
                        home_rule: urls.contains(&0).then(|| MatchedRule {
                            line: 6,
                            allow: false,
                            pattern: "/".to_owned(),
                        }),
                        rules: Vec::new(),
                        blocked: urls
                            .iter()
                            .map(|u| BlockedUrl {
                                url: *u,
                                rule: None,
                            })
                            .collect(),
                    }
                })
                .collect(),
            rules: Vec::new(),
            engines: Vec::new(),
            declared: Default::default(),
            probes: None,
            content: None,
            llms_txt: None,
            llms_txt_skipped: None,
            markdown: None,
        }
    }

    fn page_fetch() -> ProbeFetch {
        ProbeFetch {
            status: Some(200),
            error: None,
            final_url: None,
            redirects: 0,
            bytes: 2048,
            truncated: false,
            html: true,
            content_type: Some("text/html".to_owned()),
            title: Some("Example".to_owned()),
            challenge: None,
            retried: false,
        }
    }

    /// A fetch check over the three pages with `verdicts` per token (one per page).
    fn with_probes(mut r: AccessReport, verdicts: &[(&str, Vec<Verdict>)]) -> AccessReport {
        let urls = (0..3u16)
            .map(|i| ProbedUrl {
                url: i,
                baseline: page_fetch(),
                results: verdicts
                    .iter()
                    .map(|(t, v)| ProbeResult {
                        token: (*t).to_owned(),
                        fetch: Some(page_fetch()),
                        verdict: v[usize::from(i)].clone(),
                    })
                    .collect(),
            })
            .collect();
        r.probes = Some(ProbeReport {
            version: 1,
            synthetic: true,
            from: "hello@codoseo.com".to_owned(),
            bots: verdicts.iter().map(|(t, _)| (*t).to_owned()).collect(),
            skipped_bots: vec![SkippedBot {
                token: "Claude-SearchBot".to_owned(),
                reason: SkipReason::NoUserAgent,
            }],
            urls,
            stop: None,
            requests: 9,
        });
        r
    }

    fn engine(id: EngineId) -> &'static Engine {
        engines().iter().find(|e| e.id == id).unwrap()
    }

    fn same() -> Vec<Verdict> {
        vec![Verdict::Same, Verdict::Same, Verdict::Same]
    }

    fn challenged() -> Vec<Verdict> {
        vec![
            Verdict::Challenged {
                vendor: Vendor::Cloudflare,
            };
            3
        ]
    }

    #[test]
    fn allowed_and_fetched_the_same_is_reachable() {
        let r = with_probes(report(&[]), &[("PerplexityBot", same())]);
        let v = product_verdict(&r, &Intent::default(), engine(EngineId::Perplexity));
        assert_eq!(v.reach, Reach::Reachable);
        assert_eq!(v.fit, Fit::Wanted);
        assert_eq!(v.look().0, "Reachable");
        assert!(
            v.why.contains("same page as CodoSEObot on 3 of 3"),
            "{}",
            v.why
        );
        assert!(v.finds() && !v.problem());
    }

    #[test]
    fn allowed_but_never_fetched_as_the_bot_is_allowed() {
        // Claude-SearchBot has no published user agent; Googlebot is never probed; and a report
        // without a fetch check can't say more than robots.txt.
        let r = with_probes(report(&[]), &[("PerplexityBot", same())]);
        let v = product_verdict(&r, &Intent::default(), engine(EngineId::Claude));
        assert_eq!(v.reach, Reach::Allowed);
        assert!(v.why.contains("no full user agent"), "{}", v.why);
        let v = product_verdict(&report(&[]), &Intent::default(), engine(EngineId::Google));
        assert_eq!(v.reach, Reach::Allowed);
        assert!(v.why.contains("no fetch check ran"), "{}", v.why);
        // Inconclusive everywhere: still only allowed.
        let r = with_probes(
            report(&[]),
            &[(
                "PerplexityBot",
                vec![
                    Verdict::Inconclusive {
                        reason: InconclusiveReason::BaselineFailed,
                    };
                    3
                ],
            )],
        );
        let v = product_verdict(&r, &Intent::default(), engine(EngineId::Perplexity));
        assert_eq!(v.reach, Reach::Allowed);
        assert!(v.finds());
    }

    #[test]
    fn a_home_page_block_is_blocked_unless_the_user_fetcher_still_gets_in() {
        // DuckDuckGo has no user-triggered fetcher: blocked outright.
        let r = report(&[("DuckAssistBot", &[0, 1, 2])]);
        let v = product_verdict(&r, &Intent::default(), engine(EngineId::DuckDuckGo));
        assert_eq!(v.reach, Reach::Blocked);
        assert_eq!(v.look(), ("Blocked", "c-err", "is-crit"));
        assert!(v.why.contains("line 6"), "{}", v.why);
        assert!(v.problem() && !v.finds());

        // Perplexity-User is allowed: limited.
        let r = report(&[("PerplexityBot", &[0, 1, 2])]);
        let v = product_verdict(&r, &Intent::default(), engine(EngineId::Perplexity));
        assert_eq!(v.reach, Reach::Limited);
        assert!(
            v.why.contains("Perplexity-User can still open"),
            "{}",
            v.why
        );

        // Both kept out: blocked.
        let r = report(&[("PerplexityBot", &[0]), ("Perplexity-User", &[0])]);
        let v = product_verdict(&r, &Intent::default(), engine(EngineId::Perplexity));
        assert_eq!(v.reach, Reach::Blocked);
    }

    #[test]
    fn a_firewall_challenge_is_blocked_and_a_different_page_is_limited() {
        let r = with_probes(
            report(&[("ChatGPT-User", &[0])]),
            &[("OAI-SearchBot", challenged())],
        );
        let v = product_verdict(&r, &Intent::default(), engine(EngineId::ChatGpt));
        assert_eq!(v.reach, Reach::Blocked);
        assert!(
            v.why.contains("challenged (Cloudflare) on 3 of 3"),
            "{}",
            v.why
        );

        // The user fetcher still gets in: limited.
        let r = with_probes(
            report(&[]),
            &[("OAI-SearchBot", challenged()), ("ChatGPT-User", same())],
        );
        let v = product_verdict(&r, &Intent::default(), engine(EngineId::ChatGpt));
        assert_eq!(v.reach, Reach::Limited);

        let r = with_probes(
            report(&[]),
            &[(
                "MistralAI-Index",
                vec![Verdict::Same, Verdict::Degraded, Verdict::Same],
            )],
        );
        let v = product_verdict(&r, &Intent::default(), engine(EngineId::Mistral));
        assert_eq!(v.reach, Reach::Limited);
        assert!(v.why.contains("on 1 of 3"), "{}", v.why);
    }

    #[test]
    fn some_key_pages_blocked_or_limited_by_markup_is_limited() {
        let r = report(&[("Googlebot", &[2])]);
        let v = product_verdict(&r, &Intent::default(), engine(EngineId::Google));
        assert_eq!(v.reach, Reach::Limited);
        assert!(v.why.contains("on 1 of 3 key pages"), "{}", v.why);

        let mut r = report(&[]);
        r.engines.push(EngineAccess {
            engine: EngineId::Google,
            html_pages: 3,
            groups: vec![EngineGroup {
                effect: Effect::Limited,
                causes: vec![codoseo_geo::eligibility::Cause {
                    directive: codoseo_geo::eligibility::DirectiveSlug::MaxSnippet,
                    detail: "max-snippet:50".to_owned(),
                    source: codoseo_geo::eligibility::CauseSource::Meta {
                        name: "robots".to_owned(),
                    },
                    effect: Effect::Limited,
                }],
                robots_blocked: false,
                urls: vec![1, 2],
            }],
            site: SiteCounts::default(),
        });
        let v = product_verdict(&r, &Intent::default(), engine(EngineId::Google));
        assert_eq!(v.reach, Reach::Limited);
        assert!(
            v.why.contains("2 of 3 key pages (max-snippet)"),
            "{}",
            v.why
        );
    }

    #[test]
    fn a_block_the_policy_asks_for_is_neutral_and_one_that_lets_it_in_warns() {
        let mut intent = Intent::default();
        intent.purposes.insert(Purpose::Search, Stance::Block);

        let r = report(&[("DuckAssistBot", &[0, 1, 2])]);
        let v = product_verdict(&r, &intent, engine(EngineId::DuckDuckGo));
        assert_eq!(v.fit, Fit::PerPolicy);
        assert_eq!(
            v.look(),
            ("Blocked · per your policy", "c-muted", "is-note")
        );
        assert!(!v.problem());

        // Kept out by the firewall counts as blocked too.
        let r = with_probes(report(&[]), &[("PerplexityBot", challenged())]);
        let v = product_verdict(&r, &intent, engine(EngineId::Perplexity));
        assert_eq!(v.fit, Fit::PerPolicy);

        // Not kept out: the policy is not met.
        let v = product_verdict(&report(&[]), &intent, engine(EngineId::Claude));
        assert_eq!(v.fit, Fit::PolicySaysBlock);
        assert_eq!(v.look().0, "Allowed · policy says Block");
        assert!(
            v.why.ends_with("Your policy says Block Claude-SearchBot."),
            "{}",
            v.why
        );
    }

    #[test]
    fn a_per_bot_override_decides_the_fit() {
        // Search is allowed, but this one bot is set to Block.
        let mut intent = Intent::default();
        intent
            .bots
            .insert("perplexitybot".to_owned(), Stance::Block);
        let r = report(&[("PerplexityBot", &[0, 1, 2])]);
        let v = product_verdict(&r, &intent, engine(EngineId::Perplexity));
        assert_eq!(v.fit, Fit::PerPolicy);
        assert_eq!(v.look().0, "Blocked · per your policy");
        // Another product is still judged against Allow.
        let r = report(&[("DuckAssistBot", &[0, 1, 2])]);
        let v = product_verdict(&r, &intent, engine(EngineId::DuckDuckGo));
        assert_eq!(v.fit, Fit::Wanted);
        assert_eq!(v.look().0, "Blocked");

        // No preference: blocked, but muted and not counted as a problem.
        intent.bots.insert("DuckAssistBot".to_owned(), Stance::Any);
        let v = product_verdict(&r, &intent, engine(EngineId::DuckDuckGo));
        assert_eq!(v.fit, Fit::NoPreference);
        assert_eq!(v.look(), ("Blocked", "c-muted", "is-note"));
        assert!(!v.problem());
    }

    #[test]
    fn without_robots_verdicts_the_products_are_blocked_or_unknown() {
        let mut r = report(&[]);
        r.bots.clear();
        r.robots = RobotsInfo {
            status: Some(503),
            availability: RobotsAvailability::Unavailable,
            file: None,
        };
        let v = product_verdict(&r, &Intent::default(), engine(EngineId::Google));
        assert_eq!(v.reach, Reach::Blocked);
        assert!(v.why.contains("HTTP 503"), "{}", v.why);
        r.robots.availability = RobotsAvailability::Unknown;
        let v = product_verdict(&r, &Intent::default(), engine(EngineId::Google));
        assert_eq!(v.reach, Reach::Unknown);
        assert_eq!(v.look().0, "Unknown");
    }

    #[test]
    fn every_engine_gets_a_card_with_its_bots() {
        let (cards, verdicts) = product_cards(&report(&[]), &Intent::default());
        assert_eq!(cards.len(), engines().len());
        assert_eq!(verdicts.len(), cards.len());
        let chatgpt = cards.iter().find(|c| c.name == "ChatGPT").unwrap();
        assert_eq!(chatgpt.bots, "OAI-SearchBot · ChatGPT-User");
        assert_eq!(chatgpt.operator, "OpenAI");
        let google = cards.iter().find(|c| c.operator == "Google").unwrap();
        assert_eq!(google.bots, "Googlebot");
    }

    #[test]
    fn the_tiers_split_seventy_and_thirty_unless_one_is_skipped() {
        use codoseo_geo::score::{GeoCheck, Outcome, geo_score};
        let g = geo_score(&[
            (GeoCheck::BotsBlocked, Outcome::Pass),
            (GeoCheck::LlmsTxt, Outcome::Pass),
        ])
        .unwrap();
        let [(_, e, eo), (_, r, ro)] = tier_points(&g);
        assert_eq!((e, eo, r, ro), (70.0, 70.0, 30.0, 30.0));
        let g = geo_score(&[(GeoCheck::BotsBlocked, Outcome::Pass)]).unwrap();
        let [(_, _, eo), (_, _, ro)] = tier_points(&g);
        assert_eq!((eo, ro), (100.0, 0.0));
    }

    #[test]
    fn each_tab_has_its_own_address() {
        let hrefs: Vec<String> = Tab::ALL.iter().map(|t| t.href("/s/x")).collect();
        assert_eq!(
            hrefs,
            [
                "/s/x/ai-access",
                "/s/x/ai-access/bots",
                "/s/x/ai-access/pages",
                "/s/x/ai-access/robots",
                "/s/x/ai-access/files",
                "/s/x/ai-access/visits"
            ]
        );
        let links = tab_links("/s/x", Some(&report(&[])), Tab::Bots);
        assert!(links[1].current && !links[0].current);
        assert_eq!(links[1].count, registry().bots.len().to_string());
        assert_eq!(links[2].count, "3");
        assert_eq!(links[5].count, "soon");
    }
}
