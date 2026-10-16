//! The site policy in the database: `sites.policy` (a [`SitePolicy`] as JSON) and its history.
//!
//! [`save`] is the one way a policy changes, from the policy screen, the API, an import or "Mark
//! intended": in one transaction it locks the site row, validates, writes the policy and the
//! `ai_intent`/`ai_probes` mirror (kept for one release, so a rollback reads the same intent),
//! rewrites the newest finished crawl's page issue bits, counts and health score under the new
//! SEO rules, re-evaluates the stored AI access reports quietly and rescores the newest finished
//! crawl
//! (exactly what saving the intent did before, [`crate::geo::reevaluate_quietly`]), and records a
//! [`policy_history`](history) row with the scores before and after.
//!
//! A stored value is read leniently ([`SitePolicy::from_json_lenient`]), and a site whose
//! `policy` is NULL reads its mirror columns, so a policy is never unreadable.

use std::collections::{HashMap, HashSet};

use codoseo_core::check::{CheckId, IssueBits};
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
/// [`plan_errors`]), judges the newest finished crawl again when the SEO rules change (its pages' issue
/// bits, counts and health score, `rejudge_latest`), re-evaluates its AI access quietly, rescores
/// that crawl's GEO and Overall scores and records the change, all in one transaction. Older
/// crawls keep what they were judged under, so comparing the latest crawl with the one before
/// may show changes that come from the policy alone. Saving the policy it already has
/// re-evaluates all the same but records no history row.
pub async fn save(
    pool: &PgPool,
    site_id: Uuid,
    policy: &SitePolicy,
    actor: &Actor,
) -> Result<Saved, SaveError> {
    save_with_note(pool, site_id, policy, actor, None).await
}

/// Longest note a change may carry into the history.
pub const MAX_NOTE: usize = 200;

