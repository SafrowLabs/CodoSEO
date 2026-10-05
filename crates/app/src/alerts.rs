//! Alerts, in two phases. A finished crawl with changes queues one `send_alert {crawl_id}`
//! planning job ([`plan_alert`]): it matches the crawl's changes against the site's rules and
//! queues one delivery job per channel, `send_alert {crawl_id, channel_id, change_ids}`, marking
//! the routed changes `alerted_at`. A delivery job ([`deliver_alert`]) sends one grouped message
//! to one channel, so a retry touches only that channel. Changes no rule routes stay unalerted:
//! the Monday digest picks them up.
//!
//! A site that could not be crawled twice in a row takes the same two steps with
//! `unreachable: true`, to every enabled channel the plan allows.

use std::collections::{BTreeMap, HashSet};

use codoseo_core::change::ChangeKind;
use codoseo_core::plan::PlanLimits;
use codoseo_notify::{AlertItem, AlertMessage, ChannelKind, DeliveryError, deliver};
use codoseo_store::alert_rules::{self, AlertCrawl, Route, StoredChange};
use codoseo_store::jobs::{ClaimedJob, JobKind, JobQueue};
use codoseo_store::{accounts, channels};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::jobs::JobContext;

#[derive(Deserialize)]
struct AlertPayload {
    crawl_id: Uuid,
    /// Set on a delivery job; a planning job has none.
    #[serde(default)]
    channel_id: Option<Uuid>,
    #[serde(default)]
    change_ids: Vec<i64>,
    #[serde(default)]
    unreachable: bool,
}

fn db(e: impl std::fmt::Display) -> String {
    format!("database error: {e}")
}

/// The `send_alert` handler: plans or delivers, by what the payload names.
pub async fn send_alert(ctx: &JobContext, job: &ClaimedJob) -> Result<(), String> {
    let p: AlertPayload = serde_json::from_value(job.payload.clone())
        .map_err(|e| format!("send_alert payload is invalid: {e}"))?;
    match p.channel_id {
        None if p.unreachable => plan_unreachable(ctx, p.crawl_id).await,
        None => plan_alert(ctx, p.crawl_id).await,
        Some(channel_id) => {
            // `attempt` counts the earlier tries; this one is the last when the queue won't
            // retry it again.
            let last_attempt = job.attempt + 1 >= job.max_attempts;
            let delivery = Delivery {
                crawl_id: p.crawl_id,
                channel_id,
                change_ids: p.change_ids,
                unreachable: p.unreachable,
            };
            deliver_alert(ctx, &delivery, last_attempt).await
        }
    }
}

/// The channels alerts may go to now: enabled, not muted, and allowed by the plan (Free gets
/// email only).
async fn usable_channels(
    ctx: &JobContext,
    account_id: Uuid,
) -> Result<Vec<(Uuid, ChannelKind)>, String> {
    let account = accounts::find(&ctx.pool, account_id)
        .await
        .map_err(db)?
        .ok_or_else(|| "the account no longer exists".to_owned())?;
    let email_only = PlanLimits::for_plan(account.plan).email_alerts_only;
    Ok(channels::deliverable(&ctx.pool, account_id)
        .await
        .map_err(db)?
        .into_iter()
        .filter(|(_, kind)| !email_only || *kind == ChannelKind::Email)
        .collect())
}

/// Loads the crawl when it can alert at all: an owned site, and not a no-signup audit.
async fn alertable(ctx: &JobContext, crawl_id: Uuid) -> Result<Option<(AlertCrawl, Uuid)>, String> {
    let Some(crawl) = alert_rules::alert_crawl(&ctx.pool, crawl_id)
        .await
        .map_err(db)?
    else {
        return Ok(None);
    };
    match crawl.account_id {
        Some(account_id) if !crawl.quick => Ok(Some((crawl, account_id))),
        _ => Ok(None),
    }
}

