//! Accounts and instance settings. An account is found by its canonical email (the web crate
//! computes it), so `Ana@Gmail.com` and `a.na+seo@gmail.com` are one account.

use codoseo_core::plan::Plan;
use sqlx::{FromRow, PgExecutor, PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::dbenum::enum_slug;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub id: Uuid,
    pub email: String,
    pub plan: Plan,
    pub is_owner: bool,
    pub github_id: Option<String>,
    pub created_at: OffsetDateTime,
}

#[derive(FromRow)]
struct AccountRow {
    id: Uuid,
    email: String,
    plan: String,
    is_owner: bool,
    github_id: Option<String>,
    created_at: OffsetDateTime,
}

impl From<AccountRow> for Account {
    fn from(r: AccountRow) -> Account {
        Account {
            id: r.id,
            email: r.email,
            plan: parse_plan(&r.plan),
            is_owner: r.is_owner,
            github_id: r.github_id,
            created_at: r.created_at,
        }
    }
}

pub(crate) fn parse_plan(slug: &str) -> Plan {
    serde_json::from_value(serde_json::Value::String(slug.to_owned())).unwrap_or(Plan::Free)
}

pub(crate) const ACCOUNT_COLUMNS: &str =
    "a.id, a.email, a.plan::text AS plan, a.is_owner, a.github_id, a.created_at";

/// Who is signing in, as far as account lookup is concerned.
#[derive(Debug, Clone)]
pub struct SignIn<'a> {
    /// The address as typed, used for sending.
    pub email: &'a str,
    /// The deduplication key (see the web crate's `auth::email::canonical`).
    pub canonical: &'a str,
    pub github_id: Option<&'a str>,
}

