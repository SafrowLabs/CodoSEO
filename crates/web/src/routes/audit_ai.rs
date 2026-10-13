//! The site audit's "AI & search · GEO audit" section: what each operator's bots may fetch
//! (robots.txt, judged against the owner's intent), what the synthetic fetch saw, which answer
//! engines can quote the important pages, and the seven AI & GEO checks
//! ([`FindingKind`]) as passed or failing.
//!
//! Everything here is read from the latest access report and the registry: access and checks,
//! not traffic. Real bot visits come with log ingestion (C1), and the section says so rather
//! than inventing them. None of it touches the health score.

use std::collections::BTreeSet;

use codoseo_core::check::Severity;
use codoseo_geo::eligibility::EngineId;
use codoseo_geo::eligibility::engines;
use codoseo_geo::findings::{FindingKind, evaluated_kinds, findings};
use codoseo_geo::llms::LlmsState;
use codoseo_geo::probe::{ProbeReport, Verdict};
use codoseo_geo::report::AccessReport;
use codoseo_geo::robots::RobotsAvailability;
use codoseo_geo::{Bot, Intent, Purpose, Stance, ledger, registry};
use codoseo_store::geo::Incident;
use time::OffsetDateTime;

use crate::ai_incident::asks_attention;
use crate::routes::ai_access::{bot_row, skip_label, stance_label};

/// The AI & GEO checks: one per kind of finding.
pub const GEO_CHECKS: usize = 7;

/// Operators in the order the section shows them; any other registry operator follows in
/// registry order.
const OPERATOR_ORDER: [&str; 10] = [
    "OpenAI",
    "Anthropic",
    "Google",
    "Perplexity",
    "Microsoft",
    "Apple",
    "Meta",
    "Amazon",
    "DuckDuckGo",
    "Mistral",
];

/// Open incidents listed by name in the section before "and N more".
const INCIDENTS_LISTED: usize = 3;

/// The AI & GEO checks of the latest report: each kind of finding that report could judge and
/// found nothing passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeoChecks {
    pub passed: usize,
    pub failing: usize,
    /// Kinds the report could not judge (no probes, no readable robots.txt, ...).
    pub not_checked: usize,
}

impl GeoChecks {
    pub fn of(report: &AccessReport, intent: &Intent) -> GeoChecks {
        let evaluated: BTreeSet<&str> = evaluated_kinds(report)
            .into_iter()
            .map(FindingKind::slug)
            .collect();
        let failing: BTreeSet<&str> = findings(report, intent)
            .into_iter()
            .map(|f| f.kind.slug())
            .collect();
        GeoChecks {
            passed: evaluated.difference(&failing).count(),
            failing: failing.len(),
            not_checked: GEO_CHECKS.saturating_sub(evaluated.union(&failing).count()),
        }
    }
}

/// Everything the section needs besides the incidents panel.
pub struct GeoSection {
    pub kpis: Vec<AiKpi>,
    pub operators: Vec<OperatorCard>,
    pub engines: Vec<EngineBar>,
    /// `important pages · grade A`
    pub engines_aside: String,
    /// Set instead of the operator cards when robots.txt gave no verdicts.
    pub robots_note: Option<&'static str>,
}

pub struct AiKpi {
    pub label: &'static str,
    pub value: String,
    pub note: String,
    /// Text colour class for the value, or empty.
    pub class: &'static str,
}

pub struct OperatorCard {
    pub name: String,
    /// The hashed URL of its logo, when there is one.
    pub logo: Option<&'static str>,
    /// Shown on the tile instead of a logo (`CC`).
    pub monogram: String,
    /// `3 bots`
    pub count: String,
    pub status: &'static str,
    pub status_class: &'static str,
    /// One line for narrow screens: `Search ✓ · User ✓ · Training blocked`.
    pub summary: String,
    pub bots: Vec<BotLine>,
}

pub struct BotLine {
    pub token: String,
    /// `Search`, `User fetch`, `Training control`, ...
    pub purpose: &'static str,
    pub robots: String,
    pub robots_class: &'static str,
    pub robots_tip: String,
    pub fetch: String,
    pub fetch_class: &'static str,
    pub fetch_tip: String,
}

pub struct EngineBar {
    pub name: &'static str,
    /// The engine's full name, for the tooltip.
    pub full: &'static str,
    /// `13 / 13`
    pub label: String,
    pub width: u32,
    pub class: &'static str,
}

