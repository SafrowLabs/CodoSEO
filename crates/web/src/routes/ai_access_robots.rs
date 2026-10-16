//! The robots.txt tab of `/s/{site}/ai-access`: the file as the latest crawl fetched it, line by
//! line, with the groups each bot obeys marked, a tester ("can this bot fetch this path?") and a
//! summary of the file's groups.
//!
//! The file's text is kept in the report ([`codoseo_geo::report::RobotsBody`]) since this tab
//! came; a report from before shows what it can and says the text appears after the next crawl.
//! The tester runs on the server with the same matcher the reports use, at
//! `/ai-access/robots/test?bot=&path=` (an htmx fragment, or the whole tab without JavaScript).

use codoseo_geo::report::{AccessReport, agent_rules};
use codoseo_geo::robots::{
    GroupMatch, GroupOutline, MatchedRule, RobotsAvailability, RobotsTxt, product_token,
};
use codoseo_geo::{Honours, registry};
use serde::Deserialize;

use crate::routes::ai_access::plural;

/// Lines of the file shown; a longer file says how many more there are.
pub const MAX_SHOWN_LINES: usize = 2_000;
/// Longest path the tester takes, in bytes.
pub const MAX_TEST_PATH: usize = 1_024;
/// Longest product token the tester takes, in characters.
const MAX_TOKEN: usize = 64;
/// Bots offered as chips above the tester's select.
const CHIPS: usize = 6;
/// Agents a group's summary names before "+N".
const GROUP_AGENTS: usize = 4;
/// Bots worth testing first when nothing names them.
const USUAL: [&str; 6] = [
    "OAI-SearchBot",
    "PerplexityBot",
    "ClaudeBot",
    "GPTBot",
    "Googlebot",
    "ChatGPT-User",
];

#[derive(Debug, Default, Deserialize)]
pub struct RobotsQuery {
    pub bot: Option<String>,
    pub path: Option<String>,
}

pub struct RobotsTab {
    pub url: String,
    pub status: String,
    pub status_class: &'static str,
    /// `612 bytes`
    pub size: Option<String>,
    pub truncated: bool,
    /// `Last changed: crawl #39, Oct 2` or `Unchanged since crawl #44, …`
    pub changed: Option<String>,
    pub checked: String,
    pub checked_full: String,
    /// The file, line by line; empty when there is no text to show (see `note`).
    pub lines: Vec<ViewLine>,
    /// Lines past [`MAX_SHOWN_LINES`].
    pub lines_more: usize,
    /// Why there are no lines.
    pub note: Option<&'static str>,
    pub tester: Tester,
    pub groups: Vec<GroupSummary>,
    /// `No Crawl-delay · 1 Content-Signal line · 1 sitemap`
    pub groups_foot: String,
    /// The Content-Signal lines as the report read them.
    pub signals: Vec<String>,
}

pub struct ViewLine {
    pub n: u32,
    pub text: String,
    /// `is-comment`, `is-in` (the tested bot's group), `is-hit` (the deciding line), or empty.
    pub class: &'static str,
    /// `3 bots` on a group's first line; `decides /path` on the deciding line.
    pub note: String,
}

pub struct Tester {
    /// The bot tested, as written in the registry (or as asked, for one it doesn't list).
    pub bot: String,
    pub path: String,
    pub chips: Vec<TesterChip>,
    pub options: Vec<TesterOption>,
    /// The bot asked for when the registry doesn't list it, as the select's first option.
    pub other: Option<String>,
    /// Set unless the input was refused or there is nothing to test against.
    pub result: Option<TestResult>,
    /// Why the input was refused (the response is then a 400).
    pub error: Option<String>,
    /// Why there is nothing to test against.
    pub unavailable: Option<&'static str>,
}

pub struct TesterChip {
    pub token: String,
    pub href: String,
    pub current: bool,
}

pub struct TesterOption {
    pub value: String,
    pub label: String,
    pub selected: bool,
}

pub struct TestResult {
    pub head: String,
    pub class: &'static str,
    pub why: String,
}