/// How new accounts are created.
#[derive(Debug, Clone, Copy)]
pub struct SignupPolicy {
    /// Self-hosted: new accounts get the self-hosted plan, the first becomes the owner, and the
    /// owner can close signups. Cloud: new accounts start on Free.
    pub self_hosted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignInOutcome {
    Existing(Account),
    Created(Account),
    /// No account matches and the owner has closed signups.
    SignupsClosed,
}

impl SignInOutcome {
    pub fn account(self) -> Option<Account> {
        match self {
            SignInOutcome::Existing(a) | SignInOutcome::Created(a) => Some(a),
            SignInOutcome::SignupsClosed => None,
        }
    }
}

pub async fn find(pool: &PgPool, id: Uuid) -> Result<Option<Account>, sqlx::Error> {
    let row: Option<AccountRow> = sqlx::query_as(&format!(
        "SELECT {ACCOUNT_COLUMNS} FROM accounts a WHERE a.id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(Account::from))
}

/// Finds the account for a sign-in (by GitHub id first, then canonical email), or creates it.
/// A GitHub sign-in that matches an existing email account links the GitHub id to it.
pub async fn sign_in(
    pool: &PgPool,
    who: &SignIn<'_>,
    policy: SignupPolicy,
) -> Result<SignInOutcome, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // Serialises account creation, so two simultaneous first signups can't both become owner
    // and two sign-ins with email variants can't race into two accounts.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('codoseo.accounts.sign_in'))")
        .execute(&mut *tx)
        .await?;

    if let Some(gh) = who.github_id
        && let Some(found) = find_where(&mut tx, "a.github_id = $1", gh).await?
    {
        tx.commit().await?;
        return Ok(SignInOutcome::Existing(found));
    }

    if let Some(found) = find_where(&mut tx, "a.email_canonical = $1", who.canonical).await? {
        let found = match who.github_id {
            Some(gh) if found.github_id.is_none() => {
                sqlx::query("UPDATE accounts SET github_id = $2 WHERE id = $1")
                    .bind(found.id)
                    .bind(gh)
                    .execute(&mut *tx)
                    .await?;
                Account {
                    github_id: Some(gh.to_owned()),
                    ..found
                }
            }
            _ => found,
        };
        tx.commit().await?;
        return Ok(SignInOutcome::Existing(found));
    }

    // An account from before `email_canonical` existed (or one that lost a backfill tie) is
    // still found by its address, and takes the key if it is free.
    let legacy: Option<AccountRow> = sqlx::query_as(&format!(
        "SELECT {ACCOUNT_COLUMNS} FROM accounts a WHERE lower(a.email) = lower($1) \
         ORDER BY a.created_at, a.id LIMIT 1"
    ))
    .bind(who.email)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(found) = legacy {
        sqlx::query(
            "UPDATE accounts SET email_canonical = $2 \
             WHERE id = $1 AND email_canonical IS NULL \
               AND NOT EXISTS (SELECT 1 FROM accounts WHERE email_canonical = $2)",
        )
        .bind(found.id)
        .bind(who.canonical)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(SignInOutcome::Existing(found.into()));
    }

    let first: bool = sqlx::query_scalar("SELECT NOT EXISTS (SELECT 1 FROM accounts)")
        .fetch_one(&mut *tx)
        .await?;
    if policy.self_hosted && !first && !signups_open_tx(&mut tx).await? {
        tx.commit().await?;
        return Ok(SignInOutcome::SignupsClosed);
    }

    let plan = if policy.self_hosted {
        Plan::SelfHosted
    } else {
        Plan::Free
    };
    let row: AccountRow = sqlx::query_as(
        "INSERT INTO accounts (email, email_canonical, github_id, plan, is_owner, last_login_at) \
         VALUES ($1, $2, $3, $4::plan, $5, now()) \
         RETURNING id, email, plan::text AS plan, is_owner, github_id, created_at",
    )
    .bind(who.email)
    .bind(who.canonical)
    .bind(who.github_id)
    .bind(enum_slug(&plan))
    .bind(policy.self_hosted && first)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(SignInOutcome::Created(row.into()))
}

async fn find_where(
    tx: &mut Transaction<'_, Postgres>,
    predicate: &str,
    value: &str,
) -> Result<Option<Account>, sqlx::Error> {
    let row: Option<AccountRow> = sqlx::query_as(&format!(
        "SELECT {ACCOUNT_COLUMNS} FROM accounts a WHERE {predicate}"
    ))
    .bind(value)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(Account::from))
}

/// Records a sign-in. Coming back clears a pending "Keep monitoring?" warning and lifts an
/// inactivity pause (Free accounts only; a paid account is never paused).
pub async fn touch_login(pool: &PgPool, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE accounts SET last_login_at = now(), keep_monitoring_sent_at = NULL, \
           paused = CASE WHEN plan = 'free' THEN false ELSE paused END \
         WHERE id = $1",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// A click on a link in one of our emails (magic link, resume link) counts as activity.
pub async fn record_email_click(pool: &PgPool, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE accounts SET last_email_click_at = now() WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// The "Keep monitoring?" link was used: monitoring is back on, and it counts as an email
/// click.
pub async fn resume_monitoring(executor: impl PgExecutor<'_>, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE accounts SET paused = false, last_email_click_at = now(), \
           keep_monitoring_sent_at = NULL WHERE id = $1",
    )
    .bind(id)
    .execute(executor)
    .await?;
    Ok(())
}

const SIGNUPS_OPEN: &str = "signups_open";

async fn signups_open_tx(tx: &mut Transaction<'_, Postgres>) -> Result<bool, sqlx::Error> {
    let v: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT value FROM instance_settings WHERE key = $1")
            .bind(SIGNUPS_OPEN)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(v.and_then(|v| v.as_bool()).unwrap_or(true))
}

/// Whether new accounts may be created (self-hosted; open until the owner closes them).
pub async fn signups_open(pool: &PgPool) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let open = signups_open_tx(&mut tx).await?;
    tx.commit().await?;
    Ok(open)
}

pub async fn set_signups_open(pool: &PgPool, open: bool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO instance_settings (key, value) VALUES ($1, $2) \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
    )
    .bind(SIGNUPS_OPEN)
    .bind(serde_json::Value::Bool(open))
    .execute(pool)
    .await?;
    Ok(())
}
