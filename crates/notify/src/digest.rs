//! The weekly digest: one email per account, one block per monitored site. The caller (the
//! app's `send_digest` job) gathers the numbers into a [`DigestView`]; this module words them
//! and renders the HTML and plain-text parts.

use crate::email::Email;
use crate::message::AlertItem;
use askama::Template;

/// Changes of the week, counted by severity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeverityCounts {
    pub critical: u32,
    pub warning: u32,
    pub notice: u32,
}

impl SeverityCounts {
    pub fn total(&self) -> u32 {
        self.critical + self.warning + self.notice
    }

    /// "1 critical, 4 warnings, 11 notices" (severities with none are left out), or `None` when
    /// there were no changes at all.
    pub fn summary(&self) -> Option<String> {
        let parts: Vec<String> = [
            (self.critical, "critical", "critical"),
            (self.warning, "warning", "warnings"),
            (self.notice, "notice", "notices"),
        ]
        .into_iter()
        .filter(|(n, _, _)| *n > 0)
        .map(|(n, one, many)| format!("{n} {}", if n == 1 { one } else { many }))
        .collect();
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

/// One site's week.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteDigest {
    pub domain: String,
    pub score: u8,
    /// Against the crawl a week earlier; `None` in the first week, when there is nothing to
    /// compare with.
    pub score_delta: Option<i32>,
    pub checks_passed: u32,
    pub checks_total: u32,
    /// `(issue title, affected pages)` for checks failing now that did not fail a week ago.
    pub new_issues: Vec<(String, u32)>,
    /// The reverse: failing a week ago, passing now.
    pub resolved_issues: Vec<(String, u32)>,
    pub changes_by_severity: SeverityCounts,
    /// The most severe changes of the week (a handful, not all of them).
    pub top_changes: Vec<AlertItem>,
    pub dashboard_url: String,
    /// Cloud only: where RankOrg picks up from this audit.
    pub rankorg_url: Option<String>,
}

impl SiteDigest {
    /// "Your site passed 37/40 checks. 2 new issues."
    pub fn headline(&self) -> String {
        let passed = format!(
            "Your site passed {}/{} checks.",
            self.checks_passed, self.checks_total
        );
        if self.score_delta.is_none() {
            return passed;
        }
        match self.new_issues.len() {
            0 => format!("{passed} No new issues."),
            1 => format!("{passed} 1 new issue."),
            n => format!("{passed} {n} new issues."),
        }
    }

    /// "92 (+3)", "88 (-2)", "80 (no change)", or just "71" in the first week.
    pub fn score_label(&self) -> String {
        match self.score_delta {
            None => self.score.to_string(),
            Some(0) => format!("{} (no change)", self.score),
            Some(d) => format!("{} ({d:+})", self.score),
        }
    }

    /// "1 critical, 4 warnings, 11 notices", or "No changes this week."
    pub fn changes_line(&self) -> String {
        match self.changes_by_severity.summary() {
            Some(s) => format!("Changes this week: {s}."),
            None => "No changes this week.".to_owned(),
        }
    }

    /// Changes counted in the week beyond the ones listed.
    pub fn more_changes(&self) -> u32 {
        self.changes_by_severity
            .total()
            .saturating_sub(u32::try_from(self.top_changes.len()).unwrap_or(u32::MAX))
    }
}

/// The whole email.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestView {
    pub account_email: String,
    /// "Sep 28 to Oct 5, 2026".
    pub week_label: String,
    pub sites: Vec<SiteDigest>,
    /// Where the account turns alerts and the digest down.
    pub settings_url: String,
}

#[derive(Template)]
#[template(path = "digest.html")]
struct HtmlDigest<'a> {
    view: &'a DigestView,
}

#[derive(Template)]
#[template(path = "digest.txt")]
struct TextDigest<'a> {
    view: &'a DigestView,
}

impl DigestView {
    /// "CodoSEO weekly: example.com 92 (+3)" for one site, "CodoSEO weekly: 3 sites" for
    /// several.
    pub fn subject(&self) -> String {
        match self.sites.as_slice() {
            [one] => format!("CodoSEO weekly: {} {}", one.domain, one.score_label()),
            many => format!("CodoSEO weekly: {} sites", many.len()),
        }
    }

    /// The email, with both parts.
    pub fn render(&self) -> Result<Email, askama::Error> {
        Ok(Email {
            to: self.account_email.clone(),
            subject: self.subject(),
            text: TextDigest { view: self }.render()?,
            html: Some(HtmlDigest { view: self }.render()?),
        })
    }
}
