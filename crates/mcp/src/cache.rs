//! JSON audit cache on disk: one file per audit, reusing `codoseo_core::audit::Audit`
//! so a cached MCP audit and a `codoseo crawl --format json` file are interchangeable.

use std::path::PathBuf;

use codoseo_core::audit::Audit;

use crate::types::AuditId;

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("no cached audit with id \"{0}\"")]
    NotFound(String),
    #[error("could not read the audit cache: {0}")]
    Io(#[from] std::io::Error),
    #[error("cached audit \"{0}\" is unreadable: {1}")]
    Corrupt(String, codoseo_core::audit::AuditError),
}

pub struct AuditCache {
    dir: PathBuf,
}

impl AuditCache {
    pub fn new(dir: PathBuf) -> AuditCache {
        AuditCache { dir }
    }

    fn path(&self, id: &AuditId) -> PathBuf {
        self.dir.join(format!("{}.json", id.0))
    }

    pub fn save(&self, id: &AuditId, audit: &Audit) -> Result<(), CacheError> {
        std::fs::create_dir_all(&self.dir)?;
        let bytes = serde_json::to_vec(audit).expect("Audit always serialises");
        std::fs::write(self.path(id), bytes)?;
        Ok(())
    }

    pub fn load(&self, id: &AuditId) -> Result<Audit, CacheError> {
        let path = self.path(id);
        let bytes = std::fs::read(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                CacheError::NotFound(id.0.clone())
            } else {
                CacheError::Io(e)
            }
        })?;
        Audit::from_json(&bytes).map_err(|e| CacheError::Corrupt(id.0.clone(), e))
    }

    /// Every id with a valid cached file. Unreadable files are skipped, not errors.
    pub fn list(&self) -> Vec<AuditId> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_str()?.to_owned();
                let id = AuditId(name.strip_suffix(".json")?.to_owned());
                self.load(&id).ok().map(|_| id)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codoseo_core::audit::AUDIT_FORMAT_VERSION;
    use codoseo_core::output::StopReason;
    use codoseo_core::report::{CrawlReport, CrawlSummary};
    use codoseo_core::snapshot::Snapshot;
    use url::Url;

    fn sample_audit() -> Audit {
        Audit {
            format_version: AUDIT_FORMAT_VERSION,
            tool_version: "0.0.1".to_owned(),
            created_at: 1_700_000_000,
            duration_ms: 42,
            start_url: Url::parse("https://example.com/").unwrap(),
            report: CrawlReport {
                health_score: 100,
                checks_passed: 44,
                checks_total: 44,
                counts: Vec::new(),
                inlink_samples: Vec::new(),
                summary: CrawlSummary::default(),
            },
            snapshot: Snapshot {
                origin: Url::parse("https://example.com/").unwrap(),
                stop: StopReason::Completed,
                pages: Vec::new(),
                robots: None,
                sitemap: Default::default(),
            },
        }
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let cache = AuditCache::new(dir.path().to_owned());
        let id = AuditId::new();
        let audit = sample_audit();
        cache.save(&id, &audit).unwrap();
        let loaded = cache.load(&id).unwrap();
        assert_eq!(loaded, audit);
    }

    #[test]
    fn loading_a_missing_id_is_a_typed_not_found_error() {
        let dir = tempfile::tempdir().unwrap();
        let cache = AuditCache::new(dir.path().to_owned());
        let err = cache.load(&AuditId::new()).unwrap_err();
        assert!(matches!(err, CacheError::NotFound(_)));
    }

    #[test]
    fn a_corrupted_file_is_a_typed_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let cache = AuditCache::new(dir.path().to_owned());
        let id = AuditId::new();
        std::fs::write(dir.path().join(format!("{}.json", id.0)), b"not json").unwrap();
        let err = cache.load(&id).unwrap_err();
        assert!(matches!(err, CacheError::Corrupt(_, _)));
    }

    #[test]
    fn list_only_returns_valid_cached_ids() {
        let dir = tempfile::tempdir().unwrap();
        let cache = AuditCache::new(dir.path().to_owned());
        let good = AuditId::new();
        cache.save(&good, &sample_audit()).unwrap();
        std::fs::write(dir.path().join("garbage.json"), b"not json").unwrap();
        let ids = cache.list();
        assert_eq!(ids, vec![good]);
    }
}
