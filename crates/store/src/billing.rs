//! Dodo Payments webhooks applied to accounts: which plan an account is on, until when, and
//! what happens to its sites when that changes.
//!
//! Every delivery is recorded under its `webhook-id` and applied in the same transaction, so a
//! replay changes nothing and a failed apply leaves no record (the retry is processed). Events
//! can arrive out of order, so one older than the last applied event is ignored.

use std::collections::HashSet;

use codoseo_core::plan::{Plan, PlanLimits};
use sqlx::{FromRow, PgConnection, PgPool};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::accounts::parse_plan;
use crate::plans::apply_plan_limits;

/// How long a plan outlives the billing date it was paid up to, so a late renewal webhook
/// doesn't cut a paying customer off.
pub const GRACE: Duration = Duration::days(3);

/// Used when a paid-up event carries no next billing date: one billing month, which the next
/// renewal event corrects.
const FALLBACK_PERIOD: Duration = Duration::days(31);

/// A subscription webhook as the store needs it: the web side has already verified the
/// signature, parsed the envelope and mapped the product to a plan.
#[derive(Debug, Clone)]
pub struct SubscriptionEvent {
    /// The `webhook-id`: stable across Dodo's retries.
    pub event_id: String,
    /// The event type, e.g. `subscription.active`.
    pub kind: String,
    /// When the event happened (the envelope's `timestamp`), the ordering key.
    pub at: OffsetDateTime,
    /// The raw body, kept for support.
    pub payload: serde_json::Value,
    /// `metadata.account_id`, set when the checkout was created.
    pub account_hint: Option<Uuid>,
    pub subscription_id: Option<String>,
    pub customer_id: Option<String>,
    /// The customer's email in canonical form.
    pub email_canonical: Option<String>,
    /// The plan the subscription's product maps to; `None` for a product we don't sell.
    pub plan: Option<Plan>,
    pub status: Option<String>,
    pub next_billing_date: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The same `webhook-id` was already processed.
    Duplicate,
    /// Recorded, but no account matches.
    NoAccount,
    /// Older than the last event applied to the account.
    Stale,
    /// The account is on this plan now (paid until its new expiry).
    Applied(Plan),
    /// The account dropped to Free.
    Downgraded,
    /// Recorded and understood, and nothing needed to change.
    Unchanged(&'static str),
}

/// What the billing screen shows.
#[derive(Debug, Clone, FromRow)]
pub struct AccountBilling {
    pub customer_id: Option<String>,
    pub subscription_id: Option<String>,
    pub plan_expires_at: Option<OffsetDateTime>,
    pub updated_at: Option<OffsetDateTime>,
    /// Set once the current subscription was cancelled: the plan ends at `plan_expires_at`.
    pub cancelled_at: Option<OffsetDateTime>,
}

pub async fn account_billing(
    pool: &PgPool,
    account_id: Uuid,
) -> Result<Option<AccountBilling>, sqlx::Error> {
    sqlx::query_as(
        "SELECT dodo_customer_id AS customer_id, dodo_subscription_id AS subscription_id, \
                plan_expires_at, billing_updated_at AS updated_at, \
                plan_cancelled_at AS cancelled_at \
         FROM accounts WHERE id = $1",
    )
    .bind(account_id)
    .fetch_optional(pool)
    .await
}

/// Puts the account on `plan` until `expires` (`None` for no expiry) and brings its sites in
/// line with the plan's limits. Run it inside the transaction that decided the change.
pub async fn apply_plan(
    conn: &mut PgConnection,
    account_id: Uuid,
    plan: Plan,
    expires: Option<OffsetDateTime>,
) -> Result<(), sqlx::Error> {
    // A plan change is a fresh start for the cancellation mark: a paid-up subscription isn't
    // cancelled, and a downgrade has nothing left to cancel.
    sqlx::query(
        "UPDATE accounts SET plan = $2::plan, plan_expires_at = $3, plan_cancelled_at = NULL \
         WHERE id = $1",
    )
    .bind(account_id)
    .bind(plan_slug(plan))
    .bind(expires)
    .execute(&mut *conn)
    .await?;
    apply_plan_limits(conn, account_id).await
}

fn plan_slug(plan: Plan) -> &'static str {
    match plan {
        Plan::Free => "free",
        Plan::Pro => "pro",
        Plan::Agency => "agency",
        Plan::SelfHosted => "self_hosted",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// A subscription that is paid up: set the plan from the product.
    Activate { needs_active_status: bool },
    /// Keep the plan until its expiry.
    Cancel,
    /// The subscription is over.
    End,
    /// Payment trouble: Dodo retries, the plan runs to its expiry.
    Watch,
}

fn action_for(kind: &str) -> Option<Action> {
    Some(match kind {
        "subscription.active" | "subscription.renewed" | "subscription.plan_changed" => {
            Action::Activate {
                needs_active_status: false,
            }
        }
        "subscription.updated" => Action::Activate {
            needs_active_status: true,
        },
        "subscription.cancelled" => Action::Cancel,
        "subscription.expired" | "subscription.failed" => Action::End,
        "subscription.on_hold" | "subscription.past_due" => Action::Watch,
        _ => return None,
    })
}

/// Records the event and applies it. See the module docs for the rules; the return value says
/// what happened, for logging.
pub async fn handle_event(
    pool: &PgPool,
    ev: &SubscriptionEvent,
    now: OffsetDateTime,
) -> Result<Outcome, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let inserted = sqlx::query(
        "INSERT INTO billing_events (event_id, type, received_at, payload) \
         VALUES ($1, $2, $3, $4) ON CONFLICT (event_id) DO NOTHING",
    )
    .bind(&ev.event_id)
    .bind(&ev.kind)
    .bind(now)
    .bind(&ev.payload)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if inserted == 0 {
        return Ok(Outcome::Duplicate);
    }
    let Some(action) = action_for(&ev.kind) else {
        tx.commit().await?;
        return Ok(Outcome::Unchanged("unknown event type"));
    };
    let Some(account_id) = resolve_account(&mut tx, ev).await? else {
        tx.commit().await?;
        return Ok(Outcome::NoAccount);
    };