pub struct GroupSummary {
    pub agents: String,
    pub text: String,
}

/// What a bot gets for a path from a file: the verdict, the groups it read and the rule that
/// decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tested {
    pub allowed: bool,
    pub group: GroupMatch,
    /// Indexes into [`RobotsTxt::outline`].
    pub groups: Vec<u32>,
    pub rule: Option<MatchedRule>,
}

/// Runs `token` against `robots` for `path`, with the fallback its operator documents.
pub fn test_path(robots: &RobotsTxt, token: &str, path: &str) -> Tested {
    let agent = agent_rules(robots, token);
    let verdict = robots.verdict_for(&agent, path);
    Tested {
        allowed: verdict.allowed,
        group: verdict.group,
        groups: agent.groups().to_vec(),
        rule: verdict.rule,
    }
}

/// A path the tester takes: starts with `/`, at most [`MAX_TEST_PATH`] bytes, no whitespace or
/// control characters.
pub fn valid_path(path: &str) -> Result<(), String> {
    if !path.starts_with('/') {
        return Err("The path must start with /.".to_owned());
    }
    if path.len() > MAX_TEST_PATH {
        return Err(format!(
            "The path is too long: at most {MAX_TEST_PATH} characters."
        ));
    }
    if path.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("The path can't contain spaces or control characters.".to_owned());
    }
    Ok(())
}

/// A token the tester takes: a registry bot, or a product token as robots.txt reads one.
fn valid_token(token: &str) -> Result<String, String> {
    if let Some(b) = registry().bot(token) {
        return Ok(b.token.clone());
    }
    let ok = !token.is_empty()
        && token.chars().count() <= MAX_TOKEN
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(token.to_owned())
    } else {
        Err("A bot is a product token: letters, digits, - and _ only.".to_owned())
    }
}

/// How many registry bots read each group of the file, by group index.
pub fn readers(robots: &RobotsTxt, groups: usize) -> Vec<usize> {
    let mut n = vec![0usize; groups];
    for b in &registry().bots {
        for &g in agent_rules(robots, &b.token).groups() {
            if let Some(c) = n.get_mut(g as usize) {
                *c += 1;
            }
        }
    }
    n
}

/// The file's lines for the viewer: comments muted, each group's first line annotated with how
/// many registry bots read it, and `tested`'s groups and deciding line marked.
pub fn view_lines(
    body: &str,
    outline: &[GroupOutline],
    readers: &[usize],
    tested: Option<(&Tested, &str)>,
) -> (Vec<ViewLine>, usize) {
    let text = body.strip_prefix('\u{feff}').unwrap_or(body);
    let all: Vec<&str> = text.lines().collect();
    let total = all.len();
    let lines = all
        .into_iter()
        .take(MAX_SHOWN_LINES)
        .enumerate()
        .map(|(i, line)| {
            let n = u32::try_from(i + 1).unwrap_or(u32::MAX);
            let first_of = outline.iter().position(|g| g.first_line == n);
            let in_tested = tested.is_some_and(|(t, _)| {
                t.groups.iter().any(|&g| {
                    outline
                        .get(g as usize)
                        .is_some_and(|g| (g.first_line..=g.last_line).contains(&n))
                })
            });
            let hit = tested.and_then(|(t, path)| {
                t.rule
                    .as_ref()
                    .filter(|r| r.line == n)
                    .map(|_| format!("decides {path}"))
            });
            let class = if hit.is_some() {
                "is-hit"
            } else if in_tested {
                "is-in"
            } else if line.trim_start().starts_with('#') {
                "is-comment"
            } else {
                ""
            };
            let note = hit.unwrap_or_else(|| match first_of {
                Some(g) => plural(readers.get(g).copied().unwrap_or(0), "bot", "bots"),
                None => String::new(),
            });
            ViewLine {
                n,
                text: line.to_owned(),
                class,
                note,
            }
        })
        .collect();
    (lines, total.saturating_sub(MAX_SHOWN_LINES))
}

