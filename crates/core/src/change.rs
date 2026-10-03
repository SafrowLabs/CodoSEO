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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub kind: ChangeKind,
    pub severity: Severity,
    pub url: Option<Url>,
    pub before: String,
    pub after: String,
}
