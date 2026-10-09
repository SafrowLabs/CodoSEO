//! Lints `data/ai-bots.json`: the CI check that keeps the public registry well-formed.

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use codoseo_geo::{Bot, Honours, Purpose, registry, registry_json};
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
    assert!(r.bots.len() >= 25, "only {} bots", r.bots.len());
    assert!(serde_json::from_str::<Value>(registry_json()).is_ok());
}

#[test]
fn tokens_are_unique_and_clean() {
    let mut seen = HashSet::new();
    for b in &registry().bots {
        assert!(
            !b.token.is_empty()
                && b.token
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "bad token {:?}",
            b.token
        );
        assert!(
            seen.insert(b.token.to_ascii_lowercase()),
            "duplicate {}",
            b.token
        );
    }
}

#[test]
fn every_operator_is_present() {
    let ops: HashSet<&str> = registry()
        .bots
        .iter()
        .map(|b| b.operator.as_str())
        .collect();
    for want in [
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
    ] {
        assert!(ops.contains(want), "missing operator {want}");
    }
}

#[test]
fn urls_and_dates_are_sane() {
    let r = registry();
    let updated = parse_date(&r.updated).expect("updated is not a date");
    assert!(updated <= today(), "updated is in the future");
    assert!(r.homepage.starts_with("https://"));
    for b in &r.bots {
        assert!(
            b.source_url.starts_with("https://"),
            "{} source_url",
            b.token
        );
        for u in b.ip_ranges_url.iter().chain(b.signature_agent.iter()) {
            assert!(u.starts_with("https://"), "{} url {u}", b.token);
        }
        let reviewed = parse_date(&b.last_reviewed)
            .unwrap_or_else(|| panic!("{} last_reviewed is not a date", b.token));
        assert!(reviewed <= updated, "{} reviewed after updated", b.token);
        assert!(!b.notes.is_empty(), "{} has no notes", b.token);
    }
}

#[test]
fn control_tokens_have_no_network_identity() {
    let controls: Vec<&Bot> = registry().bots.iter().filter(|b| !b.crawls).collect();
    assert!(!controls.is_empty());
    for b in controls {
        assert!(b.user_agent_contains.is_none(), "{}", b.token);
        assert!(b.ip_ranges_url.is_none(), "{}", b.token);
    }
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
    let props: HashSet<&str> = bot["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(props, fields.iter().copied().collect::<HashSet<_>>());

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
