//! `/admin`: the funnel, the quick-audit queue and failed jobs, for the people running the
//! instance. In the cloud that is whoever is listed in `ADMIN_EMAILS`; self-hosted, the owner.
//! Everyone else gets the same 404 as a page that doesn't exist.

use askama::Template;
use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use codoseo_store::accounts::Account;
use codoseo_store::events::{self, EventKind, FunnelCount};
use codoseo_store::jobs::{self, FailedJob};
use codoseo_store::quick;

use crate::auth::{CurrentUser, email};
use crate::config::Mode;
use crate::error::AppError;
use crate::fmt;
use crate::layout::{Screen, Shell};
use crate::render::html;
use crate::state::AppState;

/// Failed jobs listed.
const FAILED_JOBS_SHOWN: i64 = 20;

pub fn routes() -> Router<AppState> {
    Router::new().route("/admin", get(page))
}

/// Whether `account` runs this instance.
pub fn is_admin(state: &AppState, account: &Account) -> bool {
    match state.config.mode {
        Mode::SelfHost => account.is_owner,
        Mode::Cloud => state
            .config
            .admin_emails
            .contains(&email::canonical(&account.email)),
    }
}

pub struct FunnelRow {
    /// The `events.kind` value.
    pub kind: &'static str,
    pub label: &'static str,
    pub unique: String,
    pub events: String,
    /// Share of the step before that got here, `66.7%`; `—` for the first step or no base.
    pub conversion: String,
    /// The raw unique count, for tests and scripts.
    pub raw: i64,
}

pub struct FunnelTable {
    pub title: &'static str,
    pub rows: Vec<FunnelRow>,
}

pub struct JobRow {
    pub kind: &'static str,
    pub attempts: i16,
    pub error: String,
    pub when: String,
}

#[derive(Template)]
#[template(path = "admin/index.html")]
pub struct AdminPage {
    pub shell: Shell,
    pub tables: Vec<FunnelTable>,
    pub queue_depth: i64,
    pub jobs: Vec<JobRow>,
}

fn label(kind: EventKind) -> &'static str {
    match kind {
        EventKind::AuditStarted => "Audit started",
        EventKind::AuditFinished => "Audit finished",
        EventKind::EmailGiven => "Email given",
        EventKind::LinkClicked => "Link clicked",
        EventKind::FirstFullCrawl => "First full crawl",
        EventKind::ActiveAfter4Weeks => "Active after 4 weeks",
        EventKind::RankorgClick => "RankOrg click",
    }
}

fn table(title: &'static str, counts: &[FunnelCount]) -> FunnelTable {
    let rows = counts
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let conversion = match i {
                0 => "—".to_owned(),
                // RankOrg clicks branch off the report, and the 4-week step needs the
                // scheduler (M7): neither is a step after the one before it yet.
                _ if matches!(
                    c.kind,
                    EventKind::RankorgClick | EventKind::ActiveAfter4Weeks
                ) =>
                {
                    "—".to_owned()
                }
                _ if counts[i - 1].unique == 0 => "—".to_owned(),
                _ => fmt::percent(c.unique, counts[i - 1].unique),
            };
            FunnelRow {
                kind: c.kind.as_str(),
                label: label(c.kind),
                unique: fmt::thousands(c.unique),
                events: fmt::thousands(c.events),
                conversion,
                raw: c.unique,
            }
        })
        .collect();
    FunnelTable { title, rows }
}

fn job_row(j: &FailedJob) -> JobRow {
    JobRow {
        kind: j.kind.slug(),
        attempts: j.attempt,
        error: j
            .last_error
            .clone()
            .unwrap_or_else(|| "no error recorded".to_owned()),
        when: fmt::ago(j.created_at),
    }
}

async fn page(State(state): State<AppState>, user: CurrentUser) -> Result<Response, AppError> {
    if !is_admin(&state, &user.account) {
        return Err(AppError::NotFound);
    }
    let shell = Shell::load(&state, &user, None, Screen::Admin).await?;
    let pool = &state.pool;
    let week = events::funnel_counts(pool, 7).await?;
    let month = events::funnel_counts(pool, 30).await?;
    let jobs = jobs::failed_jobs(pool, FAILED_JOBS_SHOWN).await?;
    Ok(html(&AdminPage {
        shell,
        tables: vec![table("Last 7 days", &week), table("Last 30 days", &month)],
        queue_depth: quick::queue_depth(pool).await?,
        jobs: jobs.iter().map(job_row).collect(),
    })?
    .into_response())
}
