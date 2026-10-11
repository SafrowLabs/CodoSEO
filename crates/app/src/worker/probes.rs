//! Synthetic AI-agent fetch probes after a monitored crawl: their settings (`CODOSEO_PROBE_FROM`,
//! `CODOSEO_PROBE_URLS`) and the run. A probe run never fails the crawl: whatever goes wrong is
//! logged, and the report then keeps no probes.

use std::time::Duration;

use codoseo_core::crawl::{AddressPolicy, PROBE_FROM};
use codoseo_core::output::CrawlOutput;
use codoseo_core::plan::{Plan, PlanLimits};
use codoseo_crawler::probe::{DEFAULT_MIN_GAP, PROBE_TIMEOUT, ProbeConfig, run_probes};
use codoseo_geo::probe::{ProbeReport, plan_probes};
use codoseo_geo::report::AccessReport;
use codoseo_geo::robots::RobotsTxt;
use codoseo_web::metrics;
use tracing::Instrument;

/// The most pages `CODOSEO_PROBE_URLS` may ask for.
pub const MAX_PROBE_URLS: u8 = 10;
/// How long past its budget a probe run may go before it is abandoned. The executor checks the
/// budget between pages only, so the last page's requests (each with its own timeout and maybe a
/// retry) can run over.
const OVERRUN: Duration = Duration::from_secs(60);

/// How the worker probes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeSettings {
    /// The `From` header on every probe and baseline: `CODOSEO_PROBE_FROM`, else [`PROBE_FROM`].
    pub from: String,
    /// `CODOSEO_PROBE_URLS`: the pages probed per crawl for the `self_hosted` plan, at most
    /// [`MAX_PROBE_URLS`]. `None` keeps the plan's own number. Other plans ignore it.
    pub self_hosted_urls: Option<u8>,
    /// The shortest gap between two probe requests. Tests shorten it.
    pub min_gap: Duration,
    /// Per-request timeout. Tests shorten it.
    pub request_timeout: Duration,
}

impl Default for ProbeSettings {
    fn default() -> ProbeSettings {
        ProbeSettings {
            from: PROBE_FROM.to_owned(),
            self_hosted_urls: None,
            min_gap: DEFAULT_MIN_GAP,
            request_timeout: PROBE_TIMEOUT,
        }
    }
}

impl ProbeSettings {
    /// The settings from `CODOSEO_PROBE_FROM` and `CODOSEO_PROBE_URLS`.
    pub fn from_env() -> ProbeSettings {
        ProbeSettings::from_vars(
            std::env::var("CODOSEO_PROBE_FROM").ok().as_deref(),
            std::env::var("CODOSEO_PROBE_URLS").ok().as_deref(),
        )
    }

    /// The settings for these variable values. An empty or invalid value is logged and ignored;
    /// a page count above [`MAX_PROBE_URLS`] is clamped to it.
    pub fn from_vars(from: Option<&str>, urls: Option<&str>) -> ProbeSettings {
        let mut settings = ProbeSettings::default();
        if let Some(from) = from.map(str::trim).filter(|f| !f.is_empty()) {
            // A header value: visible ASCII and spaces only.
            if from.bytes().all(|b| (0x20..0x7f).contains(&b)) {
                settings.from = from.to_owned();
            } else {
                tracing::warn!(
                    "CODOSEO_PROBE_FROM is not a valid header value; using {PROBE_FROM}"
                );
            }
        }
        if let Some(urls) = urls.map(str::trim).filter(|u| !u.is_empty()) {
            match urls.parse::<u32>() {
                Ok(n) => {
                    if n > u32::from(MAX_PROBE_URLS) {
                        tracing::warn!(
                            requested = n,
                            "CODOSEO_PROBE_URLS is above {MAX_PROBE_URLS}; probing {MAX_PROBE_URLS} pages"
                        );
                    }
                    settings.self_hosted_urls = Some(n.min(u32::from(MAX_PROBE_URLS)) as u8);
                }
                Err(_) => tracing::warn!(
                    value = urls,
                    "CODOSEO_PROBE_URLS is not a whole number; using the plan's page count"
                ),
            }
        }
        settings
    }