fn rule_text(r: &MatchedRule) -> String {
    format!(
        "{}: {}",
        if r.allow { "Allow" } else { "Disallow" },
        r.pattern
    )
}

/// The tester's verdict in words.
pub fn result_text(outline: &[GroupOutline], token: &str, path: &str, t: &Tested) -> TestResult {
    let first = t
        .groups
        .first()
        .and_then(|g| outline.get(*g as usize))
        .map_or(0, |g| g.first_line);
    let applies = match t.group {
        GroupMatch::Named => format!("Its own group (line {first}) applies"),
        GroupMatch::Fallback => format!(
            "No group names it, so it follows {}'s group (line {first})",
            registry().robots_fallback(token).unwrap_or("another bot")
        ),
        GroupMatch::Wildcard => {
            format!("No group names it, so User-agent: * (line {first}) applies")
        }
        GroupMatch::None => "No group applies to it".to_owned(),
    };
    let mut why = if path == "/robots.txt" {
        "/robots.txt itself is always allowed.".to_owned()
    } else {
        match (&t.rule, t.group) {
            (Some(r), _) => format!("{applies}; line {} “{}” matches.", r.line, rule_text(r)),
            (None, GroupMatch::None) => format!("{applies}, so everything is allowed."),
            (None, _) => format!("{applies}; no rule in it matches {path}, so it's allowed."),
        }
    };
    match registry().bot(token) {
        Some(b) => match b.honours_robots {
            Honours::Partial => why.push_str(&format!(
                " {} says this bot doesn't follow robots.txt for every request.",
                b.operator
            )),
            Honours::No => why.push_str(&format!(
                " {} says this bot ignores robots.txt anyway.",
                b.operator
            )),
            Honours::Yes | Honours::Unknown => {}
        },
        None => why.push_str(" It isn't in the registry: tested as the token written."),
    }
    TestResult {
        head: if t.allowed {
            format!("{token} can fetch {path}")
        } else {
            format!("{token} can't fetch {path}")
        },
        class: if t.allowed { "c-ok" } else { "c-err" },
        why,
    }
}

fn byte_size(n: u32) -> String {
    if n < 1024 {
        plural(n as usize, "byte", "bytes")
    } else {
        format!("{:.1} KB", f64::from(n) / 1024.0)
    }
}

fn encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// The bots offered as chips: those a group names, then the usual ones, at most [`CHIPS`].
fn chip_tokens(robots: Option<&RobotsTxt>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(r) = robots {
        for b in &registry().bots {
            if agent_rules(r, &b.token).group() == GroupMatch::Named && out.len() < CHIPS / 2 {
                out.push(b.token.clone());
            }
        }
    }
    for t in USUAL {
        if out.len() < CHIPS && !out.iter().any(|o| o == t) && registry().bot(t).is_some() {
            out.push(t.to_owned());
        }
    }
    out
}

/// What the tab needs beyond the report.
pub struct RobotsInput<'a> {
    pub base: &'a str,
    pub report: &'a AccessReport,
    pub url: String,
    pub checked: String,
    pub checked_full: String,
    /// `crawl #39, Oct 2` for the crawl the body was first stored with, and whether it changed
    /// then.
    pub since: Option<(String, bool)>,
}

