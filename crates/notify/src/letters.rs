//! Every one-off email CodoSEO sends, worded in one place: the sign-in link, an audit's report
//! link, the MCP monitoring confirmation, the "Keep monitoring?" reminder and the
//! channel-switched-off notice. Alerts are in [`crate::message`] and the digest in
//! [`crate::digest`]. `cargo run -p codoseo-notify --example letters <dir>` writes them all out
//! with sample data, to look at.

use crate::email::Email;
use crate::letter::{Button, Letter, Link};

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

/// The sign-in form's email. The person asked for it themselves, so there is no stop link:
/// the footer only says what to do if it wasn't them.
pub fn sign_in(logo_url: &str, link: &str) -> Draft {
    Draft {
        subject: "Your CodoSEO sign-in link".to_owned(),
        letter: Letter {
            logo_url: logo_url.to_owned(),
            title: "Here's your sign-in link".to_owned(),
            intro: vec![
                "Press the button to sign in to CodoSEO. It works once and stops working in \
                 15 minutes."
                    .to_owned(),
            ],
            button: Some(Button {
                label: "Sign in to CodoSEO".to_owned(),
                url: link.to_owned(),
            }),
            footer: vec![
                "You got this email because you signed in or signed up on CodoSEO. If it \
                 wasn't you, ignore it; nothing happens without the link."
                    .to_owned(),
            ],
            ..Letter::default()
        },
    }
}

/// An audit's "get the full report" email. Anyone may have typed the address, so it carries
/// the opt-out link.
pub fn report_link(logo_url: &str, domain: &str, link: &str, opt_out: &str) -> Draft {
    Draft {
        subject: format!("Your CodoSEO report for {domain}"),
        letter: Letter {
            logo_url: logo_url.to_owned(),
            title: format!("Your full report for {domain}"),
            intro: vec![
                format!(
                    "Open the full CodoSEO report for {domain}. The link works once and stops \
                     working in 15 minutes."
                ),
                "Opening it also starts weekly monitoring of the site, with an email when \
                 something important breaks."
                    .to_owned(),
            ],
            button: Some(Button {
                label: "Open the full report".to_owned(),
                url: link.to_owned(),
            }),
            footer: vec![format!(
                "You got this email because this address was entered on an audit of {domain} \
                 at codoseo.com. If it wasn't you, ignore it; nothing happens without the link."
            )],
            links: stop_all(opt_out),
            ..Letter::default()
        },
    }
}

/// The no-key MCP tool's confirmation: an AI assistant gave CodoSEO the address.
pub fn confirm_monitoring(logo_url: &str, domain: &str, link: &str, opt_out: &str) -> Draft {
    Draft {
        subject: format!("Confirm monitoring for {domain}"),
        letter: Letter {
            logo_url: logo_url.to_owned(),
            title: format!("Confirm monitoring for {domain}"),
            intro: vec![
                format!(
                    "An AI assistant you are working with asked CodoSEO to monitor {domain}. \
                     Press the button to confirm."
                ),
                format!(
                    "CodoSEO then starts weekly monitoring on your free account (one is created \
                     if you don't have one), crawls {domain} now, and emails you when something \
                     important breaks. It also shows you an API key once, which lets your \
                     assistant read your site's results."
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

/// The reminder a Free account gets after 30 quiet days.
pub fn keep_monitoring(logo_url: &str, account_email: &str, link: &str, unsub: &str) -> Draft {
    Draft {
        subject: "Keep monitoring your site on CodoSEO?".to_owned(),
        letter: Letter {
            logo_url: logo_url.to_owned(),
            title: "Keep monitoring your site?".to_owned(),
            intro: vec![
                "We haven't seen you on CodoSEO for 30 days. Press the button and we keep \
                 crawling your site and emailing you when something important breaks."
                    .to_owned(),
                "If we don't hear from you within 7 days we'll pause monitoring. Nothing is \
                 deleted, and signing in again turns it back on."
                    .to_owned(),
            ],
            button: Some(Button {
                label: "Keep monitoring".to_owned(),
                url: link.to_owned(),
            }),
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
    logo_url: &str,
    domain: &str,
    channel: &str,
    error: &str,
    settings_url: &str,
    account_email: &str,
    unsub: &str,
) -> Draft {
    let subject = format!("We turned off your {channel} alerts for {domain}");
    Draft {
        letter: Letter {
            logo_url: logo_url.to_owned(),
            eyebrow: Some(format!("Alert channel · {domain}")),
            title: subject.clone(),
            intro: vec![
                format!("Delivery failed on every retry: {error}"),
                "Check the address, then turn the channel back on in your alert settings. \
                 Until then its alerts wait for the Monday digest."
                    .to_owned(),
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
        subject,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOGO: &str = "https://codoseo.com/email-logo.png";

    #[test]
    fn the_sign_in_email_has_no_stop_link_and_the_first_contact_ones_do() {
        let signin = sign_in(LOGO, "https://codoseo.com/auth/magic/t")
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
        for draft in [
            report_link(
                LOGO,
                "example.com",
                "https://codoseo.com/auth/magic/t",
                "https://o",
            ),
            confirm_monitoring(
                LOGO,
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
            confirm_monitoring(LOGO, "example.com", "https://l", "https://o")
                .email("a@b.co")
                .unwrap()
                .text
                .contains("24 hours")
        );
    }

    #[test]
    fn notifications_carry_the_one_click_link() {
        let keep = keep_monitoring(
            LOGO,
            "a@b.co",
            "https://codoseo.com/monitoring/resume/t",
            "https://u",
        )
        .email("a@b.co")
        .unwrap();
        assert_eq!(keep.unsubscribe.as_deref(), Some("https://u"));
        assert!(
            keep.text.contains("Unsubscribe: https://u\n"),
            "{}",
            keep.text
        );
        let off = channel_off(
            LOGO,
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
        assert!(e.text.contains("the server answered 410"), "{}", e.text);
        assert!(e.html.unwrap().contains("Open alert settings"));
    }
}