/// One open incident, linked to its card (with Fix and Recheck) on the AI access screen.
pub struct IncidentLink {
    pub href: String,
    pub severity: &'static str,
    pub severity_label: &'static str,
    pub title: String,
}

/// What the fetch checks could do for this site: switched on for it, and possible on the
/// install (`CODOSEO_PROBE_URLS` above 0 for the plan).
#[derive(Debug, Clone, Copy)]
pub struct ProbeSetting {
    pub site_on: bool,
    pub installed: bool,
}

pub fn section(report: &AccessReport, intent: &Intent, probes: ProbeSetting) -> GeoSection {
    let eligibility = ledger::entry("eligibility").map(|e| e.grade.label());
    GeoSection {
        kpis: kpis(report, intent, probes),
        operators: operator_cards(report, intent),
        engines: engine_bars(report),
        engines_aside: match eligibility {
            Some(g) => format!("important pages · grade {g}"),
            None => "important pages".to_owned(),
        },
        robots_note: (!report.has_verdicts()).then_some(match report.robots.availability {
            RobotsAvailability::Unknown => {
                "robots.txt wasn't checked on the latest crawl, so there are no per-bot verdicts yet."
            }
            _ => {
                "robots.txt couldn't be read, so bots that follow it treat the whole site as off limits. Per-bot verdicts return once it answers 200 or 404."
            }
        }),
    }
}

/// Open incidents that ask for attention, most severe first, the first few by name.
pub fn incident_links(base: &str, open: &[Incident], now: OffsetDateTime) -> Vec<IncidentLink> {
    let mut asking: Vec<&Incident> = open.iter().filter(|i| asks_attention(i, now)).collect();
    asking.sort_by_key(|i| (i.severity, i.id));
    asking
        .into_iter()
        .take(INCIDENTS_LISTED)
        .map(|i| {
            let (severity, severity_label) = match i.severity {
                Severity::Critical => ("critical", "Critical"),
                Severity::Warning => ("warning", "Warning"),
                Severity::Notice => ("notice", "Notice"),
            };
            IncidentLink {
                href: format!("{base}/ai-access#issue-{}", i.id),
                severity,
                severity_label,
                title: i.title.clone(),
            }
        })
        .collect()
}

fn in_purpose(p: Purpose) -> Vec<&'static Bot> {
    registry().bots.iter().filter(|b| b.purpose == p).collect()
}

fn kpis(report: &AccessReport, intent: &Intent, probes: ProbeSetting) -> Vec<AiKpi> {
    let search = in_purpose(Purpose::Search);
    let training = in_purpose(Purpose::Training);
    let mut out = Vec::new();
    if report.has_verdicts() {
        // As the AI access screen's tiles count them: blocked on any important page.
        let blocked: Vec<&str> = search
            .iter()
            .filter(|b| report.bot(&b.token).is_some_and(|a| !a.blocked.is_empty()))
            .map(|b| b.token.as_str())
            .collect();
        let wanted = intent.purpose_stance(Purpose::Search) == Stance::Allow;
        out.push(AiKpi {
            label: "Search bots allowed",
            value: format!("{} / {}", search.len() - blocked.len(), search.len()),
            note: match blocked.as_slice() {
                [] => "on every important page".to_owned(),
                [one] => format!("blocked: {one}"),
                [one, rest @ ..] => format!("blocked: {one} and {} more", rest.len()),
            },
            class: if blocked.is_empty() {
                "c-ok"
            } else if wanted {
                "c-warn"
            } else {
                ""
            },
        });
        let stopped = training
            .iter()
            .filter(|b| report.bot(&b.token).and_then(|a| a.home_allowed) == Some(false))
            .count();
        let stance = intent.purpose_stance(Purpose::Training);
        out.push(AiKpi {
            label: "Training bots blocked",
            value: format!("{stopped} / {}", training.len()),
            note: format!("your intent: {}", stance_label(stance).to_lowercase()),
            class: if stance == Stance::Block && stopped < training.len() {
                "c-warn"
            } else {
                ""
            },
        });
    } else {
        for (label, n) in [
            ("Search bots allowed", search.len()),
            ("Training bots blocked", training.len()),
        ] {
            out.push(AiKpi {
                label,
                value: format!("— / {n}"),
                note: "robots.txt not readable".to_owned(),
                class: "c-ghost",
            });
        }
    }
    out.push(fetch_kpi(report, intent, probes));
    out.push(content_kpi(report));
    out.push(llms_kpi(report));
    out
}