    #[derive(FromRow)]
    struct Locked {
        plan: String,
        dodo_subscription_id: Option<String>,
        plan_expires_at: Option<OffsetDateTime>,
        billing_updated_at: Option<OffsetDateTime>,
    }
    // The row lock serialises two webhooks for one account, so the timestamp check below is
    // made against the state the other one left.
    let account: Locked = sqlx::query_as(
        "SELECT plan::text AS plan, dodo_subscription_id, plan_expires_at, billing_updated_at \
         FROM accounts WHERE id = $1 FOR UPDATE",
    )
    .bind(account_id)
    .fetch_one(&mut *tx)
    .await?;

    if parse_plan(&account.plan) == Plan::SelfHosted {
        tx.commit().await?;
        return Ok(Outcome::Unchanged(
            "self-hosted accounts have no subscription",
        ));
    }
    if account.billing_updated_at.is_some_and(|last| ev.at < last) {
        tx.commit().await?;
        return Ok(Outcome::Stale);
    }
    // The end of an earlier subscription says nothing about the one the account has now.
    let other_subscription = matches!(
        (&account.dodo_subscription_id, &ev.subscription_id),
        (Some(have), Some(got)) if have != got
    );
    if other_subscription && matches!(action, Action::Cancel | Action::End | Action::Watch) {
        tx.commit().await?;
        return Ok(Outcome::Unchanged("another subscription"));
    }

    // A different subscription must not take over an account that is paid up on one already
    // (a forged or misdirected `metadata.account_id` would otherwise swap the ids). Once the
    // paid period has run out, a new subscription is welcome.
    let paid_up = matches!(parse_plan(&account.plan), Plan::Pro | Plan::Agency)
        && account.plan_expires_at.is_some_and(|expires| expires > now);
    if other_subscription && paid_up && matches!(action, Action::Activate { .. }) {
        tracing::warn!(
            account = %account_id,
            current = ?account.dodo_subscription_id,
            offered = ?ev.subscription_id,
            "ignoring a different subscription for an account that is already paid up"
        );
        tx.commit().await?;
        return Ok(Outcome::Unchanged(
            "account is paid up on another subscription",
        ));
    }

