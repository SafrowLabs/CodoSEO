//! `LocalBackend`: runs crawls directly against the crawler/checks/diff crates, with
//! no database. Implemented in T3.3.

use crate::cache::AuditCache;

pub struct LocalBackend {
    #[allow(dead_code)]
    cache: AuditCache,
}