/// Makes sure the account's own address is a channel with the default rules on this site, so a
/// site that predates alert rules (or an account that never opened the settings) is covered.
async fn ensure_defaults(ctx: &JobContext, account_id: Uuid, site_id: Uuid) -> Result<(), String> {
    let email = channels::ensure_default_email(&ctx.pool, &ctx.channel_key, account_id)
        .await
        .map_err(db)?;
    alert_rules::create_defaults(&ctx.pool, site_id, email)
        .await
        .map_err(db)
}

/// Phase one: which of the crawl's unalerted changes go instantly to which channel.
///
/// `became_noindex` is instant only on a key page (the start page, the 20 pages with the most
/// inlinks, or a starred page); on any other page it waits for the digest like everything
/// without an instant rule.
pub async fn plan_alert(ctx: &JobContext, crawl_id: Uuid) -> Result<(), String> {
    let Some((crawl, account_id)) = alertable(ctx, crawl_id).await? else {
        return Ok(());
    };
    let changes = alert_rules::unalerted_changes(&ctx.pool, crawl_id)
        .await
        .map_err(db)?;
    if changes.is_empty() {
        return Ok(());
    }
    ensure_defaults(ctx, account_id, crawl.site_id).await?;
    let usable = usable_channels(ctx, account_id).await?;

    let key_pages = if changes.iter().any(|c| c.kind == ChangeKind::BecameNoindex) {
        alert_rules::key_page_urls(&ctx.pool, crawl_id, &crawl.starred)
            .await
            .map_err(db)?
    } else {
        HashSet::new()
    };

    // The instant channels per kind, looked up once each.
    let mut channels_for: BTreeMap<&'static str, Vec<Uuid>> = BTreeMap::new();
    for change in &changes {
        if !channels_for.contains_key(change.kind.slug()) {
            let instant = alert_rules::instant_channels_for(&ctx.pool, crawl.site_id, change.kind)
                .await
                .map_err(db)?;
            channels_for.insert(change.kind.slug(), instant);
        }
    }

    let mut routes: Vec<Route> = usable
        .iter()
        .map(|(id, _)| Route {
            channel_id: *id,
            change_ids: Vec::new(),
        })
        .collect();
    for change in &changes {
        if change.kind == ChangeKind::BecameNoindex
            && !change.url.as_ref().is_some_and(|u| key_pages.contains(u))
        {
            continue;
        }
        for route in &mut routes {
            if channels_for[change.kind.slug()].contains(&route.channel_id) {
                route.change_ids.push(change.id);
            }
        }
    }
    routes.retain(|r| !r.change_ids.is_empty());
    alert_rules::enqueue_routes(&ctx.pool, crawl_id, &routes)
        .await
        .map_err(db)
}

/// Phase one for a site that could not be crawled: every enabled channel the plan allows.
pub async fn plan_unreachable(ctx: &JobContext, crawl_id: Uuid) -> Result<(), String> {
    let Some((crawl, account_id)) = alertable(ctx, crawl_id).await? else {
        return Ok(());
    };
    ensure_defaults(ctx, account_id, crawl.site_id).await?;
    let usable = usable_channels(ctx, account_id).await?;
    let ids: Vec<Uuid> = usable.into_iter().map(|(id, _)| id).collect();
    alert_rules::enqueue_unreachable(&ctx.pool, crawl_id, &ids)
        .await
        .map_err(db)
}

/// One delivery job: this crawl's message to this channel.
#[derive(Debug, Clone)]
pub struct Delivery {
    pub crawl_id: Uuid,
    pub channel_id: Uuid,
    /// The changes to list; empty for an unreachable alert.
    pub change_ids: Vec<i64>,
    pub unreachable: bool,
}

fn item(change: &StoredChange) -> AlertItem {
    AlertItem {
        severity: change.severity,
        kind: change.kind.slug().to_owned(),
        kind_label: change.kind.label().to_owned(),
        url: change.url.clone(),
        before: change.before.clone(),
        after: change.after.clone(),
    }
}

fn channel_name(kind: ChannelKind) -> &'static str {
    match kind {
        ChannelKind::Email => "email",
        ChannelKind::Slack => "Slack",
        ChannelKind::Discord => "Discord",
        ChannelKind::Webhook => "webhook",
    }
}

