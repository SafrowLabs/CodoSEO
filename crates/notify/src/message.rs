//! The channel-neutral alert: what happened on a site, ready for any channel to render.
//!
//! One message covers one crawl on one site. A crawl with hundreds of changes still produces one
//! message: the listing is capped at [`MAX_ITEMS`] and the rest are counted in `more`.

use codoseo_core::check::Severity;
use uuid::Uuid;

use crate::email::Email;
use crate::letter::{Button, Letter, Line, Link, Tone};

/// How many changes a message lists; the rest become "and N more".
pub const MAX_ITEMS: usize = 20;

/// What the message is about; the webhook `event` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertKind {
    Changes,
    Unreachable,
    Test,
}

impl AlertKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AlertKind::Changes => "changes",
            AlertKind::Unreachable => "unreachable",
            AlertKind::Test => "test",
        }
    }
}

/// One change in the listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertItem {
    pub severity: Severity,
    /// Stable machine name of the change kind (`became_noindex`), for webhooks.
    pub kind: String,
    /// Human label of the change kind ("Became noindex").
    pub kind_label: String,
    pub url: Option<String>,
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertMessage {
    pub kind: AlertKind,
    pub site_domain: String,
    /// The site's dashboard link in CodoSEO.
    pub site_url: String,
    /// `None` for a test message.
    pub crawl_id: Option<Uuid>,
    pub headline: String,
    pub items: Vec<AlertItem>,
    /// Changes beyond the listed ones.
    pub more: u32,
}

impl AlertMessage {
    /// A crawl's changes. Lists the first [`MAX_ITEMS`]; the rest are counted in `more`.
    pub fn changes(
        site_domain: impl Into<String>,
        site_url: impl Into<String>,
        crawl_id: Uuid,
        headline: impl Into<String>,
        mut items: Vec<AlertItem>,
    ) -> AlertMessage {
        let more = items.len().saturating_sub(MAX_ITEMS);
        items.truncate(MAX_ITEMS);
        AlertMessage {
            kind: AlertKind::Changes,
            site_domain: site_domain.into(),
            site_url: site_url.into(),
            crawl_id: Some(crawl_id),
            headline: headline.into(),
            items,
            more: u32::try_from(more).unwrap_or(u32::MAX),
        }
    }

    /// "We couldn't reach your site"; `reason` is the failure in plain words.
    pub fn unreachable(
        site_domain: impl Into<String>,
        site_url: impl Into<String>,
        crawl_id: Uuid,
        reason: &str,
    ) -> AlertMessage {
        let site_domain = site_domain.into();
        AlertMessage {
            kind: AlertKind::Unreachable,
            headline: format!("We couldn't reach {site_domain}: {reason}"),
            site_domain,
            site_url: site_url.into(),
            crawl_id: Some(crawl_id),
            items: Vec::new(),
            more: 0,
        }
    }

    /// What the "send a test" button delivers.
    pub fn test(site_domain: impl Into<String>, site_url: impl Into<String>) -> AlertMessage {
        let site_domain = site_domain.into();
        AlertMessage {
            kind: AlertKind::Test,
            headline: format!("Test notification from CodoSEO for {site_domain}"),
            site_domain,
            site_url: site_url.into(),
            crawl_id: None,
            items: Vec::new(),
            more: 0,
        }
    }

    /// The most severe thing in the message (unreachable counts as critical).
    pub fn severity(&self) -> Severity {
        if self.kind == AlertKind::Unreachable {
            return Severity::Critical;
        }
        self.items
            .iter()
            .map(|i| i.severity)
            .min()
            .unwrap_or(Severity::Notice)
    }
}

impl AlertItem {
    /// "Critical", "Warning" or "Notice".
    pub fn severity_word(&self) -> &'static str {
        severity_label(self.severity)
    }

    /// "index → noindex"; empty when neither side has a value.
    pub fn change_text(&self) -> String {
        match (self.before.is_empty(), self.after.is_empty()) {
            (true, true) => String::new(),
            (false, false) => format!("{} → {}", one_line(&self.before), one_line(&self.after)),
            (true, false) => one_line(&self.after),
            (false, true) => format!("was {}", one_line(&self.before)),
        }
    }
}

