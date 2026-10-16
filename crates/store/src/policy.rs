//! The site policy in the database: `sites.policy` (a [`SitePolicy`] as JSON) and its history.
//!
//! [`save`] is the one way a policy changes, from the policy screen, the API, an import or "Mark
//! intended": in one transaction it locks the site row, validates, writes the policy and the
//! `ai_intent`/`ai_probes` mirror (kept for one release, so a rollback reads the same intent),
//! re-evaluates the stored AI access reports quietly and rescores the newest finished crawl
//! (exactly what saving the intent did before, [`crate::geo::reevaluate_quietly`]), and records a
//! [`policy_history`](history) row with the scores before and after.
//!
//! A stored value is read leniently ([`SitePolicy::from_json_lenient`]), and a site whose
//! `policy` is NULL reads its mirror columns, so a policy is never unreadable.

use std::collections::{HashMap, HashSet};

use codoseo_core::check::IssueBits;
use codoseo_core::page::{PageFields, PageRecord};
use codoseo_core::plan::{Plan, PlanLimits};
use codoseo_core::report::InlinkSample;
use codoseo_geo::report::AccessReport;
use codoseo_policy::{PolicyError, PolicyInput, SitePolicy};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

/// Where a policy change came from (`policy_history.via`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Web,
    Api,
    Mcp,
    Import,
    Migration,
    MarkIntended,
}

impl Via {
    pub fn slug(self) -> &'static str {
        match self {
            Via::Web => "web",
            Via::Api => "api",
            Via::Mcp => "mcp",
            Via::Import => "import",
            Via::Migration => "migration",
            Via::MarkIntended => "mark_intended",
        }
    }
}

/// Who changed the policy, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Actor {
    pub via: Via,
    pub account_id: Option<Uuid>,
    pub api_key_id: Option<Uuid>,
}

impl Actor {
    pub fn web(account_id: Uuid) -> Actor {
        Actor::new(Via::Web, account_id)
    }

    pub fn new(via: Via, account_id: Uuid) -> Actor {
        Actor {
            via,
            account_id: Some(account_id),
            api_key_id: None,
        }
    }
}

/// Why a policy was not saved.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("{}", codoseo_policy::error_lines(.0))]
    Invalid(Vec<PolicyError>),
    #[error("no such site")]
    NotFound,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// What a save did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    /// The site had a stored AI access report to re-evaluate.
    pub had_report: bool,
    /// AI access incidents the new policy opened and resolved (quietly).
    pub opened: usize,
    pub resolved: usize,
    /// The history row, or `None` when nothing changed.
    pub history_id: Option<i64>,
    /// The newest finished crawl's health and GEO scores before and after.
    pub health: (Option<i16>, Option<i16>),
    pub geo: (Option<i16>, Option<i16>),
}

/// The policy from the site row's columns: `policy`, else the mirror (`ai_intent`, `ai_probes`).
pub(crate) fn from_columns(
    policy: Option<Value>,
    ai_intent: Option<Value>,
    ai_probes: Option<bool>,
) -> SitePolicy {
    match policy {
        Some(v) if !v.is_null() => SitePolicy::from_json_lenient(v),
        _ => {
            let mut p = SitePolicy::default();
            if let Some(intent) = ai_intent.and_then(|v| serde_json::from_value(v).ok()) {
                p.ai.intent = intent;
            }
            p.ai.probes = ai_probes.unwrap_or(true);
            p
        }
    }
}

type PolicyColumns = (Option<Value>, Option<Value>, Option<bool>);

/// The site's policy. A site that is gone reads as the defaults.
pub async fn get(pool: &PgPool, site_id: Uuid) -> Result<SitePolicy, sqlx::Error> {
    let row: Option<PolicyColumns> =
        sqlx::query_as("SELECT policy, ai_intent, ai_probes FROM sites WHERE id = $1")
            .bind(site_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map_or_else(SitePolicy::default, |(p, i, pr)| from_columns(p, i, pr)))
}