/// [`save`], with a note on why kept in the history row's summary (`set_policy`'s `note`).
pub async fn save_with_note(
    pool: &PgPool,
    site_id: Uuid,
    policy: &SitePolicy,
    actor: &Actor,
    note: Option<&str>,
) -> Result<Saved, SaveError> {
    let note = note.map(str::trim).filter(|n| !n.is_empty());
    if let Some(n) = note
        && (n.chars().count() > MAX_NOTE || n.chars().any(char::is_control))
    {
        return Err(SaveError::Invalid(vec![PolicyError::new(
            "note",
            format!("a note is at most {MAX_NOTE} characters on one line"),
        )]));
    }
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
    // The latest crawl judged again under the new SEO rules, before the AI rescore reads its
    // health score for the Overall score.
    // Only when the SEO rules changed: an AI-only save leaves the SEO side exactly as it was.
    if before.seo != policy.seo {
        rejudge_latest(&mut tx, site_id, &policy).await?;
    }
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
            .bind(match note {
                Some(n) => format!("{} — {n}", codoseo_policy::diff::summary(&before, &policy)),
                None => codoseo_policy::diff::summary(&before, &policy),
            })
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

/// The policy changes that judged one of `crawls` again, oldest first: the score trend pins them
/// on their crawl.
pub async fn history_for_crawls(
    pool: &PgPool,
    crawls: &[Uuid],
) -> Result<Vec<HistoryRow>, sqlx::Error> {
    if crawls.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as(
        "SELECT h.id, h.via, h.account_id, a.email, h.api_key_id, k.name AS api_key_name, \
                h.summary, h.before, h.after, h.crawl_id, NULL::bigint AS crawl_number, \
                h.health_before, h.health_after, h.geo_before, h.geo_after, h.created_at \
         FROM policy_history h \
         LEFT JOIN accounts a ON a.id = h.account_id \
         LEFT JOIN api_keys k ON k.id = h.api_key_id \
         WHERE h.crawl_id = ANY($1) \
         ORDER BY h.created_at, h.id",
    )
    .bind(crawls)
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

/// The newest finished crawl as the policy rules read it: its pages (the fields the rules and the
/// threshold checks use, with their stored issue bits), the stored links to pages that redirect
/// (as indexes into `pages`), its stored check counts, the starred pages' URLs and the site's
/// origin.
#[derive(Debug, Clone)]
pub struct LatestCrawl {
    pub crawl_id: Uuid,
    /// Its place in the site's history (`#44`).
    pub number: i64,
    pub finished_at: Option<OffsetDateTime>,
    pub origin: Url,
    pub pages: Vec<PageRecord>,
    /// `pages.id` of each page, in the same order.
    pub page_ids: Vec<i64>,
    pub inlinks: Vec<InlinkSample>,
    pub inlinks_capped: bool,
    pub starred: HashSet<u64>,
    pub starred_urls: Vec<Url>,
    /// `summary.counts` as stored (checks this version doesn't know are skipped).
    pub counts: Vec<(CheckId, u32)>,
    /// The crawl's health score as stored.
    pub health_score: Option<i16>,
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
    let mut conn = pool.acquire().await?;
    latest_crawl_in(&mut conn, site_id).await
}

/// One stored page as the policy reads it back.
#[derive(sqlx::FromRow)]
struct PageRow {
    id: i64,
    url: String,
    url_hash: i64,
    status: i16,
    redirect_chain: Value,
    response_ms: Option<i32>,
    content_type: Option<String>,
    in_sitemap: bool,
    indexability: String,
    title: Option<String>,
    meta_description: Option<String>,
    word_count: Option<i32>,
    images_missing_alt: Option<i32>,
    canonical: Option<String>,
    meta_robots: Option<String>,
    x_robots_tag: Option<String>,
    redirect_target: Option<String>,
    issues: i64,
}

impl PageRow {
    fn record(self) -> Option<(i64, PageRecord)> {
        let url = Url::parse(&self.url).ok()?;
        Some((
            self.id,
            PageRecord {
                url,
                url_hash: crate::hash::from_db(self.url_hash),
                status: self.status as u16,
                redirect_chain: serde_json::from_value(self.redirect_chain).unwrap_or_default(),
                response_ms: self.response_ms.unwrap_or(0).max(0) as u32,
                size_bytes: 0,
                content_type: self.content_type,
                depth: None,
                in_sitemap: self.in_sitemap,
                indexability: crate::crawl_queue::indexability_from_text(&self.indexability),
                fields: PageFields {
                    title: self.title,
                    meta_description: self.meta_description,
                    word_count: self.word_count.unwrap_or(0).max(0) as u32,
                    images_missing_alt: self.images_missing_alt.unwrap_or(0).max(0) as u32,
                    canonical: self.canonical.and_then(|c| Url::parse(&c).ok()),
                    meta_robots: self.meta_robots,
                    x_robots_tag: self.x_robots_tag,
                    ..PageFields::default()
                },
                inlinks: 0,
                outlinks_internal: 0,
                outlinks_external: 0,
                issues: IssueBits(crate::hash::from_db(self.issues)),
                key_hash: 0,
                redirect_target: self.redirect_target.and_then(|t| Url::parse(&t).ok()),
                outlinks_nofollow: 0,
                error: None,
            },
        ))
    }
}

/// [`latest_crawl`] on the caller's connection (a policy save's transaction).
pub(crate) async fn latest_crawl_in(
    conn: &mut sqlx::PgConnection,
    site_id: Uuid,
) -> Result<Option<LatestCrawl>, sqlx::Error> {
    type Head = (
        Uuid,
        Option<OffsetDateTime>,
        String,
        Option<i16>,
        Option<Value>,
    );
    let row: Option<Head> = sqlx::query_as(
        "SELECT c.id, c.finished_at, s.start_url, c.health_score, c.summary \
         FROM crawls c JOIN sites s ON s.id = c.site_id \
         WHERE c.site_id = $1 AND c.status = 'done' \
         ORDER BY c.finished_at DESC, c.created_at DESC, c.id DESC LIMIT 1",
    )
    .bind(site_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((crawl_id, finished_at, start_url, health_score, summary)) = row else {
        return Ok(None);
    };
    let counts = summary
        .as_ref()
        .and_then(|s| s.get("counts"))
        .and_then(|c| serde_json::from_value::<Vec<(String, u32)>>(c.clone()).ok())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(slug, n)| Some((CheckId::from_slug(&slug)?, n)))
        .collect();
    let number: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM crawls WHERE site_id = $1 \
           AND (created_at, id) <= (SELECT created_at, id FROM crawls WHERE id = $2)",
    )
    .bind(site_id)
    .bind(crawl_id)
    .fetch_one(&mut *conn)
    .await?;
    let rows: Vec<PageRow> = sqlx::query_as(
        "SELECT id, url, url_hash, status, redirect_chain, response_ms, content_type, in_sitemap, \
                indexability::text AS indexability, title, meta_description, word_count, \
                images_missing_alt, canonical, meta_robots, x_robots_tag, redirect_target, issues \
         FROM pages WHERE crawl_id = $1 ORDER BY id",
    )
    .bind(crawl_id)
    .fetch_all(&mut *conn)
    .await?;
    let (page_ids, pages): (Vec<i64>, Vec<PageRecord>) =
        rows.into_iter().filter_map(PageRow::record).unzip();
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
        .fetch_all(&mut *conn)
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
    let starred = crate::sites::starred_key_pages(&mut *conn, site_id).await?;
    let starred_urls = crate::sites::starred_urls(&mut *conn, site_id).await?;
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
        page_ids,
        inlinks,
        inlinks_capped,
        starred,
        starred_urls,
        counts,
        health_score,
    }))
}

/// Pages updated per statement when a save rewrites the latest crawl's issue bits.
const UPDATE_BATCH: usize = 10_000;