pub(crate) fn severity_label(s: Severity) -> &'static str {
    match s {
        Severity::Critical => "Critical",
        Severity::Warning => "Warning",
        Severity::Notice => "Notice",
    }
}

/// Collapses whitespace so a value stays on one line.
pub(crate) fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// At most `max` characters, ending in an ellipsis when cut.
pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// A URL for display: no scheme, cut to `max` characters.
pub(crate) fn short_url(url: &str, max: usize) -> String {
    let bare = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    truncate(bare, max)
}

/// Joins lines while they fit `budget` characters; returns the text and how many lines were left
/// out (the caller adds them to "and N more").
pub(crate) fn fit_lines(lines: &[String], budget: usize) -> (String, usize) {
    let mut out = String::new();
    let mut used = 0;
    for (i, line) in lines.iter().enumerate() {
        let len = line.chars().count() + usize::from(i > 0);
        if used + len > budget {
            return (out, lines.len() - i);
        }
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line);
        used += len;
    }
    (out, 0)
}

fn more_text(more: u32) -> String {
    format!("and {more} more")
}

/// What the email rendering needs beyond the message: the logo image and, for a real alert,
/// the recipient's one-click unsubscribe link (a test message has none).
#[derive(Debug, Clone, Copy, Default)]
pub struct EmailOptions<'a> {
    pub logo_url: &'a str,
    pub unsubscribe: Option<&'a str>,
}

/// The email channel's rendering: subject, plain text and the HTML letter. With an unsubscribe
/// link the footer says how to stop these emails, and the message carries the one-click headers.
pub fn email(msg: &AlertMessage, to: &str, opts: &EmailOptions<'_>) -> Email {
    let subject = format!("CodoSEO: {}", one_line(&msg.headline));
    let (eyebrow, button_label) = match msg.kind {
        AlertKind::Changes => (
            format!("Instant alert · {}", msg.site_domain),
            "See the changes",
        ),
        AlertKind::Unreachable => (
            format!("Site unreachable · {}", msg.site_domain),
            "Open the site in CodoSEO",
        ),
        AlertKind::Test => (
            format!("Test message · {}", msg.site_domain),
            "Open in CodoSEO",
        ),
    };
    let items = msg
        .items
        .iter()
        .map(|i| {
            let mut parts = Vec::new();
            if let Some(url) = &i.url {
                parts.push(url.clone());
            }
            let change = i.change_text();
            if !change.is_empty() {
                parts.push(change);
            }
            Line {
                tone: Tone::from_severity(i.severity),
                title: i.kind_label.clone(),
                detail: (!parts.is_empty()).then(|| parts.join(" · ")),
            }
        })
        .collect();
    let (footer, links) = match opts.unsubscribe {
        Some(url) => (
            vec![format!("Sent to {to}, an alert channel on CodoSEO.")],
            vec![Link {
                label: "Unsubscribe".to_owned(),
                url: url.to_owned(),
            }],
        ),
        None => (
            vec![match msg.kind {
                AlertKind::Test => "A test message from your alert settings on CodoSEO.".to_owned(),
                _ => "Sent to an alert channel on CodoSEO.".to_owned(),
            }],
            Vec::new(),
        ),
    };
    let letter = Letter {
        logo_url: opts.logo_url.to_owned(),
        eyebrow: Some(eyebrow),
        title: msg.headline.clone(),
        intro: match msg.kind {
            AlertKind::Test => vec!["Alerts for this site reach this address.".to_owned()],
            _ => Vec::new(),
        },
        items,
        more: (msg.more > 0).then(|| more_text(msg.more)),
        sections: Vec::new(),
        button: Some(Button {
            label: button_label.to_owned(),
            url: msg.site_url.clone(),
        }),
        after: Vec::new(),
        footer,
        links,
        unsubscribe: opts.unsubscribe.map(str::to_owned),
    };
    letter.email(to, &subject).unwrap_or_else(|e| {
        tracing::error!(error = %e, "the email letter did not render; sending the text part");
        Email {
            to: to.to_owned(),
            subject,
            text: letter.text(),
            html: None,
            unsubscribe: letter.unsubscribe.clone(),
        }
    })
}
