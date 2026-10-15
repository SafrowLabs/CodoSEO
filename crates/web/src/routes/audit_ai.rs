//! The site audit's "AI bots" table: one row per AI product (each answer engine of
//! [`codoseo_geo::eligibility`]), the bot whose robots.txt group governs it, whether it can
//! reach the important pages, and which pages it can't, with the reason on tap or focus. A last
//! row covers the training crawlers, which feed no answer and so never count as "can reach".
//!
//! Everything is read from the latest access report, the registry and the owner's intent:
//! robots.txt verdicts and the synthetic fetch, not traffic.

use codoseo_geo::eligibility::{EngineId, engines};
use codoseo_geo::probe::{ProbeReport, Verdict};
use codoseo_geo::report::AccessReport;
use codoseo_geo::{Intent, Purpose, Stance, registry};

use crate::routes::ai_access::path_of;

/// Blocked paths a row names before "+N".
const PATHS_SHOWN: usize = 3;

pub struct BotsTable {
    pub rows: Vec<ProductRow>,
    /// Products that reach every important page, of those not blocked by choice.
    pub reach: usize,
    pub of: usize,
    pub blocked: usize,
    pub partly: usize,
    /// robots.txt gave no verdicts: every row is Unknown.
    pub unknown: bool,
}

impl BotsTable {
    /// One status per counted product, for the dots on the score card.
    pub fn dots(&self) -> Vec<&'static str> {
        self.rows
            .iter()
            .filter(|r| r.counted)
            .map(|r| r.status.dot())
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProductStatus {
    Allowed,
    Blocked,
    Partly,
    BlockedByYou,
    Unknown,
}

impl ProductStatus {
    pub fn label(self) -> &'static str {
        match self {
            ProductStatus::Allowed => "Allowed",
            ProductStatus::Blocked => "Blocked",
            ProductStatus::Partly => "Partly",
            ProductStatus::BlockedByYou => "Blocked by you",
            ProductStatus::Unknown => "Unknown",
        }
    }

    /// The badge's tone class.
    pub fn class(self) -> &'static str {
        match self {
            ProductStatus::Allowed => "t-ok",
            ProductStatus::Blocked => "t-err",
            ProductStatus::Partly => "t-warn",
            ProductStatus::BlockedByYou | ProductStatus::Unknown => "t-muted",
        }
    }

    pub fn dot(self) -> &'static str {
        match self {
            ProductStatus::Allowed => "ok",
            ProductStatus::Blocked => "err",
            ProductStatus::Partly => "warn",
            ProductStatus::BlockedByYou | ProductStatus::Unknown => "na",
        }
    }
}

pub struct ProductRow {
    /// For the element ids.
    pub key: String,
    pub name: &'static str,
    /// The operator, for the logo tile.
    pub operator: String,
    /// `PerplexityBot`, or several tokens for the training row.
    pub bots: String,
    pub status: ProductStatus,
    /// `Blocked: /, /pricing, /about +2 · 447 allowed`
    pub pages: String,
    /// Why, in a sentence, for the popover.
    pub why: String,
    /// Part of "N of M can reach your pages".
    pub counted: bool,
}

pub fn engine_short(id: EngineId) -> &'static str {
    match id {
        EngineId::Google => "Google & AI Overviews",
        EngineId::Bing => "Copilot & Bing",
        EngineId::Apple => "Apple Intelligence",
        EngineId::Amazon => "Alexa & Rufus",
        EngineId::ChatGpt => "ChatGPT",
        EngineId::Claude => "Claude",
        EngineId::Perplexity => "Perplexity",
        EngineId::MetaAi => "Meta AI",
        EngineId::DuckDuckGo => "DuckDuckGo",
        EngineId::Mistral => "Mistral Le Chat",
    }
}

/// The verdict a bot's fetch shows, picked as the AI access screen's fetch check picks it:
/// treated worse anywhere, else the same page, else whatever there is.
fn shown_verdict<'a>(probes: &'a ProbeReport, token: &str) -> Option<&'a Verdict> {
    let verdicts: Vec<&Verdict> = probes
        .urls
        .iter()
        .flat_map(|u| &u.results)
        .filter(|r| r.token == token)
        .map(|r| &r.verdict)
        .collect();
    verdicts
        .iter()
        .find(|v| v.is_positive())
        .or_else(|| verdicts.iter().find(|v| **v == &Verdict::Same))
        .or(verdicts.first())
        .copied()
}

/// Probed URLs where the bot was treated worse, of those probed for it.
fn worse_count(probes: &ProbeReport, token: &str) -> (usize, usize) {
    let mut worse = 0;
    let mut probed = 0;
    for u in &probes.urls {
        if let Some(r) = u.results.iter().find(|r| r.token == token) {
            probed += 1;
            if r.verdict.is_positive() {
                worse += 1;
            }
        }
    }
    (worse, probed)
}

