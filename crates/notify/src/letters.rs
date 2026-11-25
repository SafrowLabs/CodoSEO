//! Every one-off email CodoSEO sends, worded in one place: the sign-in link, an audit's report
//! link, the MCP monitoring confirmation, the "Keep monitoring?" reminder and the
//! channel-switched-off notice. Alerts are in [`crate::message`] and the digest in
//! [`crate::digest`]. `cargo run -p codoseo-notify --example letters <dir>` writes them all out
//! with sample data, to look at.

use crate::email::Email;
use crate::letter::{Button, Icon, Letter, Link, Mood, Row, Score};

/// A subject and a letter, ready to address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub subject: String,
    pub letter: Letter,
}

impl Draft {
    pub fn email(&self, to: &str) -> Result<Email, askama::Error> {
        self.letter.email(to, &self.subject)
    }
}

fn stop_all(opt_out: &str) -> Vec<Link> {
    vec![Link {
        label: "Stop all email from CodoSEO".to_owned(),
        url: opt_out.to_owned(),
    }]
}

fn unsubscribe(url: &str) -> Vec<Link> {
    vec![Link {
        label: "Unsubscribe".to_owned(),
        url: url.to_owned(),
    }]
}

fn row(icon: Icon, title: &str, detail: impl Into<String>) -> Row {
    Row {
        icon,
        title: title.to_owned(),
        detail: detail.into(),
    }
}

/// The sign-in form's email. The person asked for it themselves, so there is no stop link:
/// the footer only says what to do if it wasn't them.
pub fn sign_in(images: &str, link: &str, address: &str) -> Draft {
    Draft {
        subject: "Your CodoSEO sign-in link".to_owned(),
        letter: Letter {
            images: images.to_owned(),
            mascot: Some(Mood::Rest),
            eyebrow: Some("Sign in · codoseo.com".to_owned()),
            title: "Welcome back, here's your link".to_owned(),
            intro: vec!["Press the button and you're in. No password to remember.".to_owned()],
            button: Some(Button {
                label: "Sign in to CodoSEO".to_owned(),
                url: link.to_owned(),
            }),
            facts: vec![
                row(
                    Icon::Clock,
                    "15 minutes",
                    "Then it stops working and you can ask for a new one.",
                ),
                row(
                    Icon::Once,
                    "Works once",
                    "The first press signs you in; a second does nothing.",
                ),
                row(
                    Icon::Mail,
                    "Sent to you",
                    format!("{address}, from the sign-in form."),
                ),
            ],
            footer: vec![
                "You got this email because you signed in or signed up on CodoSEO. If it \
                 wasn't you, ignore it; nothing happens without the link."
                    .to_owned(),
            ],
            ..Letter::default()
        },
    }
}

/// What the audit found so far, for the report email.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Audit {
    /// Pages checked.
    pub pages: u32,
    /// Overall, GEO and SEO, once the audit is done.
    pub scores: Option<[i16; 3]>,
    /// "all 9 fixes", or "every fix" before they are known.
    pub fixes: String,
}

/// An audit's "get the full report" email. Anyone may have typed the address, so it carries
/// the opt-out link.
pub fn report_link(images: &str, domain: &str, link: &str, opt_out: &str, audit: &Audit) -> Draft {
    let (title, intro) = match audit.scores {
        Some(_) => (
            "Your full report is ready".to_owned(),
            format!(
                "We checked {} of {domain}. Here is where it stands; the full report has the \
                 rest.",
                pages(audit.pages)
            ),
        ),
        None => (
            format!("Your full report of {domain}"),
            format!(
                "We are checking {domain} now. The full report has everything, as it comes in."
            ),
        ),
    };
    let fixes = if audit.fixes.is_empty() {
        "every fix".to_owned()
    } else {
        audit.fixes.clone()
    };
    Draft {
        subject: format!("Your CodoSEO report for {domain}"),
        letter: Letter {
            images: images.to_owned(),
            mascot: Some(Mood::Rest),
            eyebrow: Some(format!("Audit · {domain}")),
            title,
            intro: vec![intro],
            scores: audit
                .scores
                .map(|[overall, geo, seo]| {
                    vec![
                        score("Overall", overall),
                        score("GEO", geo),
                        score("SEO", seo),
                    ]
                })
                .unwrap_or_default(),
            rows_label: Some("In the full report".to_owned()),
            rows: vec![
                row(
                    Icon::Pages,
                    "Up to 500 pages",
                    "Every page's checks, not only the first 100.",
                ),
                Row {
                    icon: Icon::Fix,
                    title: format!("{}, with copy-paste prompts", capitalise(&fixes)),
                    detail: "Ready for your CMS, your developer or your AI agent.".to_owned(),
                },
                row(
                    Icon::Week,
                    "A re-check every week",
                    "With an email when something important breaks.",
                ),
            ],
            button: Some(Button {
                label: "Open the full report".to_owned(),
                url: link.to_owned(),
            }),
            after: vec![
                "The link works once and stops working in 15 minutes. Opening it creates your \
                 free account; no card, no password."
                    .to_owned(),
            ],
            footer: vec![format!(
                "You got this email because this address was entered on an audit of {domain} \
                 at codoseo.com. If it wasn't you, ignore it; nothing happens without the link."
            )],
            links: stop_all(opt_out),
            ..Letter::default()
        },
    }
}

