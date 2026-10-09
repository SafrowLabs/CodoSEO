use codoseo_geo::declared::{parse_content_signal, parse_content_usage, parse_tdmrep};
use codoseo_geo::robots::{GroupMatch, RobotsAvailability, RobotsTxt, availability};

fn allowed(file: &str, token: &str, path: &str) -> bool {
    RobotsTxt::parse(file.as_bytes())
        .verdict(token, path)
        .allowed
}

#[test]
fn rules_carry_one_based_line_numbers() {
    let t = RobotsTxt::parse(b"# header\nUser-agent: *\n\nDisallow: /a\nAllow: /a/b # ok\n");
    let v = t.verdict("x", "/a/c");
    assert!(!v.allowed);
    let rule = v.rule.expect("a rule matched");
    assert_eq!(
        (rule.line, rule.allow, rule.pattern.as_str()),
        (4, false, "/a")
    );
    assert_eq!(t.verdict("x", "/a/b").rule.map(|r| r.line), Some(5));
}

#[test]
fn named_group_beats_wildcard_and_same_token_groups_merge() {
    let file = "User-agent: *\nDisallow: /\n\nUser-agent: OAI-SearchBot\nAllow: /\n\nuser-agent: oai-searchbot\nDisallow: /private\n";
    let t = RobotsTxt::parse(file.as_bytes());
    let open = t.verdict("OAI-SearchBot", "/page");
    assert!(open.allowed);
    assert_eq!(open.group, GroupMatch::Named);
    // The second group for the same token merges with the first.
    assert!(!t.verdict("oai-searchbot", "/private/x").allowed);
    let other = t.verdict("GPTBot", "/page");
    assert!(!other.allowed);
    assert_eq!(other.group, GroupMatch::Wildcard);
    assert_eq!(
        RobotsTxt::parse(b"").verdict("GPTBot", "/").group,
        GroupMatch::None
    );
}

#[test]
fn tokens_are_exact_products() {
    let file = "User-agent: Googlebot-News\nDisallow: /\n";
    assert!(allowed(file, "Googlebot", "/x"));
    assert!(!allowed(file, "Googlebot-News", "/x"));
    let gpt = "User-agent: GPTBot\nDisallow: /\n";
    assert!(!allowed(gpt, "GPTBot", "/"));
    assert!(allowed(gpt, "OAI-SearchBot", "/"));
}

#[test]
fn longest_pattern_wins_allow_wins_ties_wildcards_and_robots_txt() {
    let file = "User-agent: *\nDisallow: /p\nAllow: /p/ok\nDisallow: /*.pdf$\nDisallow: /same\nAllow: /same\nDisallow: /\n";
    assert!(allowed(file, "x", "/p/ok/1"));
    assert!(!allowed(file, "x", "/p/no"));
    assert!(!allowed(file, "x", "/a.pdf"));
    assert!(allowed(file, "x", "/same"));
    assert!(allowed(file, "x", "/robots.txt"));
}

const CLOUDFLARE: &str =
    "User-Agent: *\nContent-signal: search=yes, ai-train=no, use=reference\nAllow: /\n";

#[test]
fn content_signal_in_the_wildcard_group() {
    let t = RobotsTxt::parse(CLOUDFLARE.as_bytes());
    let [signal] = t.content_signals() else {
        panic!("one content signal");
    };
    assert_eq!(signal.line, 2);
    assert_eq!(signal.agents, ["*"]);
    let pairs: Vec<(&str, &str, bool)> = signal
        .pairs
        .iter()
        .map(|p| (p.key.as_str(), p.value.as_str(), p.known))
        .collect();
    assert_eq!(
        pairs,
        [
            ("search", "yes", true),
            ("ai-train", "no", true),
            ("use", "reference", true)
        ]
    );
    // The signal is a group member, so the Allow after it still belongs to the group.
    assert!(t.verdict("GPTBot", "/x").allowed);
}

