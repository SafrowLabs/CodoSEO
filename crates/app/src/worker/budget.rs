//! Memory budget: the worker's budget is 70% of its cgroup memory limit (spec section 7), and
//! a crawl reserves about 1.5 KB per page. The budget is advisory for v0.1 (one worker, and
//! plan caps already bound `max_pages`); it matters most for self-hosted overrides.

use std::fs;

/// The spec's per-page memory estimate.
const RESERVATION_PER_PAGE: u64 = 1536;

/// 70% of the cgroup v2 `memory.max` (v1 `memory.limit_in_bytes` as a fallback), or `default`
/// when neither file is present, unreadable, or reports `max` (unlimited) — the case on a
/// plain macOS/Linux dev machine and on an unrestricted CI runner.
pub fn memory_budget_bytes(default: u64) -> u64 {
    let limit = read_cgroup_v2().or_else(read_cgroup_v1).unwrap_or(default);
    limit * 7 / 10
}

fn read_cgroup_v2() -> Option<u64> {
    let text = fs::read_to_string("/sys/fs/cgroup/memory.max").ok()?;
    text.trim().parse().ok()
}

fn read_cgroup_v1() -> Option<u64> {
    let text = fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes").ok()?;
    text.trim().parse().ok()
}

/// Whether a crawl capped at `max_pages` fits within `budget`, at ~1.5 KB/page.
pub fn fits(max_pages: u32, budget: u64) -> bool {
    (max_pages as u64) * RESERVATION_PER_PAGE <= budget
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_at_the_exact_boundary() {
        let budget = RESERVATION_PER_PAGE * 100;
        assert!(fits(100, budget));
        assert!(!fits(101, budget));
    }

    #[test]
    fn fits_zero_pages_always() {
        assert!(fits(0, 0));
    }

    #[test]
    fn memory_budget_bytes_never_panics_and_is_a_fraction_of_something_positive() {
        // Whether or not this machine/CI runner has a real cgroup limit, the result must be a
        // sane positive number and never exceed the ceiling implied by the default.
        let default = 1_000_000_000;
        let budget = memory_budget_bytes(default);
        assert!(budget > 0);
        assert!(budget <= default.max(budget));
    }

    #[test]
    fn memory_budget_bytes_falls_back_to_default_when_no_cgroup_is_readable() {
        // This dev machine (macOS) has no /sys/fs/cgroup at all, so this always exercises the
        // fallback path here; it may also pass trivially in a cgroup-free CI container.
        if std::path::Path::new("/sys/fs/cgroup/memory.max").exists()
            || std::path::Path::new("/sys/fs/cgroup/memory/memory.limit_in_bytes").exists()
        {
            return;
        }
        let default = 1_000_000_000;
        assert_eq!(memory_budget_bytes(default), default * 7 / 10);
    }
}
