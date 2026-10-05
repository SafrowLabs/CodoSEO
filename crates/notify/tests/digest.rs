//! The weekly digest: snapshots of both formats for one site and for three, the subject, the
//! headline and the score delta in its three states.

use codoseo_core::check::Severity;
use codoseo_notify::digest::{DigestView, SeverityCounts, SiteDigest};
use codoseo_notify::message::AlertItem;

fn change(severity: Severity, label: &str, url: &str, before: &str, after: &str) -> AlertItem {
    AlertItem {
        severity,
        kind: label.to_lowercase().replace(' ', "_"),
        kind_label: label.to_owned(),
        url: Some(url.to_owned()),
        before: before.to_owned(),
        after: after.to_owned(),
    }
}

fn example() -> SiteDigest {
    SiteDigest {
        domain: "example.com".into(),
        score: 92,
        score_delta: Some(3),
        checks_passed: 37,
        checks_total: 40,
        new_issues: vec![
            ("Page returns a 5xx error".into(), 2),
            ("Title is too long".into(), 14),
        ],
        resolved_issues: vec![("Missing meta description".into(), 6)],
        changes_by_severity: SeverityCounts {
            critical: 1,
            warning: 4,
            notice: 11,
        },
        top_changes: vec![
            change(
                Severity::Critical,
                "Became noindex",
                "https://example.com/pricing",
                "index",
                "noindex",
            ),
            change(
                Severity::Warning,
                "Status changed",
                "https://example.com/blog/old-post",
                "200",
                "404",
            ),
        ],
        dashboard_url: "https://codoseo.test/s/11111111-1111-1111-1111-111111111111/audit".into(),
        rankorg_url: Some(
            "https://rankorg.com/?domain=example.com&utm_source=codoseo&utm_medium=digest".into(),
        ),
    }
}

fn quiet(domain: &str, score: u8, delta: Option<i32>) -> SiteDigest {
    SiteDigest {
        domain: domain.into(),
        score,
        score_delta: delta,
        checks_passed: 40,
        checks_total: 40,
        new_issues: vec![],
        resolved_issues: vec![],
        changes_by_severity: SeverityCounts::default(),
        top_changes: vec![],
        dashboard_url: format!("https://codoseo.test/s/{domain}/audit"),
        rankorg_url: None,
    }
}

fn view(sites: Vec<SiteDigest>) -> DigestView {
    DigestView {
        account_email: "owner@example.com".into(),
        week_label: "Sep 28 to Oct 5, 2026".into(),
        sites,
        settings_url: "https://codoseo.test/settings/alerts".into(),
    }
}

fn three() -> DigestView {
    view(vec![
        example(),
        quiet("shop.example.org", 88, Some(-2)),
        // A first week: nothing to compare with.
        quiet("new-site.dev", 71, None),
    ])
}

#[test]
fn one_site_html() {
    insta::assert_snapshot!(view(vec![example()]).render().unwrap().html.unwrap());
}

#[test]
fn one_site_text() {
    insta::assert_snapshot!(view(vec![example()]).render().unwrap().text);
}

#[test]
fn three_sites_html() {
    insta::assert_snapshot!(three().render().unwrap().html.unwrap());
}

#[test]
fn three_sites_text() {
    insta::assert_snapshot!(three().render().unwrap().text);
}

#[test]
fn the_subject_names_the_score_and_delta_for_one_site_and_counts_several() {
    assert_eq!(
        view(vec![example()]).subject(),
        "CodoSEO weekly: example.com 92 (+3)"
    );
    assert_eq!(
        view(vec![quiet("a.com", 80, Some(-4))]).subject(),
        "CodoSEO weekly: a.com 80 (-4)"
    );
    assert_eq!(
        view(vec![quiet("a.com", 80, Some(0))]).subject(),
        "CodoSEO weekly: a.com 80 (no change)"
    );
    // The first week has no delta to show.
    assert_eq!(
        view(vec![quiet("a.com", 80, None)]).subject(),
        "CodoSEO weekly: a.com 80"
    );
    assert_eq!(three().subject(), "CodoSEO weekly: 3 sites");
}

#[test]
fn the_headline_counts_checks_and_new_issues() {
    assert_eq!(
        example().headline(),
        "Your site passed 37/40 checks. 2 new issues."
    );
    let mut one = example();
    one.new_issues.truncate(1);
    assert_eq!(
        one.headline(),
        "Your site passed 37/40 checks. 1 new issue."
    );
    one.new_issues.clear();
    assert_eq!(
        one.headline(),
        "Your site passed 37/40 checks. No new issues."
    );
    // First week: nothing to compare against, so no claim about new issues.
    assert_eq!(
        quiet("a.com", 71, None).headline(),
        "Your site passed 40/40 checks."
    );
}

#[test]
fn the_rankorg_line_shows_only_when_there_is_a_link() {
    let with = view(vec![example()]).render().unwrap();
    assert!(with.text.contains("rankorg.com"));
    assert!(with.html.unwrap().contains("rankorg.com"));
    let without = view(vec![quiet("a.com", 80, None)]).render().unwrap();
    assert!(!without.text.to_lowercase().contains("rankorg"));
    assert!(!without.html.unwrap().to_lowercase().contains("rankorg"));
}

#[test]
fn the_email_goes_to_the_account_with_both_parts_and_escapes_html() {
    let mut site = example();
    site.new_issues = vec![("Title <b>bold</b> & more".into(), 1)];
    // A page URL and values from the crawled site are attacker-controlled text.
    site.top_changes = vec![change(
        Severity::Critical,
        "Title changed",
        "https://example.com/\"><script>alert(1)</script>",
        "<img src=x onerror=alert(2)>",
        "a & b \"quoted\" 'single'",
    )];
    let email = view(vec![site]).render().unwrap();
    assert_eq!(email.to, "owner@example.com");
    assert!(email.text.contains("Title <b>bold</b> & more"));
    let html = email.html.unwrap();
    assert!(
        html.contains("Title &#60;b&#62;bold&#60;/b&#62; &#38; more"),
        "{html}"
    );
    assert!(!html.contains("<b>bold</b>"));
    // Hostile page URL, before and after: escaped everywhere in the HTML, raw in the text.
    assert!(!html.contains("<script>"), "{html}");
    assert!(!html.contains("<img src=x"), "{html}");
    assert!(!html.contains("\"><script"), "{html}");
    assert!(
        html.contains("&#60;script&#62;alert(1)&#60;/script&#62;"),
        "{html}"
    );
    assert!(
        html.contains("&#60;img src=x onerror=alert(2)&#62;"),
        "{html}"
    );
    assert!(
        html.contains("a &#38; b &#34;quoted&#34; &#39;single&#39;"),
        "{html}"
    );
    assert!(email.text.contains("<script>alert(1)</script>"));
    assert!(!html.contains("<img"), "no external images");
}