    /// The pages to probe for a site on `plan` (`None`: a no-signup audit) with `limits`.
    pub fn probe_urls(&self, plan: Option<Plan>, limits: &PlanLimits) -> u8 {
        match (plan, self.self_hosted_urls) {
            (Some(Plan::SelfHosted), Some(n)) => n.min(MAX_PROBE_URLS),
            _ => limits.probe_urls,
        }
    }
}

/// Probes up to `urls` important pages of `report` (built from `out`) within the budget and
/// request cap of `limits`. `None` when there is nothing to probe (no robots verdicts, no HTML 200
/// page), or when the run panicked or overran its budget by more than a minute.
pub async fn probe(
    report: &AccessReport,
    out: &CrawlOutput,
    urls: u8,
    limits: &PlanLimits,
    settings: &ProbeSettings,
    policy: AddressPolicy,
) -> Option<ProbeReport> {
    // Read only for the crawl delay; the robots verdicts are already in the report.
    let robots = out
        .robots
        .as_ref()
        .and_then(|r| RobotsTxt::from_response(Some(r.status), r.body.as_bytes()));
    let plan = plan_probes(report, robots.as_ref(), usize::from(urls))?;
    let cfg = ProbeConfig {
        from: settings.from.clone(),
        min_gap: settings.min_gap,
        request_timeout: settings.request_timeout,
        ..ProbeConfig::new(
            report,
            plan,
            policy,
            limits.probe_budget,
            limits.probe_max_requests,
        )
    };
    // Its own task, so a panic in the executor is caught here instead of failing the crawl.
    let mut run = tokio::spawn(run_probes(cfg).in_current_span());
    match tokio::time::timeout(limits.probe_budget + OVERRUN, &mut run).await {
        Ok(Ok(probes)) => {
            metrics::probe_requests(u64::from(probes.requests));
            tracing::info!(
                requests = probes.requests,
                urls = probes.urls.len(),
                stop = ?probes.stop,
                "synthetic probes finished"
            );
            Some(probes)
        }
        Ok(Err(e)) => {
            tracing::error!(error = %e, "synthetic probe run panicked; the report keeps no probes");
            None
        }
        Err(_elapsed) => {
            run.abort();
            tracing::warn!("synthetic probe run overran its budget; the report keeps no probes");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_defaults_to_the_codoseo_mailbox_and_can_be_overridden() {
        assert_eq!(
            ProbeSettings::from_vars(None, None),
            ProbeSettings::default()
        );
        assert_eq!(ProbeSettings::default().from, PROBE_FROM);
        assert_eq!(
            ProbeSettings::from_vars(Some(" ops@example.org "), None).from,
            "ops@example.org"
        );
        assert_eq!(ProbeSettings::from_vars(Some(""), None).from, PROBE_FROM);
        assert_eq!(
            ProbeSettings::from_vars(Some("bad\nvalue"), None).from,
            PROBE_FROM
        );
    }

    #[test]
    fn probe_urls_apply_to_self_hosted_only_and_are_clamped_to_ten() {
        let urls = |v: &str| ProbeSettings::from_vars(None, Some(v)).self_hosted_urls;
        assert_eq!(urls("3"), Some(3));
        assert_eq!(urls("0"), Some(0));
        assert_eq!(urls("10"), Some(10));
        assert_eq!(urls("50"), Some(10));
        assert_eq!(urls("many"), None);
        assert_eq!(urls("-1"), None);
        assert_eq!(urls(""), None);

        let eight = ProbeSettings::from_vars(None, Some("8"));
        let self_hosted = PlanLimits::for_plan(Plan::SelfHosted);
        assert_eq!(eight.probe_urls(Some(Plan::SelfHosted), &self_hosted), 8);
        assert_eq!(
            ProbeSettings::default().probe_urls(Some(Plan::SelfHosted), &self_hosted),
            5
        );
        for plan in [Plan::Free, Plan::Pro, Plan::Agency] {
            let limits = PlanLimits::for_plan(plan);
            assert_eq!(eight.probe_urls(Some(plan), &limits), limits.probe_urls);
        }
        assert_eq!(eight.probe_urls(None, &PlanLimits::quick_audit()), 0);
    }
}