fn fetch_kpi(report: &AccessReport, intent: &Intent, probes: ProbeSetting) -> AiKpi {
    let off = |note: &str| AiKpi {
        label: "Fetch checks",
        value: "Not run".to_owned(),
        note: note.to_owned(),
        class: "c-ghost",
    };
    let Some(p) = report.probes.as_ref().filter(|p| !p.urls.is_empty()) else {
        return if !probes.installed {
            off("off on this install")
        } else if !probes.site_on {
            off("off for this site")
        } else {
            off("run after the next crawl")
        };
    };
    let grade = ledger::entry("synthetic_probe").map(|e| e.grade.label());
    let worse = p
        .bots
        .iter()
        .filter(|token| {
            let wanted = registry()
                .bot(token)
                .is_some_and(|b| intent.effective(b) == Stance::Allow);
            wanted && shown_verdict(p, token).is_some_and(Verdict::is_positive)
        })
        .count();
    AiKpi {
        label: "Fetch checks",
        value: match worse {
            0 => "All same page".to_owned(),
            n => format!("{n} treated worse"),
        },
        note: match grade {
            Some(g) => format!("synthetic · grade {g}"),
            None => "synthetic".to_owned(),
        },
        class: if worse == 0 { "c-ok" } else { "c-warn" },
    }
}

fn content_kpi(report: &AccessReport) -> AiKpi {
    let grade = ledger::entry("content_verdict").map(|e| e.grade.label());
    match &report.content {
        Some(c) if !c.pages.is_empty() => {
            let shells = c.shells().count();
            AiKpi {
                label: "Content in HTML",
                value: match shells {
                    0 => "No shells".to_owned(),
                    1 => "1 shell".to_owned(),
                    n => format!("{n} shells"),
                },
                note: match grade {
                    Some(g) => format!("of {} important pages · {g}", c.pages.len()),
                    None => format!("of {} important pages", c.pages.len()),
                },
                class: if shells == 0 { "c-ok" } else { "c-warn" },
            }
        }
        _ => AiKpi {
            label: "Content in HTML",
            value: "Not checked".to_owned(),
            note: if report.html_important() == 0 {
                "no HTML pages to read".to_owned()
            } else {
                "checked from the next crawl".to_owned()
            },
            class: "c-ghost",
        },
    }
}

fn llms_kpi(report: &AccessReport) -> AiKpi {
    let (value, class) = match &report.llms_txt {
        None => ("Not checked", "c-ghost"),
        Some(l) => match l.state {
            LlmsState::Available => ("Available", ""),
            LlmsState::Absent | LlmsState::SoftNotFound => ("None", ""),
            LlmsState::Unavailable if l.adopted => ("Unavailable", "c-warn"),
            LlmsState::Unavailable => ("Unavailable", ""),
        },
    };
    AiKpi {
        label: "llms.txt",
        value: value.to_owned(),
        note: "informational · not scored".to_owned(),
        class,
    }
}

/// The verdict a bot's row shows, picked as the AI access screen's fetch check picks it:
/// treated worse anywhere, else the same page, else inconclusive, else whatever there is.
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
        .or_else(|| {
            verdicts
                .iter()
                .find(|v| matches!(v, Verdict::Inconclusive { .. }))
        })
        .or(verdicts.first())
        .copied()
}

fn purpose_short(bot: &Bot) -> &'static str {
    match (bot.purpose, bot.crawls) {
        (Purpose::Training, false) => "Training control",
        (Purpose::Search, _) => "Search",
        (Purpose::UserFetch, _) => "User fetch",
        (Purpose::Agent, _) => "Agent",
        (Purpose::Training, true) => "Training",
        (Purpose::Ads, _) => "Ads",
    }
}

/// The robots.txt cell: allowed or blocked, and whether that is what the owner wants.
fn robots_cell(report: &AccessReport, intent: &Intent, bot: &Bot) -> (String, &'static str) {
    if !report.has_verdicts() {
        return ("Unknown".to_owned(), "t-muted");
    }
    let access = report.bot(&bot.token);
    let total = report.important.len();
    let blocked = access.map_or(0, |a| a.blocked.len());
    let home_blocked = access.and_then(|a| a.home_allowed) == Some(false);
    let stance = intent.effective(bot);
    if blocked == 0 {
        let class = match stance {
            Stance::Block => "t-warn",
            _ => "t-ok",
        };
        let text = if stance == Stance::Block {
            "Allowed · you block it"
        } else {
            "Allowed"
        };
        return (text.to_owned(), class);
    }
    let word = if home_blocked && blocked == total {
        "Blocked".to_owned()
    } else {
        format!("Blocked on {blocked}/{total}")
    };
    match stance {
        Stance::Block if home_blocked => (format!("{word} · intended"), "t-intent"),
        Stance::Allow => (word, "t-err"),
        _ => (word, "t-muted"),
    }
}

