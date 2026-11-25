//! Writes every email CodoSEO sends, with sample data, as `.html` and `.txt` files:
//! `cargo run -p codoseo-notify --example letters -- <dir>`. For looking at the templates in a
//! browser or a mail client; nothing here runs in production.

use std::fs;
use std::path::PathBuf;

use codoseo_core::check::Severity;
use codoseo_notify::digest::{DigestView, SeverityCounts, SiteDigest};
use codoseo_notify::message::{self, AlertItem, AlertMessage, EmailOptions};
use codoseo_notify::{Email, letters};
use uuid::Uuid;

const LOGO: &str = "https://codoseo.com/email-logo.png";
const UNSUB: &str = "https://codoseo.com/email/unsubscribe/eyJhIjoib3duZXJAZXhhbXBsZS5jb20ifQ.sig";
const OPT_OUT: &str =
    "https://codoseo.com/email/unsubscribe/eyJhIjoic29tZW9uZUBleGFtcGxlLmNvbSJ9.sig";

fn item(
    severity: Severity,
    kind: &str,
    label: &str,
    url: &str,
    before: &str,
    after: &str,
) -> AlertItem {
    AlertItem {
        severity,
        kind: kind.to_owned(),
        kind_label: label.to_owned(),
        url: Some(url.to_owned()),
        before: before.to_owned(),
        after: after.to_owned(),
    }
}

fn changes() -> Vec<AlertItem> {
    vec![
        item(
            Severity::Critical,
            "became_noindex",
            "Key page became noindex",
            "https://example.com/pricing",
            "index",
            "noindex",
        ),
        item(
            Severity::Critical,
            "status_changed",
            "Page returns 404",
            "https://example.com/journal/old-trail-guide",
            "200",
            "404",
        ),
        item(
            Severity::Warning,
            "title_changed",
            "Title changed",
            "https://example.com/tents",
            "Tents | Ridgeline",
            "Tents",
        ),
    ]
}

fn main() {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "target/letters".to_owned()),
    );
    fs::create_dir_all(&dir).expect("output directory");
    let to = "owner@example.com";
    let site = "https://codoseo.com/s/11111111-1111-1111-1111-111111111111/changes";
    let opts = EmailOptions {
        logo_url: LOGO,
        unsubscribe: Some(UNSUB),
    };
    let test_opts = EmailOptions {
        logo_url: LOGO,
        unsubscribe: None,
    };
    let digest = DigestView {
        account_email: to.to_owned(),
        week_label: "Nov 17 to Nov 24, 2026".to_owned(),
        sites: vec![SiteDigest {
            domain: "example.com".to_owned(),
            score: 92,
            score_delta: Some(3),
            checks_passed: 37,
            checks_total: 40,
            new_issues: vec![
                ("Page returns a 5xx error".to_owned(), 2),
                ("Title is too long".to_owned(), 14),
            ],
            resolved_issues: vec![("Missing meta description".to_owned(), 6)],
            changes_by_severity: SeverityCounts {
                critical: 1,
                warning: 4,
                notice: 11,
            },
            top_changes: changes().into_iter().take(2).collect(),
            dashboard_url: "https://codoseo.com/s/11111111-1111-1111-1111-111111111111/audit"
                .to_owned(),
            rankorg_url: Some(
                "https://rankorg.com/?domain=example.com&utm_source=codoseo&utm_medium=digest"
                    .to_owned(),
            ),
        }],
        settings_url: "https://codoseo.com/settings/alerts".to_owned(),
        unsubscribe_url: UNSUB.to_owned(),
        logo_url: LOGO.to_owned(),
    };
    let mails: Vec<(&str, Email)> = vec![
        (
            "01-sign-in",
            letters::sign_in(LOGO, "https://codoseo.com/auth/magic/dR4kQ9s")
                .email(to)
                .unwrap(),
        ),
        (
            "02-report-link",
            letters::report_link(
                LOGO,
                "example.com",
                "https://codoseo.com/auth/magic/dR4kQ9s",
                OPT_OUT,
            )
            .email(to)
            .unwrap(),
        ),
        (
            "03-confirm-monitoring",
            letters::confirm_monitoring(
                LOGO,
                "example.com",
                "https://codoseo.com/monitoring/start/dR4kQ9s",
                OPT_OUT,
            )
            .email(to)
            .unwrap(),
        ),
        (
            "04-instant-alert",
            message::email(
                &AlertMessage::changes(
                    "example.com",
                    site,
                    Uuid::nil(),
                    "3 changes on example.com",
                    changes(),
                ),
                to,
                &opts,
            ),
        ),
        (
            "05-site-unreachable",
            message::email(
                &AlertMessage::unreachable("example.com", site, Uuid::nil(), "it didn't answer"),
                to,
                &opts,
            ),
        ),
        ("06-weekly-digest", digest.render().unwrap()),
        (
            "07-keep-monitoring",
            letters::keep_monitoring(
                LOGO,
                to,
                "https://codoseo.com/monitoring/resume/dR4kQ9s",
                UNSUB,
            )
            .email(to)
            .unwrap(),
        ),
        (
            "08-channel-off",
            letters::channel_off(
                LOGO,
                "example.com",
                "Slack",
                "the server answered 410: channel_not_found",
                "https://codoseo.com/settings/alerts",
                to,
                UNSUB,
            )
            .email(to)
            .unwrap(),
        ),
        (
            "09-test-message",
            message::email(&AlertMessage::test("example.com", site), to, &test_opts),
        ),
    ];
    for (name, mail) in mails {
        fs::write(
            dir.join(format!("{name}.html")),
            mail.html.as_deref().unwrap_or(""),
        )
        .unwrap();
        fs::write(
            dir.join(format!("{name}.txt")),
            format!("Subject: {}\n\n{}", mail.subject, mail.text),
        )
        .unwrap();
        println!("{name}: {}", mail.subject);
    }
    println!("written to {}", dir.display());
}