/// The site's policy and when it last changed (its newest history row; `None` when it never
/// has).
pub async fn get_with_time(
    pool: &PgPool,
    site_id: Uuid,
) -> Result<(SitePolicy, Option<OffsetDateTime>), sqlx::Error> {
    let policy = get(pool, site_id).await?;
    let at: Option<OffsetDateTime> =
        sqlx::query_scalar("SELECT max(created_at) FROM policy_history WHERE site_id = $1")
            .bind(site_id)
            .fetch_one(pool)
            .await?;
    Ok((policy, at))
}

/// Locks the site row and reads its policy. `FOR NO KEY UPDATE` conflicts with a policy save's
/// `UPDATE` and with the AI access writers, but not with rows that merely reference the site, so
/// crawls can still be queued meanwhile. `None` for a site that is gone.
pub(crate) async fn lock(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
) -> Result<Option<SitePolicy>, sqlx::Error> {
    let row: Option<PolicyColumns> = sqlx::query_as(
        "SELECT policy, ai_intent, ai_probes FROM sites WHERE id = $1 FOR NO KEY UPDATE",
    )
    .bind(site_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(|(p, i, pr)| from_columns(p, i, pr)))
}

/// Writes the policy and its mirror columns, in the caller's transaction (the row is locked).
pub(crate) async fn write(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
    policy: &SitePolicy,
) -> Result<(), sqlx::Error> {
    let intent = serde_json::to_value(&policy.ai.intent).expect("an intent always serializes");
    sqlx::query("UPDATE sites SET policy = $2, ai_intent = $3, ai_probes = $4 WHERE id = $1")
        .bind(site_id)
        .bind(policy.to_json())
        .bind(intent)
        .bind(policy.ai.probes)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// The newest finished crawl's id and its health and GEO scores, chosen like
/// `crawls::latest_done`.
async fn latest_scores(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
) -> Result<Option<(Uuid, Option<i16>, Option<i16>)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, health_score, geo_score FROM crawls WHERE site_id = $1 AND status = 'done' \
         ORDER BY finished_at DESC, created_at DESC, id DESC LIMIT 1",
    )
    .bind(site_id)
    .fetch_optional(&mut **tx)
    .await
}

/// Validates and saves the site's policy (refusing more must-work pages than the plan allows,
/// [`plan_errors`]), re-evaluates its AI access quietly, rescores the
/// newest finished crawl and records the change, all in one transaction. Saving the policy it
/// already has re-evaluates all the same but records no history row.
pub async fn save(
    pool: &PgPool,
    site_id: Uuid,
    policy: &SitePolicy,
    actor: &Actor,
) -> Result<Saved, SaveError> {
    policy.validate().map_err(SaveError::Invalid)?;
    // Stored exactly as it reads back.
    let policy = SitePolicy::from_json_lenient(policy.to_json());

    let mut tx = pool.begin().await?;
    let before = lock(&mut tx, site_id).await?.ok_or(SaveError::NotFound)?;
    let refused = plan_errors(site_plan(&mut *tx, site_id).await?, &policy);
    if !refused.is_empty() {
        return Err(SaveError::Invalid(refused));
    }
    let scores_before = latest_scores(&mut tx, site_id).await?;
    write(&mut tx, site_id, &policy).await?;
    let had_report: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM ai_reports WHERE site_id = $1)")
            .bind(site_id)
            .fetch_one(&mut *tx)
            .await?;
    let (opened, resolved) = crate::geo::reevaluate_in(&mut tx, site_id).await?;
    let scores_after = latest_scores(&mut tx, site_id).await?;
    let health = (
        scores_before.and_then(|s| s.1),
        scores_after.and_then(|s| s.1),
    );
    let geo = (
        scores_before.and_then(|s| s.2),
        scores_after.and_then(|s| s.2),
    );
    let history_id = if before == policy {
        None
    } else {
        Some(
            sqlx::query_scalar(
                "INSERT INTO policy_history (site_id, account_id, via, api_key_id, before, after, \
                   summary, crawl_id, health_before, health_after, geo_before, geo_after) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) RETURNING id",
            )
            .bind(site_id)
            .bind(actor.account_id)
            .bind(actor.via.slug())
            .bind(actor.api_key_id)
            .bind(before.to_json())
            .bind(policy.to_json())
            .bind(codoseo_policy::diff::summary(&before, &policy))
            .bind(scores_after.map(|s| s.0))
            .bind(health.0)
            .bind(health.1)
            .bind(geo.0)
            .bind(geo.1)
            .fetch_one(&mut *tx)
            .await?,
        )
    };
    tx.commit().await?;
    Ok(Saved {
        had_report,
        opened,
        resolved,
        history_id,
        health,
        geo,
    })
}