fn fetch_problem(v: &Verdict) -> String {
    match v {
        Verdict::Challenged { vendor } => format!("{} challenge", vendor.label()),
        Verdict::Blocked { status: Some(s) } => format!("Firewall {s}"),
        Verdict::Blocked { status: None } => "Connection dropped".to_owned(),
        Verdict::Diverted { .. } => "Redirected away".to_owned(),
        Verdict::Degraded => "Different page".to_owned(),
        _ => "Rate limited".to_owned(),
    }
}

fn blocked_line(report: &AccessReport, token: &str, total: usize) -> String {
    let access = report.bot(token);
    let blocked: Vec<String> = access
        .map(|a| {
            a.blocked
                .iter()
                .filter_map(|b| report.important.get(b.url as usize))
                .map(|u| path_of(&u.url))
                .collect()
        })
        .unwrap_or_default();
    let mut line = format!(
        "Blocked: {}",
        blocked
            .iter()
            .take(PATHS_SHOWN)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    );
    if blocked.len() > PATHS_SHOWN {
        line.push_str(&format!(" +{}", blocked.len() - PATHS_SHOWN));
    }
    let allowed = total.saturating_sub(blocked.len());
    if allowed > 0 {
        line.push_str(&format!(" · {allowed} allowed"));
    }
    line
}

