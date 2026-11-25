//! The weekly digest: one email per account, one section per monitored site. The caller (the
//! app's `send_digest` job) gathers the numbers into a [`DigestView`]; this module words them
//! and lays them out as a [`Letter`].

use crate::email::Email;
use crate::letter::{Button, Group, Letter, Line, Link, Mood, Section, Tone};
use crate::message::AlertItem;

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
    /// The one-click link that stops the digest, alerts and reminders to this address.
    pub unsubscribe_url: String,
    /// Where the email images are (`https://codoseo.com/email/`); empty renders the name as
    /// text.
    pub images: String,
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

    /// "example.com this week: 92 (+3)", or "3 sites this week".
    pub fn title(&self) -> String {
        match self.sites.as_slice() {
            [one] => format!("{} this week: {}", one.domain, one.score_label()),
            many => format!("{} sites this week", many.len()),
        }
    }

    /// The digest as a letter: one section per site.
    pub fn letter(&self) -> Letter {
        let pages = |n: &u32| format!("{n} page{}", if *n == 1 { "" } else { "s" });
        let issues = |list: &[(String, u32)], tone: Tone| -> Vec<Line> {
            list.iter()
                .map(|(title, n)| Line {
                    tone: Some(tone),
                    title: title.clone(),
                    detail: Some(pages(n)),
                    ..Line::default()
                })
                .collect()
        };
        let sections = self
            .sites
            .iter()
            .map(|site| {
                let mut groups = Vec::new();
                if !site.new_issues.is_empty() {
                    groups.push(Group {
                        label: "New issues".to_owned(),
                        tone: Tone::Critical,
                        items: issues(&site.new_issues, Tone::Critical),
                    });
                }
                if !site.resolved_issues.is_empty() {
                    groups.push(Group {
                        label: "Resolved".to_owned(),
                        tone: Tone::Good,
                        items: issues(&site.resolved_issues, Tone::Good),
                    });
                }
                if !site.top_changes.is_empty() {
                    groups.push(Group {
                        label: "Most important changes".to_owned(),
                        tone: Tone::Notice,
                        items: site
                            .top_changes
                            .iter()
                            .map(|c| Line {
                                tone: Some(Tone::from_severity(c.severity)),
                                title: c.kind_label.clone(),
                                detail: c.url.clone(),
                                before: (!c.before.is_empty()).then(|| c.before.clone()),
                                after: (!c.after.is_empty()).then(|| c.after.clone()),
                            })
                            .collect(),
                    });
                }
                Section {
                    heading: site.domain.clone(),
                    aside: Some(site.score_label()),
                    lines: vec![site.headline(), site.changes_line()],
                    groups,
                    more: (site.more_changes() > 0)
                        .then(|| format!("and {} more", site.more_changes())),
                    button: Some(Button {
                        label: "Open the dashboard".to_owned(),
                        url: site.dashboard_url.clone(),
                    }),
                    link: site.rankorg_url.as_ref().map(|url| Link {
                        label: "Turn these pages into rankings with RankOrg".to_owned(),
                        url: url.clone(),
                    }),
                }
            })
            .collect();
        Letter {
            images: self.images.clone(),
            mascot: Some(Mood::Rest),
            eyebrow: Some(format!("Weekly digest · {}", self.week_label)),
            title: self.title(),
            sections,
            footer: vec!["You get this email every Monday.".to_owned()],
            links: vec![
                Link {
                    label: "Change what you hear about".to_owned(),
                    url: self.settings_url.clone(),
                },
                Link {
                    label: "Unsubscribe".to_owned(),
                    url: self.unsubscribe_url.clone(),
                },
            ],
            unsubscribe: Some(self.unsubscribe_url.clone()),
            ..Letter::default()
        }
    }

    /// The email, with both parts.
    pub fn render(&self) -> Result<Email, askama::Error> {
        self.letter().email(&self.account_email, &self.subject())
    }
}