    let outcome = match action {
        Action::Activate {
            needs_active_status,
        } => {
            let status_ok = match ev.status.as_deref() {
                Some(s) => s == "active",
                None => !needs_active_status,
            };
            match (ev.plan, status_ok) {
                (Some(plan), true) => {
                    let expires = ev.next_billing_date.unwrap_or(ev.at + FALLBACK_PERIOD) + GRACE;
                    // A customer belongs to one account; if another already has this id, keep
                    // the plan change and leave the id off rather than fail the delivery.
                    let customer = match &ev.customer_id {
                        Some(c) if customer_owned_elsewhere(&mut tx, c, account_id).await? => {
                            tracing::warn!(
                                account = %account_id,
                                "customer id belongs to another account, not storing it"
                            );
                            None
                        }
                        other => other.clone(),
                    };
                    sqlx::query(
                        "UPDATE accounts SET \
                           dodo_customer_id = COALESCE($2, dodo_customer_id), \
                           dodo_subscription_id = COALESCE($3, dodo_subscription_id), \
                           paused = false \
                         WHERE id = $1",
                    )
                    .bind(account_id)
                    .bind(&customer)
                    .bind(&ev.subscription_id)
                    .execute(&mut *tx)
                    .await?;
                    apply_plan(&mut tx, account_id, plan, Some(expires)).await?;
                    Outcome::Applied(plan)
                }
                (None, _) => Outcome::Unchanged("product is not one of our plans"),
                (_, false) => Outcome::Unchanged("subscription is not active"),
            }
        }
        Action::Cancel => {
            sqlx::query("UPDATE accounts SET plan_cancelled_at = $2 WHERE id = $1")
                .bind(account_id)
                .bind(ev.at)
                .execute(&mut *tx)
                .await?;
            Outcome::Unchanged("cancelled: the plan runs to its expiry")
        }
        Action::Watch => Outcome::Unchanged("payment trouble: waiting for Dodo"),
        Action::End => {
            apply_plan(&mut tx, account_id, Plan::Free, None).await?;
            Outcome::Downgraded
        }
    };
    // Every event applied to the account moves the ordering mark, including the ones that
    // change nothing: a late `active` must not win over an `on_hold` that came after it.
    sqlx::query("UPDATE accounts SET billing_updated_at = $2 WHERE id = $1")
        .bind(account_id)
        .bind(ev.at)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(outcome)
}

async fn customer_owned_elsewhere(
    conn: &mut PgConnection,
    customer_id: &str,
    account_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM accounts WHERE dodo_customer_id = $1 AND id <> $2)",
    )
    .bind(customer_id)
    .bind(account_id)
    .fetch_one(conn)
    .await
}

/// The account an event is about: the owner of the subscription if some account already has it
/// (the metadata can't redirect a subscription away from its owner), else the id the checkout
/// carried, else the customer, else the customer's email.
async fn resolve_account(
    conn: &mut PgConnection,
    ev: &SubscriptionEvent,
) -> Result<Option<Uuid>, sqlx::Error> {
    if let Some(sub) = &ev.subscription_id {
        let owner: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM accounts WHERE dodo_subscription_id = $1")
                .bind(sub)
                .fetch_optional(&mut *conn)
                .await?;
        if owner.is_some() {
            return Ok(owner);
        }
    }
    if let Some(id) = ev.account_hint {
        let found: Option<Uuid> = sqlx::query_scalar("SELECT id FROM accounts WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
        if found.is_some() {
            return Ok(found);
        }
    }
    let lookups: [(&str, &Option<String>); 2] = [
        (
            "SELECT id FROM accounts WHERE dodo_customer_id = $1",
            &ev.customer_id,
        ),
        (
            "SELECT id FROM accounts WHERE email_canonical = $1",
            &ev.email_canonical,
        ),
    ];
    for (sql, value) in lookups {
        if let Some(value) = value {
            let found: Option<Uuid> = sqlx::query_scalar(sql)
                .bind(value)
                .fetch_optional(&mut *conn)
                .await?;
            if found.is_some() {
                return Ok(found);
            }
        }
    }
    Ok(None)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetMonitored {
    Saved,
    /// More sites than the plan allows.
    TooMany {
        max: u32,
    },
    /// One of the ids isn't a site of this account.
    UnknownSite,
}

/// Makes exactly `keep` the account's monitored sites, for the owner choosing which sites stay
/// after a downgrade. Sites turned back on get a fresh crawl slot on the plan's schedule.
pub async fn set_monitored(
    pool: &PgPool,
    account_id: Uuid,
    keep: &[Uuid],
) -> Result<SetMonitored, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let plan: String =
        sqlx::query_scalar("SELECT plan::text FROM accounts WHERE id = $1 FOR UPDATE")
            .bind(account_id)
            .fetch_one(&mut *tx)
            .await?;
    let keep: Vec<Uuid> = keep
        .iter()
        .copied()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let owned: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sites WHERE account_id = $1 AND id = ANY($2)")
            .bind(account_id)
            .bind(&keep)
            .fetch_one(&mut *tx)
            .await?;
    if owned != keep.len() as i64 {
        return Ok(SetMonitored::UnknownSite);
    }
    if let Some(max) = PlanLimits::for_plan(parse_plan(&plan)).max_sites
        && keep.len() > max as usize
    {
        return Ok(SetMonitored::TooMany { max });
    }
    sqlx::query(
        "UPDATE sites SET \
           next_crawl_at = CASE WHEN NOT monitoring_active AND id = ANY($2) \
                                THEN NULL ELSE next_crawl_at END, \
           monitoring_active = (id = ANY($2)) \
         WHERE account_id = $1",
    )
    .bind(account_id)
    .bind(&keep)
    .execute(&mut *tx)
    .await?;
    apply_plan_limits(&mut tx, account_id).await?;
    tx.commit().await?;
    Ok(SetMonitored::Saved)
}