/// After a policy change, the newest finished crawl as the new policy judges it: the threshold
/// and policy check bits of its pages set again ([`codoseo_policy::recheck`]; the bits of checks
/// that need the whole crawl are kept as stored), its counts, excluded counts, checks and health
/// score, and its `summary.policy`, all rewritten in the caller's transaction. Older crawls keep
/// what they were judged under. Returns the crawl, or `None` when the site has none with pages.
async fn rejudge_latest(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
    policy: &SitePolicy,
) -> Result<Option<Uuid>, sqlx::Error> {
    let Some(mut crawl) = latest_crawl_in(tx, site_id).await? else {
        return Ok(None);
    };
    // A crawl whose pages are gone keeps its numbers: there is nothing to judge again.
    if crawl.pages.is_empty() {
        return Ok(None);
    }
    let bits = codoseo_policy::recheck(policy, &crawl.input(None));
    let mut ids = Vec::new();
    let mut values = Vec::new();
    for ((id, page), b) in crawl.page_ids.iter().zip(&crawl.pages).zip(&bits) {
        if page.issues != *b {
            ids.push(*id);
            values.push(crate::hash::to_db(b.0));
        }
    }
    for (ids, values) in ids.chunks(UPDATE_BATCH).zip(values.chunks(UPDATE_BATCH)) {
        sqlx::query(
            "UPDATE pages SET issues = v.issues \
             FROM unnest($2::bigint[], $3::bigint[]) AS v(id, issues) \
             WHERE pages.crawl_id = $1 AND pages.id = v.id",
        )
        .bind(crawl.crawl_id)
        .bind(ids)
        .bind(values)
        .execute(&mut **tx)
        .await?;
    }
    let scored = codoseo_policy::rescore_stored(policy, &crawl.pages, &bits, &crawl.counts);
    for (page, b) in crawl.pages.iter_mut().zip(&bits) {
        page.issues = *b;
    }
    let access = crate::geo::latest_report_in(&mut **tx, site_id).await?;
    let report = codoseo_policy::evaluate(policy, &crawl.input(access.as_ref().map(|l| &l.report)));
    let counts = serde_json::to_value(&scored.counts).expect("counts always serialize");
    let excluded = (!scored.excluded.is_empty())
        .then(|| serde_json::to_value(&scored.excluded).expect("counts always serialize"));
    let policy_report = serde_json::to_value(&report).expect("a policy report always serializes");
    sqlx::query(
        "UPDATE crawls SET health_score = $2, checks_passed = $3, checks_total = $4, \
           summary = CASE WHEN $6::jsonb IS NULL THEN coalesce(summary, '{}'::jsonb) - 'excluded' \
                          ELSE jsonb_set(coalesce(summary, '{}'::jsonb), '{excluded}', $6::jsonb) END \
                     || jsonb_build_object('counts', $5::jsonb, 'policy', $7::jsonb) \
         WHERE id = $1",
    )
    .bind(crawl.crawl_id)
    .bind(i16::from(scored.health_score))
    .bind(scored.checks_passed as i16)
    .bind(scored.checks_total as i16)
    .bind(counts)
    .bind(excluded)
    .bind(policy_report)
    .execute(&mut **tx)
    .await?;
    Ok(Some(crawl.crawl_id))
}

/// A policy measured against the site's latest crawl next to the stored one: what the policy
/// screen's rail, `preview_policy` and `compare_policy` show.
#[derive(Debug, Clone)]
pub struct Measured {
    /// The stored policy.
    pub current: SitePolicy,
    /// The crawl compared with: the newest finished one, else the AI access report's.
    pub crawl_id: Option<Uuid>,
    /// The stored policy's rules on that crawl, and the other policy's.
    pub before: codoseo_policy::PolicyReport,
    pub after: codoseo_policy::PolicyReport,
    /// What saving the other policy would do to the newest finished crawl; `None` before it.
    pub preview: Option<codoseo_policy::SavePreview>,
}

/// `policy` measured against the site's latest crawl, next to the stored policy. Nothing is
/// written.
pub async fn measure(
    pool: &PgPool,
    site_id: Uuid,
    policy: &SitePolicy,
) -> Result<Measured, sqlx::Error> {
    let current = get(pool, site_id).await?;
    let crawl = latest_crawl(pool, site_id).await?;
    let ai = crate::geo::latest_report(pool, site_id).await?;
    let access = ai.as_ref().map(|l| &l.report);
    let input = match &crawl {
        Some(c) => c.input(access),
        None => PolicyInput::ai_only(access),
    };
    let before = codoseo_policy::evaluate(&current, &input);
    let after = codoseo_policy::evaluate(policy, &input);
    let preview = crawl
        .as_ref()
        .map(|c| codoseo_policy::preview(&current, policy, &input, &c.counts));
    Ok(Measured {
        crawl_id: crawl
            .as_ref()
            .map(|c| c.crawl_id)
            .or(ai.map(|l| l.crawl_id)),
        current,
        before,
        after,
        preview,
    })
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