pub fn robots_tab(input: &RobotsInput, query: &RobotsQuery) -> RobotsTab {
    let report = input.report;
    let (status, status_class) = match (report.robots.availability, report.robots.status) {
        (RobotsAvailability::Ok, Some(s)) => (format!("Fetched · HTTP {s}"), "t-ok"),
        (RobotsAvailability::Missing, Some(s)) => (format!("None · HTTP {s}"), "t-muted"),
        (RobotsAvailability::Unavailable, Some(s)) => (format!("Failing · HTTP {s}"), "t-err"),
        (RobotsAvailability::Unavailable, None) => ("Unreachable".to_owned(), "t-err"),
        _ => ("Not checked".to_owned(), "t-muted"),
    };
    let file = report
        .robots
        .file
        .as_ref()
        .filter(|_| report.robots.availability == RobotsAvailability::Ok);
    let parsed = match report.robots.availability {
        RobotsAvailability::Ok => file.map(|f| RobotsTxt::parse(f.body.as_bytes())),
        RobotsAvailability::Missing => Some(RobotsTxt::parse(b"")),
        _ => None,
    };
    let outline = parsed.as_ref().map(RobotsTxt::outline).unwrap_or_default();

    // The tester's input: refused values still show, with why.
    let mut error = None;
    let bot = match query
        .bot
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
    {
        Some(b) => match valid_token(b) {
            Ok(t) => t,
            Err(e) => {
                error = Some(e);
                b.chars().take(MAX_TOKEN).collect()
            }
        },
        None => chip_tokens(parsed.as_ref())
            .into_iter()
            .next()
            .unwrap_or_else(|| "OAI-SearchBot".to_owned()),
    };
    let path = query
        .path
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or("/")
        .to_owned();
    if error.is_none()
        && let Err(e) = valid_path(&path)
    {
        error = Some(e);
    }
    let path_shown: String = path.chars().take(MAX_TEST_PATH).collect();

    let tested = match (&parsed, &error) {
        (Some(r), None) => Some(test_path(r, &bot, &path)),
        _ => None,
    };
    let unavailable = match (report.robots.availability, file) {
        (RobotsAvailability::Ok, None) => Some(
            "The file's text wasn't stored for this check, so it can't be tested yet. It is from the next crawl.",
        ),
        (RobotsAvailability::Unavailable, _) => Some(
            "robots.txt is failing, so bots that follow it stay off every path until it answers again.",
        ),
        (RobotsAvailability::Unknown, _) => Some("robots.txt wasn't fetched on the latest crawl."),
        _ => None,
    };
    let result = tested.as_ref().map(|t| {
        let mut r = result_text(&outline, &bot, &path, t);
        if report.robots.availability == RobotsAvailability::Missing {
            r.why = "There is no robots.txt, so every path is allowed.".to_owned();
        }
        r
    });

    let (lines, lines_more) = match (file, &parsed) {
        (Some(f), Some(r)) => view_lines(
            &f.body,
            &outline,
            &readers(r, outline.len()),
            tested.as_ref().map(|t| (t, path.as_str())),
        ),
        _ => (Vec::new(), 0),
    };
    let note = match (report.robots.availability, file) {
        (RobotsAvailability::Ok, None) => Some(
            "The file's text wasn't stored for this check; it appears here after the next crawl.",
        ),
        (RobotsAvailability::Ok, Some(f)) if f.body.trim().is_empty() => {
            Some("The file is empty: every bot may crawl every page.")
        }
        (RobotsAvailability::Missing, _) => {
            Some("There is no robots.txt, so every bot that follows it may crawl every page.")
        }
        (RobotsAvailability::Unavailable, _) => Some(
            "While robots.txt fails, bots that follow it treat the whole site as off limits. Make /robots.txt answer 200, or 404 if you have none.",
        ),
        (RobotsAvailability::Unknown, _) => {
            Some("The latest crawl didn't fetch robots.txt. It is read again on the next crawl.")
        }
        _ => None,
    };

    let chip_list = chip_tokens(parsed.as_ref());
    let href = |t: &str| {
        format!(
            "{}/ai-access/robots?bot={}&path={}",
            input.base,
            encode(t),
            encode(&path_shown)
        )
    };
    let chips = chip_list
        .iter()
        .map(|t| TesterChip {
            href: href(t),
            current: *t == bot,
            token: t.clone(),
        })
        .collect();
    let options = registry()
        .bots
        .iter()
        .map(|b| TesterOption {
            value: b.token.clone(),
            label: format!("{} · {}", b.token, b.operator),
            selected: b.token == bot,
        })
        .collect();

    let (groups, groups_foot) = match &parsed {
        Some(r) => {
            let counts = readers(r, outline.len());
            let groups = outline
                .iter()
                .enumerate()
                .map(|(i, g)| {
                    let mut agents: Vec<String> =
                        g.agents.iter().take(GROUP_AGENTS).cloned().collect();
                    // product_token lower-cases: show the registry's spelling where it has one.
                    for a in &mut agents {
                        if let Some(b) = registry()
                            .bots
                            .iter()
                            .find(|b| product_token(&b.token) == *a)
                        {
                            *a = b.token.clone();
                        }
                    }
                    let mut agents = agents.join(", ");
                    if g.agents.len() > GROUP_AGENTS {
                        agents.push_str(&format!(" +{}", g.agents.len() - GROUP_AGENTS));
                    }
                    let rules = match g.rules.as_slice() {
                        [] => "no rules".to_owned(),
                        [one] => rule_text(one),
                        many => plural(many.len(), "rule", "rules"),
                    };
                    let bots = counts.get(i).copied().unwrap_or(0);
                    let text = if g.is_wildcard() {
                        format!("{rules} · {} fall back to it", plural(bots, "bot", "bots"))
                    } else {
                        format!(
                            "{rules} · {}",
                            plural(bots, "registry bot", "registry bots")
                        )
                    };
                    GroupSummary {
                        agents,
                        text: format!("lines {}–{} · {text}", g.first_line, g.last_line),
                    }
                })
                .collect();
            let delays = outline.iter().filter(|g| g.crawl_delay).count();
            let signals = report.declared.content_signals.len();
            let sitemaps = r.sitemaps().len();
            let foot = [
                if delays == 0 {
                    "No Crawl-delay".to_owned()
                } else {
                    format!("Crawl-delay in {}", plural(delays, "group", "groups"))
                },
                if signals == 0 {
                    "no Content-Signal line".to_owned()
                } else {
                    plural(signals, "Content-Signal line", "Content-Signal lines")
                },
                plural(sitemaps, "sitemap", "sitemaps"),
            ]
            .join(" · ");
            (groups, foot)
        }
        None => (Vec::new(), String::new()),
    };

    RobotsTab {
        url: input.url.clone(),
        status,
        status_class,
        size: file.map(|f| byte_size(f.bytes)),
        truncated: file.is_some_and(|f| f.truncated),
        changed: input
            .since
            .as_ref()
            .filter(|_| file.is_some())
            .map(|(c, changed)| {
                if *changed {
                    format!("Last changed: {c}")
                } else {
                    format!("Unchanged since {c}")
                }
            }),
        checked: input.checked.clone(),
        checked_full: input.checked_full.clone(),
        lines,
        lines_more,
        note,
        tester: Tester {
            other: registry().bot(&bot).is_none().then(|| bot.clone()),
            bot,
            path: path_shown,
            chips,
            options,
            result: if error.is_some() { None } else { result },
            error,
            unavailable,
        },
        groups,
        groups_foot,
        signals: report
            .declared
            .content_signals
            .iter()
            .map(|s| {
                let pairs: Vec<String> = s
                    .pairs
                    .iter()
                    .map(|p| format!("{}={}", p.key, p.value))
                    .collect();
                let who = if s.agents.is_empty() {
                    "all bots".to_owned()
                } else {
                    s.agents.join(", ")
                };
                format!("line {} · {who} · {}", s.line, pairs.join(", "))
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "# robots.txt\nUser-agent: *\nDisallow: /checkout/\nAllow: /\n\nUser-agent: GPTBot\nUser-agent: CCBot\nDisallow: /\n\n# added later\nUser-agent: PerplexityBot\nDisallow: /\nAllow: /blog/\n\nSitemap: https://x.example/sitemap.xml\n";

    fn parsed() -> (RobotsTxt, Vec<GroupOutline>) {
        let r = RobotsTxt::parse(FILE.as_bytes());
        let o = r.outline();
        (r, o)
    }

    #[test]
    fn groups_span_their_field_lines_only() {
        let (_, o) = parsed();
        let spans: Vec<(u32, u32)> = o.iter().map(|g| (g.first_line, g.last_line)).collect();
        // The comment and blank lines between groups, and the Sitemap line, belong to none.
        assert_eq!(spans, vec![(2, 4), (6, 8), (11, 13)]);
        assert!(o[0].is_wildcard());
        assert_eq!(o[1].agents, vec!["gptbot", "ccbot"]);
        assert_eq!(o[2].rules.len(), 2);
    }

    #[test]
    fn the_deciding_line_is_the_longest_match_in_the_groups_read() {
        let (r, _) = parsed();
        let t = test_path(&r, "GPTBot", "/pricing");
        assert!(!t.allowed);
        assert_eq!(t.group, GroupMatch::Named);
        assert_eq!(t.groups, vec![1]);
        assert_eq!(t.rule.as_ref().map(|r| r.line), Some(8));
        // Allow: /blog/ is longer than Disallow: / for PerplexityBot.
        let t = test_path(&r, "PerplexityBot", "/blog/post");
        assert!(t.allowed);
        assert_eq!(t.rule.as_ref().map(|r| r.line), Some(13));
        // Nothing names OAI-SearchBot: the * group, /checkout/ beats Allow: /.
        let t = test_path(&r, "OAI-SearchBot", "/checkout/pay");
        assert!(!t.allowed);
        assert_eq!(t.group, GroupMatch::Wildcard);
        assert_eq!(t.rule.as_ref().map(|r| r.line), Some(3));
        // Applebot follows Googlebot, and with no Googlebot group reads the * group.
        let t = test_path(&r, "Applebot", "/");
        assert!(t.allowed);
        assert_eq!(t.groups, vec![0]);
    }

    #[test]
    fn the_viewer_marks_the_tested_group_and_counts_each_groups_readers() {
        let (r, o) = parsed();
        let counts = readers(&r, o.len());
        let total = registry().bots.len();
        assert_eq!(counts[1], 2, "GPTBot and CCBot");
        assert_eq!(counts[2], 1);
        assert_eq!(counts[0], total - 3);
        let t = test_path(&r, "GPTBot", "/");
        let (lines, more) = view_lines(FILE, &o, &counts, Some((&t, "/")));
        assert_eq!(more, 0);
        assert_eq!(lines.len(), FILE.lines().count());
        assert_eq!(lines[0].class, "is-comment");
        assert_eq!(lines[1].note, format!("{} bots", total - 3));
        assert_eq!(lines[5].class, "is-in");
        assert_eq!(lines[5].note, "2 bots");
        assert_eq!(lines[7].class, "is-hit");
        assert_eq!(lines[7].note, "decides /");
        assert_eq!(lines[9].class, "is-comment");
        assert_eq!(lines[10].class, "");
    }

    #[test]
    fn the_result_names_the_group_and_the_line() {
        let (r, o) = parsed();
        let t = test_path(&r, "GPTBot", "/");
        let res = result_text(&o, "GPTBot", "/", &t);
        assert_eq!(res.head, "GPTBot can't fetch /");
        assert_eq!(res.class, "c-err");
        assert!(
            res.why
                .starts_with("Its own group (line 6) applies; line 8 “Disallow: /” matches."),
            "{}",
            res.why
        );
        let t = test_path(&r, "SomeBot", "/a");
        let res = result_text(&o, "SomeBot", "/a", &t);
        assert_eq!(res.head, "SomeBot can fetch /a");
        assert!(res.why.contains("User-agent: * (line 2)"), "{}", res.why);
        assert!(res.why.contains("isn't in the registry"), "{}", res.why);
    }

    #[test]
    fn paths_and_tokens_are_checked() {
        assert!(valid_path("/a?b=c").is_ok());
        assert!(valid_path("a").is_err());
        assert!(valid_path("/a b").is_err());
        assert!(valid_path(&format!("/{}", "a".repeat(MAX_TEST_PATH))).is_err());
        assert_eq!(valid_token("gptbot").unwrap(), "GPTBot");
        assert_eq!(valid_token("My-Bot_2").unwrap(), "My-Bot_2");
        assert!(valid_token("<script>").is_err());
        assert!(valid_token(&"a".repeat(65)).is_err());
    }
}