/// The synthetic fetch cell, and whether it asks for attention.
fn fetch_cell(
    report: &AccessReport,
    intent: &Intent,
    bot: &Bot,
) -> (String, &'static str, String, bool) {
    let not_probed = |tip: &str| ("Not probed".to_owned(), "t-muted", tip.to_owned(), false);
    if !bot.crawls {
        return not_probed("A control token: it never fetches pages");
    }
    let Some(probes) = &report.probes else {
        return not_probed("This report has no fetch checks");
    };
    if let Some(s) = probes.skipped_bots.iter().find(|s| s.token == bot.token) {
        return not_probed(skip_label(s.reason));
    }
    let Some(v) = shown_verdict(probes, &bot.token) else {
        return not_probed("Not part of this report's fetch checks");
    };
    let wanted = intent.effective(bot) == Stance::Allow;
    let worse = "Treated worse than CodoSEObot: see AI access";
    match v {
        Verdict::Same => (
            "Same page".to_owned(),
            "t-ok",
            "The same page CodoSEObot got".to_owned(),
            false,
        ),
        Verdict::SkippedRobots => {
            not_probed("robots.txt keeps this bot out, so its user agent wasn't sent")
        }
        Verdict::Inconclusive { .. } => (
            "Inconclusive".to_owned(),
            "t-muted",
            "Nothing to compare with, or a second fetch disagreed".to_owned(),
            false,
        ),
        v => {
            let text = match v {
                Verdict::Challenged { vendor } => format!("Challenged · {}", vendor.label()),
                Verdict::Blocked { status: Some(s) } => format!("Blocked · HTTP {s}"),
                Verdict::Blocked { status: None } => "Connection dropped".to_owned(),
                Verdict::Diverted { .. } => "Redirected".to_owned(),
                Verdict::Degraded => "Different page".to_owned(),
                _ => "Rate limited".to_owned(),
            };
            if !wanted {
                return (
                    text,
                    "t-muted",
                    "Expected: you don't ask for this bot, so it isn't reported".to_owned(),
                    false,
                );
            }
            let class = match v {
                Verdict::Throttled | Verdict::Degraded => "t-warn",
                _ => "t-err",
            };
            (text, class, worse.to_owned(), true)
        }
    }
}

fn logo(operator: &str) -> Option<&'static str> {
    let name = match operator {
        "OpenAI" => "logo-openai.svg",
        "Anthropic" => "logo-anthropic.svg",
        "Google" => "logo-google.svg",
        "Perplexity" => "logo-perplexity.svg",
        "Microsoft" => "logo-microsoft.svg",
        "Apple" => "logo-apple.svg",
        "Meta" => "logo-meta.svg",
        "Amazon" => "logo-amazon.svg",
        "DuckDuckGo" => "logo-duckduckgo.svg",
        "Mistral" => "logo-mistral.svg",
        _ => return None,
    };
    Some(crate::assets::url(name))
}

/// `Common Crawl` -> `CC`, `Acme` -> `AC`.
fn monogram(operator: &str) -> String {
    let words: Vec<&str> = operator.split_whitespace().collect();
    let letters: String = if words.len() > 1 {
        words
            .iter()
            .filter_map(|w| w.chars().next())
            .take(2)
            .collect()
    } else {
        operator.chars().take(2).collect()
    };
    letters.to_uppercase()
}

fn operator_cards(report: &AccessReport, intent: &Intent) -> Vec<OperatorCard> {
    let mut names: Vec<&str> = OPERATOR_ORDER
        .iter()
        .copied()
        .filter(|o| registry().bots.iter().any(|b| b.operator == *o))
        .collect();
    for b in &registry().bots {
        if !names.contains(&b.operator.as_str()) {
            names.push(&b.operator);
        }
    }
    names
        .into_iter()
        .map(|name| operator_card(report, intent, name))
        .collect()
}

