//! A difference between two crawls of the same site.

use serde::{Deserialize, Serialize};
use url::Url;

use crate::check::Severity;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    NewUrl,
    RemovedUrl,
    StatusChanged,
    BecameNoindex,
    TitleChanged,
    TitleRemoved,
    CanonicalChanged,
    RedirectChainGrew,
    RobotsTxtChanged,
    SitemapShrank,
    ErrorSpike,
    SiteMoved,
    /// AI search or answer bots the owner wants are kept out (robots.txt), or robots.txt fails.
    AiBotBlocked,
    /// Page markup (nosnippet, noarchive, ...) takes pages out of an engine's AI answers.
    AiAnswersRestricted,
    /// A bot the owner wants blocked can still crawl the site.
    AiBlockNotApplied,
    /// An AI access incident is gone.
    AiIssueResolved,
    /// The site's declared AI preferences (Content-Signal, Content-Usage, TDM) changed.
    AiPreferencesChanged,
}

impl ChangeKind {
    /// The stable machine name (`became_noindex`), the same as in the database and in webhooks.
    pub fn slug(self) -> &'static str {
        match self {
            ChangeKind::NewUrl => "new_url",
            ChangeKind::RemovedUrl => "removed_url",
            ChangeKind::StatusChanged => "status_changed",
            ChangeKind::BecameNoindex => "became_noindex",
            ChangeKind::TitleChanged => "title_changed",
            ChangeKind::TitleRemoved => "title_removed",
            ChangeKind::CanonicalChanged => "canonical_changed",
            ChangeKind::RedirectChainGrew => "redirect_chain_grew",
            ChangeKind::RobotsTxtChanged => "robots_txt_changed",
            ChangeKind::SitemapShrank => "sitemap_shrank",
            ChangeKind::ErrorSpike => "error_spike",
            ChangeKind::SiteMoved => "site_moved",
            ChangeKind::AiBotBlocked => "ai_bot_blocked",
            ChangeKind::AiAnswersRestricted => "ai_answers_restricted",
            ChangeKind::AiBlockNotApplied => "ai_block_not_applied",
            ChangeKind::AiIssueResolved => "ai_issue_resolved",
            ChangeKind::AiPreferencesChanged => "ai_preferences_changed",
        }
    }

    /// What a person reads in an alert or the settings grid.
    pub fn label(self) -> &'static str {
        match self {
            ChangeKind::NewUrl => "New URL",
            ChangeKind::RemovedUrl => "URL removed",
            ChangeKind::StatusChanged => "Status changed",
            ChangeKind::BecameNoindex => "Became noindex",
            ChangeKind::TitleChanged => "Title changed",
            ChangeKind::TitleRemoved => "Title removed",
            ChangeKind::CanonicalChanged => "Canonical changed",
            ChangeKind::RedirectChainGrew => "Redirect chain grew",
            ChangeKind::RobotsTxtChanged => "robots.txt changed",
            ChangeKind::SitemapShrank => "Sitemap shrank",
            ChangeKind::ErrorSpike => "Error spike (4xx/5xx)",
            ChangeKind::SiteMoved => "Site moved",
            ChangeKind::AiBotBlocked => "AI bot blocked",
            ChangeKind::AiAnswersRestricted => "AI answers restricted",
            ChangeKind::AiBlockNotApplied => "AI block not applied",
            ChangeKind::AiIssueResolved => "AI issue resolved",
            ChangeKind::AiPreferencesChanged => "AI preferences changed",
        }
    }

    /// The AI access kinds (findings of `codoseo-geo` and the declared preferences): they live on
    /// the AI access screen, not on a page of the crawl.
    pub fn is_ai(self) -> bool {
        matches!(
            self,
            ChangeKind::AiBotBlocked
                | ChangeKind::AiAnswersRestricted
                | ChangeKind::AiBlockNotApplied
                | ChangeKind::AiIssueResolved
                | ChangeKind::AiPreferencesChanged
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub kind: ChangeKind,
    pub severity: Severity,
    pub url: Option<Url>,
    pub before: String,
    pub after: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_match_the_serialized_names_and_labels_are_distinct() {
        let all = [
            ChangeKind::NewUrl,
            ChangeKind::RemovedUrl,
            ChangeKind::StatusChanged,
            ChangeKind::BecameNoindex,
            ChangeKind::TitleChanged,
            ChangeKind::TitleRemoved,
            ChangeKind::CanonicalChanged,
            ChangeKind::RedirectChainGrew,
            ChangeKind::RobotsTxtChanged,
            ChangeKind::SitemapShrank,
            ChangeKind::ErrorSpike,
            ChangeKind::SiteMoved,
            ChangeKind::AiBotBlocked,
            ChangeKind::AiAnswersRestricted,
            ChangeKind::AiBlockNotApplied,
            ChangeKind::AiIssueResolved,
            ChangeKind::AiPreferencesChanged,
        ];
        for kind in all {
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                serde_json::Value::String(kind.slug().to_owned())
            );
        }
        let labels: std::collections::HashSet<_> = all.iter().map(|k| k.label()).collect();
        assert_eq!(labels.len(), all.len());
        // The AI kinds are the `ai_` ones.
        for kind in all {
            assert_eq!(kind.is_ai(), kind.slug().starts_with("ai_"), "{kind:?}");
        }
    }
}
