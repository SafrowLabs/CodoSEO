//! `POST /hooks/ses`: Amazon SES bounce and complaint feedback, through an SNS topic subscribed
//! to this address (`SES_SNS_TOPIC_ARN`). A permanently bounced or complaining address goes on
//! the suppression list with scope `all`, so nothing more is sent to it; a transient bounce, a
//! delivery notice or a "not spam" complaint changes nothing.
//!
//! No session and no `Origin` check: the SNS signature is the authentication, and the topic
//! must be the configured one. The subscription is confirmed here too, by opening the
//! `SubscribeURL` SNS sends first. Once a message is verified it is answered 200 whatever it
//! contains; only a database or network failure is a 5xx, which makes SNS try again.

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use codoseo_store::suppressions::{self, Scope};
use serde::Deserialize;

use crate::auth::email;
use crate::error::AppError;
use crate::sns::{self, Envelope};
use crate::state::AppState;

pub const PATH: &str = "/hooks/ses";

/// The most a signing certificate may be: SNS's are about 1.5 KiB.
const CERT_LIMIT: usize = 64 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new().route(PATH, post(receive))
}

async fn receive(State(state): State<AppState>, body: Bytes) -> Result<Response, AppError> {
    let Some(cfg) = state.config.ses_feedback.as_ref() else {
        return Err(AppError::NotFound);
    };
    let envelope: Envelope = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(error) => {
            tracing::warn!(%error, "SES hook body is not an SNS message");
            return Ok((StatusCode::BAD_REQUEST, "not an SNS message").into_response());
        }
    };
    let refuse = |why: &str| -> Result<Response, AppError> {
        tracing::warn!(kind = %envelope.kind, why, "SES hook message refused");
        Ok((StatusCode::UNAUTHORIZED, "refused").into_response())
    };
    if envelope.topic_arn != cfg.topic_arn {
        return refuse("not the configured topic");
    }
    let cert_url = match sns::aws_url(&envelope.signing_cert_url, cfg.sns_base_url.as_ref(), true) {
        Ok(url) => url,
        Err(error) => return refuse(&error.to_string()),
    };
    let cert = match signing_cert(&state, cert_url.as_str()).await {
        Ok(cert) => cert,
        Err(error) => {
            tracing::warn!(%error, "could not fetch the SNS signing certificate");
            return Ok((StatusCode::SERVICE_UNAVAILABLE, "certificate unavailable").into_response());
        }
    };
    if let Err(error) = sns::verify(&cert, &envelope) {
        return refuse(&error.to_string());
    }

    match envelope.kind.as_str() {
        "SubscriptionConfirmation" => {
            let Some(raw) = envelope.subscribe_url.as_deref() else {
                return Ok((StatusCode::BAD_REQUEST, "no SubscribeURL").into_response());
            };
            let url = match sns::aws_url(raw, cfg.sns_base_url.as_ref(), false) {
                Ok(url) => url,
                Err(error) => return refuse(&error.to_string()),
            };
            match state
                .http
                .get(url)
                .send()
                .await
                .and_then(|r| r.error_for_status())
            {
                Ok(_) => {
                    tracing::info!("SES feedback subscription confirmed");
                    Ok((StatusCode::OK, "confirmed").into_response())
                }
                Err(error) => {
                    tracing::warn!(%error, "could not confirm the SES feedback subscription");
                    Ok((StatusCode::SERVICE_UNAVAILABLE, "confirmation failed").into_response())
                }
            }
        }
        "Notification" => {
            let report = apply(&state, &envelope.message).await?;
            tracing::info!(
                kind = %report.kind,
                suppressed = report.suppressed,
                ignored = report.ignored,
                "SES feedback applied"
            );
            Ok((StatusCode::OK, "ok").into_response())
        }
        other => {
            tracing::info!(kind = %other, "SES hook message noted");
            Ok((StatusCode::OK, "ok").into_response())
        }
    }
}

/// The certificate at `url`, fetched once per process: SNS rotates certificates rarely, and a
/// new one has a new URL.
async fn signing_cert(state: &AppState, url: &str) -> Result<Arc<Vec<u8>>, String> {
    if let Some(cert) = state.sns_certs.lock().expect("sns cert lock").get(url) {
        return Ok(Arc::clone(cert));
    }
    let bytes = state
        .http
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.without_url().to_string())?
        .bytes()
        .await
        .map_err(|e| e.without_url().to_string())?;
    if bytes.len() > CERT_LIMIT {
        return Err(format!("the certificate is {} bytes", bytes.len()));
    }
    let cert = Arc::new(bytes.to_vec());
    state
        .sns_certs
        .lock()
        .expect("sns cert lock")
        .insert(url.to_owned(), Arc::clone(&cert));
    Ok(cert)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SesNotification {
    notification_type: String,
    #[serde(default)]
    bounce: Option<Bounce>,
    #[serde(default)]
    complaint: Option<Complaint>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Bounce {
    bounce_type: String,
    #[serde(default)]
    bounced_recipients: Vec<Recipient>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Complaint {
    #[serde(default)]
    complained_recipients: Vec<Recipient>,
    #[serde(default)]
    complaint_feedback_type: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Recipient {
    email_address: String,
}

/// What a notification did.
pub struct Applied {
    pub kind: String,
    pub suppressed: usize,
    pub ignored: usize,
}

/// Reads the SES event inside an SNS notification and suppresses the addresses it names. A
/// message that isn't an SES event is ignored, not refused: the signature already proved it
/// came from the topic.
pub async fn apply(state: &AppState, message: &str) -> Result<Applied, AppError> {
    let Ok(event) = serde_json::from_str::<SesNotification>(message) else {
        return Ok(Applied {
            kind: "unreadable".to_owned(),
            suppressed: 0,
            ignored: 1,
        });
    };
    let (addresses, reason): (Vec<String>, &str) = match (
        event.notification_type.as_str(),
        event.bounce,
        event.complaint,
    ) {
        ("Bounce", Some(b), _) if b.bounce_type == "Permanent" => (
            b.bounced_recipients
                .into_iter()
                .map(|r| r.email_address)
                .collect(),
            "bounce",
        ),
        // A "not-spam" complaint is the person taking a report back: nothing to stop.
        ("Complaint", _, Some(c)) if c.complaint_feedback_type.as_deref() != Some("not-spam") => (
            c.complained_recipients
                .into_iter()
                .map(|r| r.email_address)
                .collect(),
            "complaint",
        ),
        _ => (Vec::new(), ""),
    };
    let mut applied = Applied {
        kind: event.notification_type,
        suppressed: 0,
        ignored: 0,
    };
    for address in addresses {
        // SES may quote the display name: `"Ana" <ana@example.com>`.
        let bare = address
            .rsplit_once('<')
            .map(|(_, rest)| rest.trim_end_matches('>'))
            .unwrap_or(&address);
        let Some(parsed) = email::parse(bare) else {
            applied.ignored += 1;
            continue;
        };
        suppressions::add(&state.pool, &email::canonical(parsed), Scope::All, reason).await?;
        applied.suppressed += 1;
    }
    Ok(applied)
}