fn operator_card(report: &AccessReport, intent: &Intent, name: &str) -> OperatorCard {
    let bots: Vec<&Bot> = registry()
        .bots
        .iter()
        .filter(|b| b.operator == name)
        .collect();
    let mut attention = false;
    let mut summary = Vec::new();
    let lines: Vec<BotLine> = bots
        .iter()
        .map(|bot| {
            let (robots, robots_class) = robots_cell(report, intent, bot);
            let (fetch, fetch_class, fetch_tip, fetch_attention) = fetch_cell(report, intent, bot);
            let row = bot_row(report, intent, bot);
            attention |= fetch_attention || (report.has_verdicts() && row.attention);
            let short = if bot.crawls {
                purpose_short(bot)
                    .split(' ')
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            } else {
                bot.token.clone()
            };
            summary.push(if fetch_attention {
                format!(
                    "{short} {}",
                    fetch.split(" · ").next().unwrap_or_default().to_lowercase()
                )
            } else if robots.starts_with("Blocked") {
                format!("{short} blocked")
            } else if robots.starts_with("Allowed") {
                format!("{short} ✓")
            } else {
                format!("{short} ?")
            });
            BotLine {
                token: bot.token.clone(),
                purpose: purpose_short(bot),
                robots,
                robots_class,
                robots_tip: row.home_tip,
                fetch,
                fetch_class,
                fetch_tip,
            }
        })
        .collect();
    let (status, status_class) = if !report.has_verdicts() {
        ("Unknown", "t-muted")
    } else if attention {
        ("Needs a look", "t-warn")
    } else {
        ("OK", "t-ok")
    };
    OperatorCard {
        name: name.to_owned(),
        logo: logo(name),
        monogram: monogram(name),
        count: match bots.len() {
            1 => "1 bot".to_owned(),
            n => format!("{n} bots"),
        },
        status,
        status_class,
        summary: summary.join(" · "),
        bots: lines,
    }
}

fn engine_short(id: EngineId) -> &'static str {
    match id {
        EngineId::Google => "Google",
        EngineId::Bing => "Bing / Copilot",
        EngineId::Apple => "Apple",
        EngineId::Amazon => "Amazon",
        EngineId::ChatGpt => "ChatGPT",
        EngineId::Claude => "Claude",
        EngineId::Perplexity => "Perplexity",
        EngineId::MetaAi => "Meta AI",
        EngineId::DuckDuckGo => "DuckDuckGo",
        EngineId::Mistral => "Mistral",
    }
}