/// Phase two: sends the message. A failure is recorded on the channel and returned, so the job
/// retries with backoff; on the job's `last_attempt` the channel is switched off and the account
/// is told, once.
///
/// A channel that has been deleted, muted or switched off, or that the plan no longer allows,
/// is skipped without error: the job is simply done.
pub async fn deliver_alert(
    ctx: &JobContext,
    delivery: &Delivery,
    last_attempt: bool,
) -> Result<(), String> {
    let pool = &ctx.pool;
    let Some(state) = channels::state(pool, delivery.channel_id)
        .await
        .map_err(db)?
    else {
        return Ok(());
    };
    if !state.enabled || state.muted {
        return Ok(());
    }
    let Some(account) = accounts::find(pool, state.account_id).await.map_err(db)? else {
        return Ok(());
    };
    if PlanLimits::for_plan(account.plan).email_alerts_only && state.kind != ChannelKind::Email {
        return Ok(());
    }
    let Some(crawl) = alert_rules::alert_crawl(pool, delivery.crawl_id)
        .await
        .map_err(db)?
    else {
        return Ok(());
    };

    let site_url = ctx
        .base_url
        .join(&format!("s/{}/changes", crawl.site_id))
        .map_err(|e| format!("bad base URL: {e}"))?;
    let message = if delivery.unreachable {
        let reason = crawl
            .failure_reason
            .as_deref()
            .unwrap_or("it did not answer");
        AlertMessage::unreachable(&crawl.domain, site_url, delivery.crawl_id, reason)
    } else {
        let changes = alert_rules::changes_by_ids(pool, delivery.crawl_id, &delivery.change_ids)
            .await
            .map_err(db)?;
        if changes.is_empty() {
            return Ok(());
        }
        let headline = format!(
            "{} {} on {}",
            changes.len(),
            if changes.len() == 1 {
                "change"
            } else {
                "changes"
            },
            crawl.domain
        );
        AlertMessage::changes(
            &crawl.domain,
            site_url,
            delivery.crawl_id,
            headline,
            changes.iter().map(item).collect(),
        )
    };

    let outcome = match channels::get_target(pool, &ctx.channel_key, delivery.channel_id).await {
        Ok(Some(target)) => deliver(&ctx.http, &ctx.mailer, &target, &message)
            .await
            .map_err(|e: DeliveryError| e.to_string()),
        Ok(None) => return Ok(()),
        Err(e) => Err(e.to_string()),
    };
    match outcome {
        Ok(()) => {
            channels::record_success(pool, delivery.channel_id)
                .await
                .map_err(db)?;
            Ok(())
        }
        Err(error) => {
            channels::record_failure(pool, delivery.channel_id, &error)
                .await
                .map_err(db)?;
            if last_attempt {
                turn_off(
                    ctx,
                    delivery.channel_id,
                    state.kind,
                    &account.email,
                    &crawl.domain,
                    &error,
                )
                .await?;
            }
            Err(error)
        }
    }
}

/// Switches a failing channel off and emails the account, once (only the call that actually
/// switched it off sends the mail).
async fn turn_off(
    ctx: &JobContext,
    channel_id: Uuid,
    kind: ChannelKind,
    account_email: &str,
    domain: &str,
    error: &str,
) -> Result<(), String> {
    let switched = channels::disable(&ctx.pool, channel_id, error)
        .await
        .map_err(db)?;
    if !switched {
        return Ok(());
    }
    let subject = format!(
        "We turned off your {} alerts for {domain}",
        channel_name(kind)
    );
    let settings = ctx
        .base_url
        .join("settings/alerts")
        .map_err(|e| format!("bad base URL: {e}"))?;
    let text = format!(
        "{subject}: {error}\n\nDelivery failed on every retry. Check the address, then turn the \
         channel back on in your alert settings: {settings}\n"
    );
    JobQueue::new(ctx.pool.clone())
        .enqueue(
            JobKind::SendEmail,
            json!({ "to": account_email, "subject": subject, "text": text }),
        )
        .await
        .map_err(db)?;
    Ok(())
}