/// One change of a site's policy.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct HistoryRow {
    pub id: i64,
    /// `web`, `api`, `mcp`, `import`, `migration` or `mark_intended`.
    pub via: String,
    pub account_id: Option<Uuid>,
    /// Who made it, if the account is still there.
    pub email: Option<String>,
    pub api_key_id: Option<Uuid>,
    /// The key's name, if it is still there.
    pub api_key_name: Option<String>,
    pub summary: String,
    pub before: Option<Value>,
    pub after: Value,
    /// The crawl the save re-evaluated, and its place in the site's history (`#44`).
    pub crawl_id: Option<Uuid>,
    pub crawl_number: Option<i64>,
    pub health_before: Option<i16>,
    pub health_after: Option<i16>,
    pub geo_before: Option<i16>,
    pub geo_after: Option<i16>,
    pub created_at: OffsetDateTime,
}

/// The site's newest `limit` policy changes, newest first.
pub async fn history(
    pool: &PgPool,
    site_id: Uuid,
    limit: i64,
) -> Result<Vec<HistoryRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT h.id, h.via, h.account_id, a.email, h.api_key_id, k.name AS api_key_name, \
                h.summary, h.before, h.after, h.crawl_id, \
                CASE WHEN c.id IS NULL THEN NULL ELSE \
                  (SELECT count(*) FROM crawls c2 WHERE c2.site_id = h.site_id \
                     AND (c2.created_at, c2.id) <= (c.created_at, c.id)) END AS crawl_number, \
                h.health_before, h.health_after, h.geo_before, h.geo_after, h.created_at \
         FROM policy_history h \
         LEFT JOIN accounts a ON a.id = h.account_id \
         LEFT JOIN api_keys k ON k.id = h.api_key_id \
         LEFT JOIN crawls c ON c.id = h.crawl_id \
         WHERE h.site_id = $1 \
         ORDER BY h.created_at DESC, h.id DESC LIMIT $2",
    )
    .bind(site_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

// ---- plan limits -----------------------------------------------------------------------------

/// The plan of the site's account (`None` for an unclaimed site).
async fn site_plan<'e, E: sqlx::PgExecutor<'e>>(
    db: E,
    site_id: Uuid,
) -> Result<Option<Plan>, sqlx::Error> {
    let text: Option<String> = sqlx::query_scalar(
        "SELECT a.plan::text FROM sites s LEFT JOIN accounts a ON a.id = s.account_id \
         WHERE s.id = $1",
    )
    .bind(site_id)
    .fetch_optional(db)
    .await?
    .flatten();
    Ok(text.and_then(|t| serde_json::from_value(Value::String(t)).ok()))
}

/// What the plan refuses in `policy`: more must-work pages than [`PlanLimits::max_must_work`].
pub fn plan_errors(plan: Option<Plan>, policy: &SitePolicy) -> Vec<PolicyError> {
    let limits = plan.map_or_else(PlanLimits::quick_audit, PlanLimits::for_plan);
    match limits.max_must_work {
        Some(max) if policy.seo.must_work.len() > usize::from(max) => {
            let plan = match plan {
                Some(Plan::Free) => "the Free plan",
                Some(Plan::Lite) => "the Lite plan",
                Some(Plan::Pro) => "the Pro plan",
                Some(Plan::SelfHosted) => "this install",
                None => "a site without an account",
            };
            vec![PolicyError::new(
                "seo.must_work",
                format!(
                    "{plan} allows {max} must-work pages and this policy lists {}; remove {} or \
                     upgrade for more (the home page and starred pages don't count).",
                    policy.seo.must_work.len(),
                    policy.seo.must_work.len() - usize::from(max)
                ),
            )]
        }
        _ => Vec::new(),
    }
}

/// [`plan_errors`] for the site's plan, before a save: every reason the policy can't be saved
/// here, its own validation included.
pub async fn check(pool: &PgPool, site_id: Uuid, policy: &SitePolicy) -> Result<(), SaveError> {
    let mut errors = policy.validate().err().unwrap_or_default();
    errors.extend(plan_errors(site_plan(pool, site_id).await?, policy));
    if errors.is_empty() {
        Ok(())
    } else {
        Err(SaveError::Invalid(errors))
    }
}

// ---- the latest crawl, for evaluating a policy -----------------------------------------------

/// Most stored inlinks read for the preferred address rule's link count; past it the count
/// reads "at least".
const MAX_POLICY_INLINKS: i64 = 20_000;

/// The newest finished crawl as the policy rules read it: its pages (the fields the rules use),
/// the stored links to pages that redirect (as indexes into `pages`), the starred pages' URLs and
/// the site's origin.
#[derive(Debug, Clone)]
pub struct LatestCrawl {
    pub crawl_id: Uuid,
    /// Its place in the site's history (`#44`).
    pub number: i64,
    pub finished_at: Option<OffsetDateTime>,
    pub origin: Url,
    pub pages: Vec<PageRecord>,
    pub inlinks: Vec<InlinkSample>,
    pub inlinks_capped: bool,
    pub starred: HashSet<u64>,
    pub starred_urls: Vec<Url>,
}

impl LatestCrawl {
    /// The input [`codoseo_policy::evaluate`] takes, with the crawl's AI access report.
    pub fn input<'a>(&'a self, access: Option<&'a AccessReport>) -> PolicyInput<'a> {
        PolicyInput {
            pages: &self.pages,
            origin: Some(&self.origin),
            starred: Some(&self.starred),
            starred_urls: &self.starred_urls,
            inlinks: &self.inlinks,
            inlinks_capped: self.inlinks_capped,
            access,
        }
    }
}

/// The site's newest finished crawl that still has its pages (the latest two keep them), or
/// `None` before the first.
pub async fn latest_crawl(
    pool: &PgPool,
    site_id: Uuid,
) -> Result<Option<LatestCrawl>, sqlx::Error> {
    let row: Option<(Uuid, Option<OffsetDateTime>, String)> = sqlx::query_as(
        "SELECT c.id, c.finished_at, s.start_url FROM crawls c JOIN sites s ON s.id = c.site_id \
         WHERE c.site_id = $1 AND c.status = 'done' \
         ORDER BY c.finished_at DESC, c.created_at DESC, c.id DESC LIMIT 1",
    )
    .bind(site_id)
    .fetch_optional(pool)
    .await?;
    let Some((crawl_id, finished_at, start_url)) = row else {
        return Ok(None);
    };
    let number: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM crawls WHERE site_id = $1 \
           AND (created_at, id) <= (SELECT created_at, id FROM crawls WHERE id = $2)",
    )
    .bind(site_id)
    .bind(crawl_id)
    .fetch_one(pool)
    .await?;
    type Row = (
        String,
        i64,
        i16,
        Option<String>,
        bool,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT url, url_hash, status, content_type, in_sitemap, indexability::text, canonical, \
                meta_robots, x_robots_tag, redirect_target, issues \
         FROM pages WHERE crawl_id = $1 ORDER BY id",
    )
    .bind(crawl_id)
    .fetch_all(pool)
    .await?;
    let pages: Vec<PageRecord> = rows
        .into_iter()
        .filter_map(
            |(
                url,
                hash,
                status,
                content_type,
                in_sitemap,
                idx,
                canonical,
                robots,
                xrt,
                target,
                issues,
            )| {
                let url = Url::parse(&url).ok()?;
                Some(PageRecord {
                    url,
                    url_hash: crate::hash::from_db(hash),
                    status: status as u16,
                    redirect_chain: Vec::new(),
                    response_ms: 0,
                    size_bytes: 0,
                    content_type,
                    depth: None,
                    in_sitemap,
                    indexability: crate::crawl_queue::indexability_from_text(&idx),
                    fields: PageFields {
                        canonical: canonical.and_then(|c| Url::parse(&c).ok()),
                        meta_robots: robots,
                        x_robots_tag: xrt,
                        ..PageFields::default()
                    },
                    inlinks: 0,
                    outlinks_internal: 0,
                    outlinks_external: 0,
                    issues: IssueBits(crate::hash::from_db(issues)),
                    key_hash: 0,
                    redirect_target: target.and_then(|t| Url::parse(&t).ok()),
                    outlinks_nofollow: 0,
                    error: None,
                })
            },
        )
        .collect();
    let index: HashMap<u64, u32> = pages
        .iter()
        .enumerate()
        .map(|(i, p)| (p.url_hash, i as u32))
        .collect();
    let by_url: HashMap<&str, u32> = pages
        .iter()
        .enumerate()
        .map(|(i, p)| (p.url.as_str(), i as u32))
        .collect();
    // Only links to pages that redirect: those are the ones the address rule counts, and finalize
    // keeps every one of them.
    let redirecting: Vec<i64> = pages
        .iter()
        .filter(|p| (300..400).contains(&p.status))
        .map(|p| crate::hash::to_db(p.url_hash))
        .collect();
    let links: Vec<(i64, String)> = if redirecting.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as(
            "SELECT target_url_hash, from_url FROM inlinks \
             WHERE crawl_id = $1 AND target_url_hash = ANY($2) ORDER BY id LIMIT $3",
        )
        .bind(crawl_id)
        .bind(&redirecting)
        .bind(MAX_POLICY_INLINKS)
        .fetch_all(pool)
        .await?
    };
    let inlinks_capped = links.len() as i64 >= MAX_POLICY_INLINKS;
    let inlinks = links
        .into_iter()
        .filter_map(|(target, from)| {
            Some(InlinkSample {
                target: *index.get(&crate::hash::from_db(target))?,
                source: *by_url.get(from.as_str())?,
                anchor: String::new(),
                nofollow: false,
            })
        })
        .collect();
    let starred = crate::sites::starred_key_pages(pool, site_id).await?;
    let starred_urls = crate::sites::starred_urls(pool, site_id).await?;
    let origin = Url::parse(&start_url)
        .ok()
        .and_then(|u| u.join("/").ok())
        .or_else(|| pages.first().and_then(|p| p.url.join("/").ok()))
        .unwrap_or_else(|| Url::parse("https://invalid.example/").expect("a URL"));
    Ok(Some(LatestCrawl {
        crawl_id,
        number,
        finished_at,
        origin,
        pages,
        inlinks,
        inlinks_capped,
        starred,
        starred_urls,
    }))
}

/// `policy` against the site's latest crawl, SEO rules on its stored pages and AI rules on its
/// latest AI access report, and the crawl compared with (the newest finished crawl, else the AI
/// access report's).
pub async fn evaluate_latest(
    pool: &PgPool,
    site_id: Uuid,
    policy: &SitePolicy,
) -> Result<(codoseo_policy::PolicyReport, Option<Uuid>), sqlx::Error> {
    let crawl = latest_crawl(pool, site_id).await?;
    let ai = crate::geo::latest_report(pool, site_id).await?;
    let access = ai.as_ref().map(|l| &l.report);
    let report = match &crawl {
        Some(c) => codoseo_policy::evaluate(policy, &c.input(access)),
        None => codoseo_policy::evaluate(policy, &PolicyInput::ai_only(access)),
    };
    Ok((report, crawl.map(|c| c.crawl_id).or(ai.map(|l| l.crawl_id))))
}