/// Important HTML pages each engine may quote: neither excluded nor limited by the page's own
/// controls, nor kept out by robots.txt.
fn engine_bars(report: &AccessReport) -> Vec<EngineBar> {
    engines()
        .iter()
        .map(|e| {
            let access = report.engine(e.id);
            let html = access.map_or(0, |a| usize::from(a.html_pages));
            let ok = html.saturating_sub(access.map_or(0, |a| a.affected()));
            let (label, width, class) = if html == 0 {
                ("—".to_owned(), 0, "bg-ghost")
            } else {
                (
                    format!("{ok} / {html}"),
                    u32::try_from(ok * 100 / html).unwrap_or(100).max(2),
                    if ok == html {
                        "bg-ok"
                    } else if ok == 0 {
                        "bg-err"
                    } else {
                        "bg-warn"
                    },
                )
            };
            EngineBar {
                name: engine_short(e.id),
                full: e.name,
                label,
                width,
                class,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codoseo_geo::report::{BlockedUrl, BotAccess, ImportantUrl, Reason, RobotsInfo};
    use codoseo_geo::robots::GroupMatch;

    fn report(blocked: &[&str]) -> AccessReport {
        let important = vec![ImportantUrl {
            url: "https://example.com/".to_owned(),
            reason: Reason::Home,
            status: Some(200),
            html_ok: true,
        }];
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
                    let out = blocked.contains(&b.token.as_str());
                    BotAccess {
                        token: b.token.clone(),
                        home_allowed: Some(!out),
                        group: GroupMatch::None,
                        home_rule: None,
                        rules: Vec::new(),
                        blocked: if out {
                            vec![BlockedUrl { url: 0, rule: None }]
                        } else {
                            Vec::new()
                        },
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

    const OFF: ProbeSetting = ProbeSetting {
        site_on: false,
        installed: true,
    };

    #[test]
    fn every_registry_bot_is_on_exactly_one_operator_card() {
        let s = section(&report(&[]), &Intent::default(), OFF);
        let shown: usize = s.operators.iter().map(|o| o.bots.len()).sum();
        assert_eq!(shown, registry().bots.len());
        assert_eq!(s.operators[0].name, "OpenAI");
        assert_eq!(s.operators[2].name, "Google");
        // Common Crawl has no logo: a monogram stands in.
        let cc = s
            .operators
            .iter()
            .find(|o| o.name == "Common Crawl")
            .unwrap();
        assert!(cc.logo.is_none());
        assert_eq!(cc.monogram, "CC");
        assert!(
            s.operators[0]
                .logo
                .unwrap()
                .starts_with("/assets/logo-openai.")
        );
        assert!(
            s.operators.iter().all(|o| o.status == "OK"),
            "nothing blocked"
        );
        assert!(s.robots_note.is_none());
    }

    #[test]
    fn a_blocked_search_bot_needs_a_look_and_a_blocked_training_bot_can_be_intended() {
        let mut intent = Intent::default();
        let s = section(&report(&["OAI-SearchBot", "GPTBot"]), &intent, OFF);
        let openai = &s.operators[0];
        assert_eq!(openai.status, "Needs a look");
        let line = |token: &str| openai.bots.iter().find(|b| b.token == token).unwrap();
        assert_eq!(line("OAI-SearchBot").robots, "Blocked");
        assert_eq!(line("OAI-SearchBot").robots_class, "t-err");
        // No preference on training: a plain fact, not a problem.
        assert_eq!(line("GPTBot").robots_class, "t-muted");
        assert!(
            openai.summary.contains("Search blocked"),
            "{}",
            openai.summary
        );
        assert_eq!(
            s.kpis[0].value,
            format!(
                "{} / {}",
                in_purpose(Purpose::Search).len() - 1,
                in_purpose(Purpose::Search).len()
            )
        );
        assert_eq!(s.kpis[0].class, "c-warn");

        intent.purposes.insert(Purpose::Training, Stance::Block);
        let s = section(&report(&["GPTBot"]), &intent, OFF);
        let gpt = s.operators[0]
            .bots
            .iter()
            .find(|b| b.token == "GPTBot")
            .unwrap();
        assert_eq!(gpt.robots, "Blocked · intended");
        assert_eq!(gpt.robots_class, "t-intent");
        assert!(s.kpis[1].note.contains("block"), "{}", s.kpis[1].note);
    }

    #[test]
    fn without_probes_or_content_the_kpis_say_why() {
        let s = section(&report(&[]), &Intent::default(), OFF);
        let by = |label: &str| s.kpis.iter().find(|k| k.label == label).unwrap();
        assert_eq!(by("Fetch checks").value, "Not run");
        assert_eq!(by("Fetch checks").note, "off for this site");
        assert_eq!(by("Content in HTML").value, "Not checked");
        assert_eq!(by("llms.txt").value, "Not checked");
        assert!(
            s.operators
                .iter()
                .flat_map(|o| &o.bots)
                .all(|b| b.fetch == "Not probed")
        );
    }

    #[test]
    fn an_unreadable_robots_txt_leaves_the_verdicts_unknown() {
        let mut r = report(&[]);
        r.robots.availability = RobotsAvailability::Unavailable;
        r.bots.clear();
        let s = section(&r, &Intent::default(), OFF);
        assert!(s.robots_note.is_some());
        assert!(s.operators.iter().all(|o| o.status == "Unknown"));
        assert_eq!(
            s.kpis[0].value,
            format!("— / {}", in_purpose(Purpose::Search).len())
        );
    }

    #[test]
    fn geo_checks_pass_what_was_judged_and_found_clean() {
        let clean = GeoChecks::of(&report(&[]), &Intent::default());
        assert_eq!(clean.failing, 0);
        assert_eq!(clean.passed + clean.not_checked, GEO_CHECKS);
        // Robots, both bot kinds and answers are judged; no probes, content or llms.txt.
        assert_eq!(clean.passed, 4);
        let blocked = GeoChecks::of(&report(&["OAI-SearchBot"]), &Intent::default());
        assert_eq!(blocked.failing, 1);
        assert_eq!(blocked.passed, 3);
    }

    #[test]
    fn monograms_take_initials() {
        assert_eq!(monogram("Common Crawl"), "CC");
        assert_eq!(monogram("acme"), "AC");
    }
}
