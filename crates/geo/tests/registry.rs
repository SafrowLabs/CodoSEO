//! Lints `data/ai-bots.json`: the CI check that keeps the public registry well-formed.

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use codoseo_geo::{Honours, Purpose, Registry, registry, registry_json};
use serde_json::Value;

/// Parses `YYYY-MM-DD` into (y, m, d), rejecting impossible dates.
fn parse_date(s: &str) -> Option<(u32, u32, u32)> {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<u32>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    (1..=days).contains(&d).then_some((y, m, d))
}

/// Today's UTC date (Howard Hinnant's civil-from-days).
fn today() -> (u32, u32, u32) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before 1970")
        .as_secs();
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y as u32, m as u32, d as u32)
}

#[test]
fn parses_and_is_large_enough() {
    let r = registry();
    assert_eq!(r.version, 1);
    assert_eq!(r.license, "CC0-1.0");
    assert!(serde_json::from_str::<Value>(registry_json()).is_ok());
}

const OPERATORS: [&str; 11] = [
    "OpenAI",
    "Anthropic",
    "Perplexity",
    "Google",
    "Apple",
    "Meta",
    "Amazon",
    "DuckDuckGo",
    "Mistral",
    "Common Crawl",
    "Microsoft",
];

/// Every lint the registry must pass, as a list of problems (empty when clean). Taking the
/// registry as an argument lets the tests below feed it deliberately broken copies.
fn lint(r: &Registry) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen = HashSet::new();
    let updated = parse_date(&r.updated);
    match updated {
        None => problems.push("updated is not a date".to_owned()),
        Some(u) if u > today() => problems.push("updated is in the future".to_owned()),
        Some(_) => {}
    }
    if !r.homepage.starts_with("https://") {
        problems.push("homepage is not https".to_owned());
    }
    for b in &r.bots {
        let t = &b.token;
        if t.is_empty()
            || !t
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            problems.push(format!("bad token {t:?}"));
        }
        if !seen.insert(t.to_ascii_lowercase()) {
            problems.push(format!("duplicate {t}"));
        }
        if !b.source_url.starts_with("https://") {
            problems.push(format!("{t} source_url is not https"));
        }
        for u in b.ip_ranges_url.iter().chain(b.signature_agent.iter()) {
            if !u.starts_with("https://") {
                problems.push(format!("{t} url {u} is not https"));
            }
        }
        match (parse_date(&b.last_reviewed), updated) {
            (None, _) => problems.push(format!("{t} last_reviewed is not a date")),
            (Some(rev), Some(upd)) if rev > upd => {
                problems.push(format!("{t} reviewed after updated"));
            }
            _ => {}
        }
        if b.notes.is_empty() {
            problems.push(format!("{t} has no notes"));
        }
        if !b.crawls && (b.user_agent_contains.is_some() || b.ip_ranges_url.is_some()) {
            problems.push(format!("control token {t} has a network identity"));
        }
        if let Some(fallback) = &b.robots_fallback {
            let known = r.bots.iter().any(|o| {
                o.token.eq_ignore_ascii_case(fallback) && !o.token.eq_ignore_ascii_case(t)
            });
            if !known {
                problems.push(format!(
                    "{t} falls back to {fallback}, which is not another bot"
                ));
            }
        }
    }
    let ops: HashSet<&str> = r.bots.iter().map(|b| b.operator.as_str()).collect();
    for want in OPERATORS {
        if !ops.contains(want) {
            problems.push(format!("missing operator {want}"));
        }
    }
    problems
}

#[test]
fn the_shipped_registry_passes_the_lint() {
    assert_eq!(lint(registry()), Vec::<String>::new());
    assert!(registry().bots.iter().any(|b| !b.crawls));
}

