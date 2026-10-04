//! The `Backend` trait: where an audit's data comes from. `LocalBackend` (in
//! [`crate::local`]) is the only implementation in v0.1; a future cloud backend
//! (Postgres-backed) implements the same trait so the tool layer in [`crate::tools`]
//! never has to know which one it's talking to.

use codoseo_core::change::Change;
use codoseo_core::check::CheckId;
use codoseo_core::page::PageRecord;
use url::Url;

use crate::types::{AuditHandle, AuditId, AuditState, RedirectReport, RobotsReport, UrlRow};

#[derive(Debug, Clone, thiserror::Error)]
pub enum BackendError {
    #[error("no audit with id \"{0}\"")]
    AuditNotFound(String),
    #[error("audit \"{0}\" has no page at that URL")]
    PageNotFound(String),
    #[error("could not fetch: {0}")]
    Fetch(String),
    #[error("{0}")]
    Other(String),
}

pub trait Backend: Send + Sync {
    /// Starts a crawl in the background and returns right away; the caller decides
    /// how long to wait before treating it as still running (see `audit_site` in
    /// [`crate::tools`]).
    fn audit(
        &self,
        url: Url,
        max_pages: u32,
    ) -> impl Future<Output = Result<AuditHandle, BackendError>> + Send;

    fn get_audit(
        &self,
        id: &AuditId,
    ) -> impl Future<Output = Result<AuditState, BackendError>> + Send;

    fn issue_urls(
        &self,
        id: &AuditId,
        check: CheckId,
        limit: u32,
        offset: u32,
    ) -> impl Future<Output = Result<Vec<UrlRow>, BackendError>> + Send;

    fn page(
        &self,
        id: &AuditId,
        url: &Url,
    ) -> impl Future<Output = Result<PageRecord, BackendError>> + Send;

    fn check_page(&self, url: Url)
    -> impl Future<Output = Result<PageRecord, BackendError>> + Send;

    fn check_robots(
        &self,
        url: Url,
        path: Option<String>,
    ) -> impl Future<Output = Result<RobotsReport, BackendError>> + Send;

    fn check_redirects(
        &self,
        url: Url,
    ) -> impl Future<Output = Result<RedirectReport, BackendError>> + Send;

    fn compare(
        &self,
        a: &AuditId,
        b: &AuditId,
    ) -> impl Future<Output = Result<Vec<Change>, BackendError>> + Send;
}
