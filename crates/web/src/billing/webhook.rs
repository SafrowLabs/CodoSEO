//! `POST /billing/webhook`: Dodo's subscription events. No session and no `Origin` check; the
//! signature is the authentication. Once a request is verified it is answered 200 whatever it
//! contains (unknown events, unknown accounts), so Dodo doesn't retry what we can't use.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use codoseo_store::billing::{self, Outcome, SubscriptionEvent};
use time::OffsetDateTime;
use uuid::Uuid;

use super::dodo;
use crate::auth::email;
use crate::error::AppError;
use crate::state::AppState;

pub async fn receive(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let Some(cfg) = state.config.billing.as_ref() else {
        return Err(AppError::NotFound);
    };
    let now = OffsetDateTime::now_utc();
    if let Err(error) = dodo::verify(&cfg.webhook_secret, &headers, &body, now) {
        tracing::warn!(%error, "billing webhook refused");
        return Ok((StatusCode::UNAUTHORIZED, "invalid signature").into_response());
    }
    // `verify` found the header, so this is present.
    let id = headers
        .get("webhook-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    // Past this point the request is authentic: answer 200 unless the body isn't a JSON object
    // at all. Fields of the wrong type were already treated as absent by `parse_event`.
    let (Ok(event), Ok(payload)) = (
        dodo::parse_event(id, &body),
        serde_json::from_slice::<serde_json::Value>(&body),
    ) else {
        tracing::warn!(%id, "billing webhook body is not a JSON object");
        return Ok((StatusCode::BAD_REQUEST, "invalid body").into_response());
    };

    let sub = event.subscription.clone();
    let stored = SubscriptionEvent {
        event_id: event.id.clone(),
        kind: event.kind.clone(),
        // The body's own time orders events; the delivery time changes on every retry.
        at: event.timestamp.unwrap_or(now),
        payload,
        account_hint: sub
            .as_ref()
            .and_then(|s| s.metadata_account_id.as_deref())
            .and_then(|a| Uuid::parse_str(a).ok()),
        subscription_id: sub.as_ref().and_then(|s| s.subscription_id.clone()),
        customer_id: sub.as_ref().and_then(|s| s.customer_id.clone()),
        email_canonical: sub
            .as_ref()
            .and_then(|s| s.customer_email.as_deref())
            .map(email::canonical),
        plan: sub
            .as_ref()
            .and_then(|s| s.product_id.as_deref())
            .and_then(|p| cfg.plan_for_product(p)),
        status: sub.as_ref().and_then(|s| s.status.clone()),
        next_billing_date: sub.as_ref().and_then(|s| s.next_billing_date),
    };
    // A database error is a 500 on purpose: nothing was recorded, and Dodo will retry.
    let outcome = billing::handle_event(&state.pool, &stored, now).await?;
    match &outcome {
        Outcome::Duplicate => tracing::info!(%id, kind = %event.kind, "billing webhook replayed"),
        Outcome::NoAccount => {
            tracing::warn!(%id, kind = %event.kind, "billing webhook matches no account")
        }
        Outcome::Stale => {
            tracing::info!(%id, kind = %event.kind, "billing webhook older than the last applied")
        }
        Outcome::Applied(plan) => {
            tracing::info!(%id, kind = %event.kind, ?plan, "billing webhook applied")
        }
        Outcome::Downgraded => {
            tracing::info!(%id, kind = %event.kind, "billing webhook downgraded the account")
        }
        Outcome::Unchanged(why) => {
            tracing::info!(%id, kind = %event.kind, why, "billing webhook changed nothing")
        }
    }
    Ok((StatusCode::OK, "ok").into_response())
}
