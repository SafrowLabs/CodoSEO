//! The file `codoseo crawl` saves: the report plus the snapshot to diff against later.

use serde::{Deserialize, Serialize};
use url::Url;

use crate::report::CrawlReport;
use crate::snapshot::Snapshot;

/// Bumped when the saved format changes in a way old readers can't handle.
pub const AUDIT_FORMAT_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error("not a valid audit file: {0}")]
    Json(#[from] serde_json::Error),
    #[error("audit file format version {0} is not supported (expected {AUDIT_FORMAT_VERSION})")]
    Version(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Audit {
    pub format_version: u32,
    pub tool_version: String,
    /// Unix seconds.
    pub created_at: u64,
    pub duration_ms: u64,
    pub start_url: Url,
    pub report: CrawlReport,
    pub snapshot: Snapshot,
}

impl Audit {
    /// Parses a saved audit, checking the format version before reading the rest.
    pub fn from_json(bytes: &[u8]) -> Result<Audit, AuditError> {
        #[derive(Deserialize)]
        struct Header {
            format_version: u32,
        }
        let header: Header = serde_json::from_slice(bytes)?;
        if header.format_version != AUDIT_FORMAT_VERSION {
            return Err(AuditError::Version(header.format_version));
        }
        Ok(serde_json::from_slice(bytes)?)
    }
}