#[test]
fn content_usage_with_and_without_a_path_and_unknown_keys() {
    let t = RobotsTxt::parse(
        b"Content-Usage: train-ai=n\nUser-agent: GPTBot\nContent-Usage: /ai-ok/ train-ai=y\nContent-Signal: Fancy=Maybe\n",
    );
    let usage = t.content_usage();
    assert_eq!(usage.len(), 2);
    assert_eq!((usage[0].line, usage[0].path.clone()), (1, None));
    assert!(usage[0].agents.is_empty());
    assert_eq!(usage[0].pairs[0].key, "train-ai");
    assert_eq!(usage[0].pairs[0].value, "n");
    assert_eq!(usage[1].path.as_deref(), Some("/ai-ok/"));
    assert_eq!(usage[1].agents, ["gptbot"]);
    let pair = &t.content_signals()[0].pairs[0];
    assert_eq!(
        (pair.key.as_str(), pair.value.as_str(), pair.known),
        ("fancy", "Maybe", false)
    );
}

#[test]
fn declared_value_parsers() {
    assert_eq!(
        parse_content_signal("Search=yes ai-train=no"),
        [
            ("search".into(), "yes".into()),
            ("ai-train".into(), "no".into())
        ]
    );
    let (path, pairs) = parse_content_usage("/ai-ok/ train-ai=y");
    assert_eq!(path.as_deref(), Some("/ai-ok/"));
    assert_eq!(pairs, [("train-ai".to_owned(), "y".to_owned())]);
    assert_eq!(parse_content_usage("train-ai=n").0, None);
}

#[test]
fn tdmrep_valid_and_invalid() {
    let ok = parse_tdmrep(
        r#"[{"location":"/*","tdm-reservation":1,"tdm-policy":"https://e.test/policy.json"},{"location":"/free/","tdm-reservation":0}]"#,
    )
    .expect("valid");
    assert_eq!(ok.len(), 2);
    assert_eq!(ok[0].reservation, Some(1));
    assert_eq!(ok[0].policy.as_deref(), Some("https://e.test/policy.json"));
    assert_eq!(ok[1].reservation, Some(0));
    assert_eq!(ok[1].policy, None);
    assert!(parse_tdmrep("not json").is_err());
    assert!(parse_tdmrep(r#"{"location":"/"}"#).is_err());
    assert!(parse_tdmrep(r#"[{"tdm-reservation":1}]"#).is_err());
    assert!(parse_tdmrep(r#"[{"location":"/","tdm-reservation":2}]"#).is_err());
}

#[test]
fn availability_table() {
    use RobotsAvailability::*;
    for (status, want) in [
        (None, Unknown),
        (Some(200), Ok),
        (Some(204), Ok),
        (Some(404), Missing),
        (Some(410), Missing),
        (Some(429), Unavailable),
        (Some(500), Unavailable),
        (Some(503), Unavailable),
    ] {
        assert_eq!(availability(status), want, "{status:?}");
    }
}

#[test]
fn line_numbers_survive_crlf_bom_and_the_size_cut() {
    let t = RobotsTxt::parse(
        "\u{feff}User-agent: *\r\nDisallow: /a\r\n\r\nDisallow: /b\r\n".as_bytes(),
    );
    assert_eq!(t.verdict("x", "/b").rule.map(|r| r.line), Some(4));
    assert!(
        !t.verdict("x", "/a").allowed,
        "the BOM does not hide the first line"
    );

    let mut body = b"User-agent: *\nDisallow: /early\n".to_vec();
    body.resize(500 * 1024, b'\n');
    body.extend_from_slice(b"Disallow: /late\n");
    let t = RobotsTxt::parse(&body);
    assert_eq!(t.verdict("x", "/early").rule.map(|r| r.line), Some(2));
    assert!(
        t.verdict("x", "/late").allowed,
        "past the cut nothing is read"
    );
}

#[test]
fn a_hostile_file_stays_cheap() {
    // Many stars, many groups with many content-signal lines: parsing and verdicts
    // must neither blow up in time nor in memory.
    let mut body = String::new();
    for _ in 0..400 {
        body.push_str("Disallow: ");
        body.push_str(&"*".repeat(1_000));
        body.push_str("x\n");
    }
    body = format!("User-agent: *\n{body}");
    for _ in 0..5_000 {
        body.push_str("User-agent: a\n");
    }
    for _ in 0..5_000 {
        body.push_str("Content-Signal: search=yes\n");
    }
    let started = std::time::Instant::now();
    let t = RobotsTxt::parse(body.as_bytes());
    for _ in 0..2_000 {
        assert!(t.verdict("GPTBot", "/some/path?q=1").allowed);
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert!(t.content_signals().len() <= 100);
}