fn score(label: &str, value: i16) -> Score {
    Score {
        label: label.to_owned(),
        value,
        delta: None,
    }
}

fn pages(n: u32) -> String {
    format!("{n} page{}", if n == 1 { "" } else { "s" })
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// The no-key MCP tool's confirmation: an AI assistant gave CodoSEO the address.
pub fn confirm_monitoring(images: &str, domain: &str, link: &str, opt_out: &str) -> Draft {
    Draft {
        subject: format!("Confirm monitoring for {domain}"),
        letter: Letter {
            images: images.to_owned(),
            mascot: Some(Mood::Crawling),
            eyebrow: Some(format!("Monitoring · {domain}")),
            title: format!("Your assistant asked us to watch {domain}"),
            intro: vec![format!(
                "An AI assistant you are working with asked CodoSEO to monitor {domain}. \
                 Nothing starts until you confirm. When you do:"
            )],
            rows: vec![
                Row {
                    icon: Icon::Crawl,
                    title: format!("We crawl {domain} now"),
                    detail: "Up to 500 pages, 44 SEO checks and 9 AI checks on each.".to_owned(),
                },
                row(
                    Icon::Week,
                    "Then again every week",
                    "On your free CodoSEO account; one is created if you don't have one.",
                ),
                row(
                    Icon::Mail,
                    "You get an email when something important breaks",
                    "A key page gone noindex, a 404 spike, a blocked AI bot.",
                ),
                row(
                    Icon::Key,
                    "Your assistant gets an API key",
                    "Shown once, so it can read your site's results.",
                ),
            ],
            button: Some(Button {
                label: "Confirm and start monitoring".to_owned(),
                url: link.to_owned(),
            }),
            after: vec!["The link works once and expires in 24 hours.".to_owned()],
            footer: vec![
                "You got this email because an AI assistant gave CodoSEO this address. If you \
                 didn't ask for this, ignore it and nothing happens."
                    .to_owned(),
            ],
            links: stop_all(opt_out),
            ..Letter::default()
        },
    }
}

/// The reminder a Free account gets after 30 quiet days. `pauses_on` is the day monitoring
/// pauses without an answer, as words: "Dec 2".
pub fn keep_monitoring(
    images: &str,
    account_email: &str,
    link: &str,
    unsub: &str,
    pauses_on: &str,
) -> Draft {
    Draft {
        subject: "Keep monitoring your site on CodoSEO?".to_owned(),
        letter: Letter {
            images: images.to_owned(),
            mascot: Some(Mood::Idle),
            eyebrow: Some("Your account".to_owned()),
            title: "Still want us to watch your site?".to_owned(),
            intro: vec![
                "We haven't seen you on CodoSEO for 30 days. We're still crawling your site \
                 every week; press the button and we carry on."
                    .to_owned(),
            ],
            button: Some(Button {
                label: "Keep monitoring".to_owned(),
                url: link.to_owned(),
            }),
            facts: vec![
                row(
                    Icon::Clock,
                    "30 quiet days",
                    "No sign-in and no email opened in a month.",
                ),
                row(
                    Icon::Pause,
                    "Pauses in 7 days",
                    format!("If we don't hear from you, monitoring pauses on {pauses_on}."),
                ),
                row(
                    Icon::Safe,
                    "Nothing is deleted",
                    "Signing in again turns it back on, history intact.",
                ),
            ],
            after: vec!["The link works once and expires in 7 days.".to_owned()],
            footer: vec![format!(
                "Sent to {account_email}, the address on your CodoSEO account."
            )],
            links: unsubscribe(unsub),
            unsubscribe: Some(unsub.to_owned()),
            ..Letter::default()
        },
    }
}

/// What the account hears when one of its channels failed on every retry and was switched
/// off. `channel` is the channel's name ("Slack"), `error` the last delivery error.
pub fn channel_off(
    images: &str,
    domain: &str,
    channel: &str,
    error: &str,
    settings_url: &str,
    account_email: &str,
    unsub: &str,
) -> Draft {
    Draft {
        subject: format!("We turned off your {channel} alerts for {domain}"),
        letter: Letter {
            images: images.to_owned(),
            mascot: Some(Mood::Warning),
            eyebrow: Some(format!("Alert channel · {domain}")),
            title: format!("We turned off your {channel} alerts"),
            intro: vec![
                "Every delivery to this channel failed over the last two days, so we stopped \
                 trying. The last error:"
                    .to_owned(),
            ],
            code: Some(error.to_owned()),
            rows: vec![
                Row {
                    icon: Icon::Chat,
                    title: "To fix it".to_owned(),
                    detail: format!(
                        "Check the address in {channel}, then turn the channel back on in your \
                         alert settings."
                    ),
                },
                row(
                    Icon::Mail,
                    "Meanwhile",
                    "Alerts for this site still reach your email, and everything else waits \
                     for the Monday digest.",
                ),
            ],
            button: Some(Button {
                label: "Open alert settings".to_owned(),
                url: settings_url.to_owned(),
            }),
            footer: vec![format!(
                "Sent to {account_email}, the owner of this channel on CodoSEO."
            )],
            links: unsubscribe(unsub),
            unsubscribe: Some(unsub.to_owned()),
            ..Letter::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMAGES: &str = "https://codoseo.com/email/";

    #[test]
    fn the_sign_in_email_has_no_stop_link_and_the_first_contact_ones_do() {
        let signin = sign_in(IMAGES, "https://codoseo.com/auth/magic/t", "a@b.co")
            .email("a@b.co")
            .unwrap();
        assert!(!signin.text.contains("Stop all email"), "{}", signin.text);
        assert!(signin.unsubscribe.is_none());
        assert!(
            signin
                .text
                .contains("Sign in to CodoSEO:\nhttps://codoseo.com/auth/magic/t\n"),
            "{}",
            signin.text
        );
        assert!(
            signin
                .text
                .contains("- Sent to you: a@b.co, from the sign-in form.")
        );
        let audit = Audit {
            pages: 100,
            scores: Some([78, 64, 86]),
            fixes: "all 9 fixes".into(),
        };
        for draft in [
            report_link(
                IMAGES,
                "example.com",
                "https://codoseo.com/auth/magic/t",
                "https://o",
                &audit,
            ),
            confirm_monitoring(
                IMAGES,
                "example.com",
                "https://codoseo.com/monitoring/start/t",
                "https://o",
            ),
        ] {
            let e = draft.email("a@b.co").unwrap();
            assert!(
                e.text.contains("Stop all email from CodoSEO: https://o\n"),
                "{}",
                e.text
            );
            assert!(
                e.unsubscribe.is_none(),
                "an opt-out is not a list unsubscribe"
            );
        }
        assert!(
            confirm_monitoring(IMAGES, "example.com", "https://l", "https://o")
                .email("a@b.co")
                .unwrap()
                .text
                .contains("24 hours")
        );
    }

    #[test]
    fn the_report_email_shows_scores_when_the_audit_is_done_and_not_before() {
        let done = report_link(
            IMAGES,
            "example.com",
            "https://l",
            "https://o",
            &Audit {
                pages: 100,
                scores: Some([78, 64, 86]),
                fixes: "all 9 fixes".into(),
            },
        );
        assert_eq!(done.letter.title, "Your full report is ready");
        let text = done.email("a@b.co").unwrap().text;
        assert!(
            text.contains("We checked 100 pages of example.com."),
            "{text}"
        );
        assert!(text.contains("Overall 78 · GEO 64 · SEO 86\n"), "{text}");
        assert!(
            text.contains("- All 9 fixes, with copy-paste prompts:"),
            "{text}"
        );
        let running = report_link(
            IMAGES,
            "example.com",
            "https://l",
            "https://o",
            &Audit::default(),
        );
        assert_eq!(running.letter.title, "Your full report of example.com");
        assert!(running.letter.scores.is_empty());
        let text = running.email("a@b.co").unwrap().text;
        assert!(
            text.contains("- Every fix, with copy-paste prompts:"),
            "{text}"
        );
    }

    #[test]
    fn notifications_carry_the_one_click_link() {
        let keep = keep_monitoring(
            IMAGES,
            "a@b.co",
            "https://codoseo.com/monitoring/resume/t",
            "https://u",
            "Dec 2",
        )
        .email("a@b.co")
        .unwrap();
        assert_eq!(keep.unsubscribe.as_deref(), Some("https://u"));
        assert!(
            keep.text.contains("Unsubscribe: https://u\n"),
            "{}",
            keep.text
        );
        assert!(
            keep.text.contains("monitoring pauses on Dec 2."),
            "{}",
            keep.text
        );
        let off = channel_off(
            IMAGES,
            "example.com",
            "Slack",
            "the server answered 410",
            "https://codoseo.com/settings/alerts",
            "a@b.co",
            "https://u",
        );
        assert_eq!(
            off.subject,
            "We turned off your Slack alerts for example.com"
        );
        let e = off.email("a@b.co").unwrap();
        assert_eq!(e.unsubscribe.as_deref(), Some("https://u"));
        assert!(
            e.text.contains("    the server answered 410\n"),
            "{}",
            e.text
        );
        let html = e.html.unwrap();
        assert!(html.contains("Open alert settings") && html.contains("mascot-warning.png"));
    }
}