pub fn bots_table(report: &AccessReport, intent: &Intent) -> BotsTable {
    let total = report.important.len();
    let verdicts = report.has_verdicts();
    let key_pages = |n: usize| {
        if n == 1 {
            "1 key page".to_owned()
        } else {
            format!("{n} key pages")
        }
    };
    let mut rows: Vec<ProductRow> = engines()
        .iter()
        .map(|e| {
            let bot = registry().bot(e.crawler);
            let operator = bot.map_or_else(|| e.name.to_owned(), |b| b.operator.clone());
            let stance = bot.map_or(Stance::Any, |b| intent.effective(b));
            let access = report.bot(e.crawler);
            let blocked = access.map_or(0, |a| a.blocked.len());
            let home_blocked = access.and_then(|a| a.home_allowed) == Some(false);
            let fetch = report
                .probes
                .as_ref()
                .and_then(|p| Some((p, shown_verdict(p, e.crawler)?)))
                .filter(|(_, v)| v.is_positive());
            let (status, pages, why) = if !verdicts {
                (
                    ProductStatus::Unknown,
                    "robots.txt unreadable".to_owned(),
                    "robots.txt couldn't be read on the latest crawl, so there is no verdict per bot.".to_owned(),
                )
            } else if blocked > 0 && stance == Stance::Block {
                (
                    ProductStatus::BlockedByYou,
                    format!("{} · your choice", blocked_line(report, e.crawler, total)),
                    format!(
                        "robots.txt blocks {} as you chose. It doesn't lower your score.",
                        e.crawler
                    ),
                )
            } else if blocked > 0 && blocked >= total {
                (
                    ProductStatus::Blocked,
                    format!("Blocked on all {}", key_pages(total)),
                    format!(
                        "robots.txt blocks {} on every key page, so {} can't use your site.",
                        e.crawler,
                        engine_short(e.id)
                    ),
                )
            } else if blocked > 0 {
                (
                    if home_blocked {
                        ProductStatus::Blocked
                    } else {
                        ProductStatus::Partly
                    },
                    blocked_line(report, e.crawler, total),
                    format!(
                        "robots.txt blocks {} on {blocked} of {}{}.",
                        e.crawler,
                        key_pages(total),
                        if home_blocked {
                            ", including the home page"
                        } else {
                            ""
                        }
                    ),
                )
            } else if let Some((probes, v)) = fetch {
                let (worse, probed) = worse_count(probes, e.crawler);
                (
                    ProductStatus::Partly,
                    format!("{} on {worse} of {probed} probed", fetch_problem(v)),
                    format!(
                        "robots.txt allows {}, but our fetch as it was treated worse than a normal visitor ({}).",
                        e.crawler,
                        fetch_problem(v).to_lowercase()
                    ),
                )
            } else {
                (
                    ProductStatus::Allowed,
                    format!("All {}", key_pages(total)),
                    format!("{} reaches all {}.", e.crawler, key_pages(total)),
                )
            };
            ProductRow {
                key: format!("{:?}", e.id).to_lowercase(),
                name: engine_short(e.id),
                operator,
                bots: e.crawler.to_owned(),
                status,
                pages,
                why,
                counted: !matches!(status, ProductStatus::BlockedByYou | ProductStatus::Unknown),
            }
        })
        .collect();

    // The training crawlers: one row, never counted.
    let training: Vec<&codoseo_geo::Bot> = registry()
        .bots
        .iter()
        .filter(|b| b.purpose == Purpose::Training && b.crawls)
        .collect();
    if !training.is_empty() {
        let out: Vec<&&codoseo_geo::Bot> = training
            .iter()
            .filter(|b| report.bot(&b.token).and_then(|a| a.home_allowed) == Some(false))
            .collect();
        let chosen = out.iter().all(|b| intent.effective(b) == Stance::Block);
        let mut tokens: Vec<String> = training.iter().take(2).map(|b| b.token.clone()).collect();
        if training.len() > 2 {
            tokens.push(format!("+{}", training.len() - 2));
        }
        let (status, pages, why) = if !verdicts {
            (
                ProductStatus::Unknown,
                "robots.txt unreadable".to_owned(),
                "robots.txt couldn't be read on the latest crawl.".to_owned(),
            )
        } else if out.is_empty() {
            (
                ProductStatus::Allowed,
                format!("All {} may crawl", training.len()),
                "No training crawler is blocked. Training use doesn't change your scores."
                    .to_owned(),
            )
        } else if chosen {
            (
                ProductStatus::BlockedByYou,
                format!("{} of {} blocked · your choice", out.len(), training.len()),
                "Blocked, as you chose. It doesn't lower your score.".to_owned(),
            )
        } else {
            (
                ProductStatus::Partly,
                format!("{} of {} blocked", out.len(), training.len()),
                "Some training crawlers are blocked. Training use doesn't change your scores."
                    .to_owned(),
            )
        };
        rows.push(ProductRow {
            key: "training".to_owned(),
            name: "Training crawlers",
            operator: "Common Crawl".to_owned(),
            bots: tokens.join(", "),
            status,
            pages,
            why,
            counted: false,
        });
    }

    let counted: Vec<&ProductRow> = rows.iter().filter(|r| r.counted).collect();
    BotsTable {
        reach: counted
            .iter()
            .filter(|r| r.status == ProductStatus::Allowed)
            .count(),
        of: counted.len(),
        blocked: counted
            .iter()
            .filter(|r| r.status == ProductStatus::Blocked)
            .count(),
        partly: counted
            .iter()
            .filter(|r| r.status == ProductStatus::Partly)
            .count(),
        unknown: !verdicts,
        rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codoseo_geo::report::{BlockedUrl, BotAccess, ImportantUrl, Reason, RobotsInfo};
    use codoseo_geo::robots::{GroupMatch, RobotsAvailability};

    fn report(blocked: &[(&str, &[u16])]) -> AccessReport {
        let important = ["/", "/pricing", "/about", "/blog", "/faq"]
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
                        group: GroupMatch::None,
                        home_rule: None,
                        rules: Vec::new(),
                        blocked: urls
                            .iter()
                            .map(|&u| BlockedUrl { url: u, rule: None })
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
        }
    }

    fn row<'a>(t: &'a BotsTable, name: &str) -> &'a ProductRow {
        t.rows.iter().find(|r| r.name == name).unwrap()
    }

    #[test]
    fn every_engine_is_a_row_and_the_training_crawlers_one_more() {
        let t = bots_table(&report(&[]), &Intent::default());
        assert_eq!(t.rows.len(), engines().len() + 1);
        assert_eq!((t.reach, t.of), (engines().len(), engines().len()));
        assert_eq!(row(&t, "Claude").pages, "All 5 key pages");
        assert_eq!(row(&t, "Claude").status, ProductStatus::Allowed);
        assert!(!t.rows.last().unwrap().counted);
    }

    #[test]
    fn blocks_read_blocked_partly_or_by_choice() {
        let r = report(&[
            ("PerplexityBot", &[0, 1, 2, 3]),
            ("OAI-SearchBot", &[3]),
            ("Claude-SearchBot", &[0, 1, 2, 3, 4]),
        ]);
        let mut intent = Intent::default();
        intent
            .bots
            .insert("Claude-SearchBot".to_owned(), Stance::Block);
        let t = bots_table(&r, &intent);
        let p = row(&t, "Perplexity");
        assert_eq!(p.status, ProductStatus::Blocked, "the home page is blocked");
        assert_eq!(p.pages, "Blocked: /, /pricing, /about +1 · 1 allowed");
        assert!(p.why.contains("including the home page"), "{}", p.why);
        let c = row(&t, "ChatGPT");
        assert_eq!(c.status, ProductStatus::Partly);
        assert_eq!(c.pages, "Blocked: /blog · 4 allowed");
        let a = row(&t, "Claude");
        assert_eq!(a.status, ProductStatus::BlockedByYou);
        assert!(!a.counted);
        assert_eq!(t.of, engines().len() - 1);
        assert_eq!(t.reach, engines().len() - 3);
        assert_eq!((t.blocked, t.partly), (1, 1));
    }

    #[test]
    fn an_unreadable_robots_txt_is_unknown() {
        let mut r = report(&[]);
        r.robots.availability = RobotsAvailability::Unavailable;
        r.bots.clear();
        let t = bots_table(&r, &Intent::default());
        assert!(t.unknown);
        assert!(t.rows.iter().all(|r| r.status == ProductStatus::Unknown));
        assert_eq!(t.of, 0);
    }
}