#[test]
fn the_lint_catches_each_kind_of_mistake() {
    let broken = |edit: &dyn Fn(&mut Registry)| {
        let mut r = registry().clone();
        edit(&mut r);
        lint(&r)
    };
    let has = |p: Vec<String>, needle: &str| {
        assert!(p.iter().any(|m| m.contains(needle)), "{needle}: {p:?}");
    };
    has(
        broken(&|r| r.bots[1].token = r.bots[0].token.to_ascii_uppercase()),
        "duplicate",
    );
    has(
        broken(&|r| r.bots[0].token = "bad token".into()),
        "bad token",
    );
    has(
        broken(&|r| r.bots[0].last_reviewed = "2026-02-30".into()),
        "not a date",
    );
    has(
        broken(&|r| r.bots[0].last_reviewed = "2099-01-01".into()),
        "after updated",
    );
    has(broken(&|r| r.updated = "2099-01-01".into()), "future");
    has(
        broken(&|r| r.bots[0].source_url = "http://example.com/".into()),
        "source_url",
    );
    has(
        broken(&|r| r.bots[0].ip_ranges_url = Some("http://example.com/x.json".into())),
        "not https",
    );
    has(broken(&|r| r.bots[0].notes.clear()), "no notes");
    has(
        broken(&|r| {
            let i = r.bots.iter().position(|b| !b.crawls).unwrap();
            r.bots[i].user_agent_contains = Some("x".into());
        }),
        "network identity",
    );
    has(
        broken(&|r| r.bots.retain(|b| b.operator != "Microsoft")),
        "missing operator Microsoft",
    );
    has(
        broken(&|r| r.bots[0].robots_fallback = Some("NoSuchBot".into())),
        "not another bot",
    );
    has(
        broken(&|r| r.bots[0].robots_fallback = Some(r.bots[0].token.clone())),
        "not another bot",
    );
}

#[test]
fn only_applebot_documents_a_fallback() {
    let with: Vec<(&str, &str)> = registry()
        .bots
        .iter()
        .filter_map(|b| Some((b.token.as_str(), b.robots_fallback.as_deref()?)))
        .collect();
    assert_eq!(with, [("Applebot", "Googlebot")]);
    assert_eq!(registry().robots_fallback("applebot"), Some("Googlebot"));
}

#[test]
fn registry_is_large_enough() {
    assert!(
        registry().bots.len() >= 25,
        "only {}",
        registry().bots.len()
    );
}

#[test]
fn schema_matches_the_rust_fields_and_enums() {
    let schema: Value = serde_json::from_str(include_str!("../data/ai-bots.schema.json"))
        .expect("schema is not JSON");
    let bot = &schema["$defs"]["bot"];

    // `required` equals the field names `Bot` serialises with.
    let sample = serde_json::to_value(&registry().bots[0]).unwrap();
    let mut fields: Vec<&str> = sample
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort_unstable();
    let mut required: Vec<&str> = bot["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    required.sort_unstable();
    assert_eq!(required, fields);
    // The optional fields are properties too, and nothing else is.
    const OPTIONAL: [&str; 1] = ["robots_fallback"];
    let props: HashSet<&str> = bot["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        props,
        fields
            .iter()
            .copied()
            .chain(OPTIONAL)
            .collect::<HashSet<_>>()
    );
    let applebot = serde_json::to_value(registry().bot("Applebot").unwrap()).unwrap();
    for f in OPTIONAL {
        assert!(applebot.get(f).is_some(), "{f} is set somewhere");
    }

    // Every bot in the data uses only enum values the schema allows.
    let allowed = |name: &str| -> HashSet<String> {
        bot["properties"][name]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect()
    };
    let (purposes, honours) = (allowed("purpose"), allowed("honours_robots"));
    let data: Value = serde_json::from_str(registry_json()).unwrap();
    for b in data["bots"].as_array().unwrap() {
        let obj = b.as_object().unwrap();
        for f in &required {
            assert!(obj.contains_key(*f), "{} lacks {f}", b["token"]);
        }
        assert!(
            purposes.contains(b["purpose"].as_str().unwrap()),
            "{}",
            b["token"]
        );
        assert!(
            honours.contains(b["honours_robots"].as_str().unwrap()),
            "{}",
            b["token"]
        );
    }
    // The schema's enums are exactly the Rust enums.
    for (p, name) in [
        (Purpose::Search, "search"),
        (Purpose::UserFetch, "user_fetch"),
        (Purpose::Agent, "agent"),
        (Purpose::Training, "training"),
        (Purpose::Ads, "ads"),
    ] {
        assert_eq!(serde_json::to_value(p).unwrap(), name);
        assert!(purposes.contains(name));
    }
    assert_eq!(purposes.len(), 5);
    for (h, name) in [
        (Honours::Yes, "yes"),
        (Honours::Partial, "partial"),
        (Honours::No, "no"),
        (Honours::Unknown, "unknown"),
    ] {
        assert_eq!(serde_json::to_value(h).unwrap(), name);
        assert!(honours.contains(name));
    }
    assert_eq!(honours.len(), 4);
}

#[test]
fn lookup_ignores_case() {
    let b = registry().bot("oai-searchbot").expect("OAI-SearchBot");
    assert_eq!(b.token, "OAI-SearchBot");
    assert!(registry().bot("no-such-bot").is_none());
}
