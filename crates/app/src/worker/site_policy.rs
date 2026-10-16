//! The site policy in a crawl: the must-work and starred pages it seeds, the policy pass after the
//! checks, the `must_work_failed` changes and the report kept in `crawls.summary.policy`.

use std::collections::{HashMap, HashSet};

use codoseo_core::change::{Change, ChangeKind};
use codoseo_core::check::Severity;
use codoseo_core::output::CrawlOutput;
use codoseo_core::report::CrawlReport;
use codoseo_core::snapshot::Snapshot;
use codoseo_geo::report::AccessReport;
use codoseo_policy::seo::must_work_rows;
use codoseo_policy::{PolicyInput, PolicyReport, SitePolicy, Status, apply_to_crawl, evaluate};
use sqlx::PgPool;
use url::Url;
use uuid::Uuid;

/// The policy a crawl runs under, with the starred pages it adds to the must-work list. The
/// default (a no-signup audit) declares no SEO rules and stars nothing.
#[derive(Debug, Clone, Default)]
pub struct SitePolicyRun {
    pub policy: SitePolicy,
    pub starred: HashSet<u64>,
    pub starred_urls: Vec<Url>,
}

/// The site's policy and starred pages.
pub async fn load(pool: &PgPool, site_id: Uuid) -> Result<SitePolicyRun, String> {
    let policy = codoseo_store::policy::get(pool, site_id)
        .await
        .map_err(|e| format!("could not load the site policy: {e}"))?;
    let starred = codoseo_store::sites::starred_key_pages(pool, site_id)
        .await
        .map_err(|e| format!("could not load starred pages: {e}"))?;
    let starred_urls = if starred.is_empty() {
        Vec::new()
    } else {
        codoseo_store::sites::starred_urls(pool, site_id)
            .await
            .map_err(|e| format!("could not load starred pages: {e}"))?
    };
    Ok(SitePolicyRun {
        policy,
        starred,
        starred_urls,
    })
}

impl SitePolicyRun {
    /// The pages fetched even when no link leads there: the must-work paths and the starred
    /// pages, when the policy has must-work pages at all.
    pub fn extra_seeds(&self, start: &Url) -> Vec<Url> {
        if self.policy.seo.must_work.is_empty() {
            return Vec::new();
        }
        self.policy
            .seo
            .must_work
            .iter()
            .filter_map(|p| start.join(p).ok())
            .chain(self.starred_urls.iter().cloned())
            .collect()
    }

    fn input<'a>(
        &'a self,
        out: &'a CrawlOutput,
        report: &'a CrawlReport,
        access: Option<&'a AccessReport>,
    ) -> PolicyInput<'a> {
        PolicyInput {
            pages: &out.pages,
            origin: Some(&out.origin),
            starred: Some(&self.starred),
            starred_urls: &self.starred_urls,
            inlinks: &report.inlink_samples,
            inlinks_capped: false,
            access,
        }
    }

    /// The policy pass after `run_checks`.
    pub fn apply(&self, out: &mut CrawlOutput, report: &mut CrawlReport) {
        apply_to_crawl(&self.policy, out, report, &self.starred, &self.starred_urls);
    }

    /// The policy against this crawl, SEO and AI rules.
    pub fn report(
        &self,
        out: &CrawlOutput,
        report: &CrawlReport,
        access: Option<&AccessReport>,
    ) -> PolicyReport {
        evaluate(&self.policy, &self.input(out, report, access))
    }

    /// One `must_work_failed` change per must-work page that failed on this crawl, whether it
    /// just started failing or keeps failing; `before` is how it fared on the previous crawl.
    pub fn must_work_changes(
        &self,
        out: &CrawlOutput,
        report: &CrawlReport,
        previous: Option<&Snapshot>,
    ) -> Vec<Change> {
        if self.policy.seo.must_work.is_empty() {
            return Vec::new();
        }
        let before: HashMap<String, String> = previous
            .map(|prev| {
                let input = PolicyInput {
                    pages: &prev.pages,
                    origin: Some(&prev.origin),
                    starred: Some(&self.starred),
                    starred_urls: &self.starred_urls,
                    ..PolicyInput::default()
                };
                must_work_rows(&self.policy.seo, &input)
                    .into_iter()
                    .filter(|r| r.status != Status::NotChecked)
                    .map(|r| (r.url.as_str().to_owned(), r.state))
                    .collect()
            })
            .unwrap_or_default();
        must_work_rows(&self.policy.seo, &self.input(out, report, None))
            .into_iter()
            .filter(|r| r.status == Status::Gaps)
            .map(|r| Change {
                kind: ChangeKind::MustWorkFailed,
                severity: Severity::Critical,
                before: before.get(r.url.as_str()).cloned().unwrap_or_default(),
                after: r.state,
                url: Some(r.url),
            })
            .collect()
    }
}
