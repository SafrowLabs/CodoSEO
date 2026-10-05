//! Dodo Payments: webhook signatures (Standard Webhooks), the subscription envelope, and the
//! two API calls the app makes (checkout and customer portal).
//!
//! Field names in the webhook payload follow Dodo's documentation. They are read leniently
//! (everything optional, unknown fields ignored) and should be confirmed against payloads from
//! Dodo's live webhook deliveries before launch.

use axum::http::HeaderMap;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use serde_json::Value;
use sha2::Sha256;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use url::Url;

use crate::config::DodoConfig;

/// How far a webhook's timestamp may be from now, either way.
pub const TOLERANCE_SECS: i64 = 5 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SigError {
    #[error("missing {0} header")]
    MissingHeader(&'static str),
    #[error("webhook-timestamp is not a unix timestamp")]
    BadTimestamp,
    #[error("webhook-timestamp is outside the 5 minute window")]
    Stale,
    #[error("the webhook secret is not a whsec_ key")]
    BadSecret,
    #[error("no valid signature")]
    NoValidSignature,
}

fn header<'a>(headers: &'a HeaderMap, name: &'static str) -> Result<&'a str, SigError> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .ok_or(SigError::MissingHeader(name))
}

/// The shortest webhook key accepted. HMAC takes a key of any length, an empty one included,
/// so without a floor a `whsec_` secret with nothing after it would "verify" forgeries.
pub const MIN_KEY_BYTES: usize = 16;

/// The HMAC key behind a `whsec_<base64>` secret.
pub fn key(secret: &str) -> Result<Vec<u8>, SigError> {
    let encoded = secret
        .trim()
        .strip_prefix("whsec_")
        .ok_or(SigError::BadSecret)?;
    let key = B64.decode(encoded).map_err(|_| SigError::BadSecret)?;
    if key.len() < MIN_KEY_BYTES {
        return Err(SigError::BadSecret);
    }
    Ok(key)
}

fn mac(key: &[u8], id: &str, timestamp: &str, body: &[u8]) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts keys of any length");
    mac.update(id.as_bytes());
    mac.update(b".");
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    mac
}

/// Checks a webhook the Standard Webhooks way: `webhook-id`, `webhook-timestamp` (unix seconds,
/// within five minutes of `now`) and `webhook-signature` (`v1,<base64 HMAC-SHA256 of
/// "{id}.{timestamp}.{body}">`, several separated by spaces; one valid is enough). Signatures
/// are compared in constant time.
pub fn verify(
    secret: &str,
    headers: &HeaderMap,
    body: &[u8],
    now: OffsetDateTime,
) -> Result<(), SigError> {
    let id = header(headers, "webhook-id")?;
    let timestamp = header(headers, "webhook-timestamp")?;
    let signatures = header(headers, "webhook-signature")?;
    let sent: i64 = timestamp.parse().map_err(|_| SigError::BadTimestamp)?;
    // `abs_diff` can't overflow, whatever the sender put in the header.
    if now.unix_timestamp().abs_diff(sent) > TOLERANCE_SECS as u64 {
        return Err(SigError::Stale);
    }
    let key = key(secret)?;
    let valid = signatures
        .split_ascii_whitespace()
        .filter_map(|s| s.strip_prefix("v1,"))
        .filter_map(|s| B64.decode(s).ok())
        .any(|sig| mac(&key, id, timestamp, body).verify_slice(&sig).is_ok());
    if valid {
        Ok(())
    } else {
        Err(SigError::NoValidSignature)
    }
}

/// The `webhook-signature` value for a body: what Dodo sends, and what tests send.
pub fn sign(secret: &str, id: &str, timestamp: i64, body: &[u8]) -> Result<String, SigError> {
    let key = key(secret)?;
    let sig = mac(&key, id, &timestamp.to_string(), body)
        .finalize()
        .into_bytes();
    Ok(format!("v1,{}", B64.encode(sig)))
}

// ---- the envelope -------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    pub subscription_id: Option<String>,
    pub product_id: Option<String>,
    pub customer_id: Option<String>,
    pub customer_email: Option<String>,
    pub status: Option<String>,
    pub next_billing_date: Option<OffsetDateTime>,
    /// `metadata.account_id`, set when the checkout was created.
    pub metadata_account_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DodoEvent {
    /// The `webhook-id` header; the body has no id of its own.
    pub id: String,
    /// `subscription.active`, ...
    pub kind: String,
    /// When it happened, from the body; `None` if missing or unreadable.
    pub timestamp: Option<OffsetDateTime>,
    /// Present on subscription events.
    pub subscription: Option<Subscription>,
}

/// ISO 8601 with an offset, or without one (taken as UTC).
fn parse_time(text: &str) -> Option<OffsetDateTime> {
    let text = text.trim();
    OffsetDateTime::parse(text, &Rfc3339)
        .or_else(|_| OffsetDateTime::parse(&format!("{text}Z"), &Rfc3339))
        .ok()
}

/// A string field, or `None` when it is missing, null, empty, or not a string (logged: the
/// signature was valid, so a surprise in the payload is worth a line, not a rejection).
fn text(object: &serde_json::Map<String, Value>, field: &str) -> Option<String> {
    match object.get(field) {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
        Some(Value::String(_)) => None,
        Some(_) => {
            tracing::warn!(field, "billing webhook field is not a string, ignoring it");
            None
        }
    }
}

/// An object field, or `None` (logged) when it is something else.
fn object<'a>(
    parent: &'a serde_json::Map<String, Value>,
    field: &str,
) -> Option<&'a serde_json::Map<String, Value>> {
    match parent.get(field) {
        None | Some(Value::Null) => None,
        Some(Value::Object(o)) => Some(o),
        Some(_) => {
            tracing::warn!(field, "billing webhook field is not an object, ignoring it");
            None
        }
    }
}

/// The body wasn't a JSON object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the webhook body is not a JSON object")]
pub struct NotAnObject;

/// Reads a verified webhook body. Fails only when it isn't a JSON object; every field inside
/// is optional, and one of the wrong type counts as absent.
pub fn parse_event(id: &str, body: &[u8]) -> Result<DodoEvent, NotAnObject> {
    let Ok(Value::Object(root)) = serde_json::from_slice::<Value>(body) else {
        return Err(NotAnObject);
    };
    let kind = text(&root, "type").unwrap_or_default();
    let timestamp = text(&root, "timestamp").as_deref().and_then(parse_time);
    let subscription = object(&root, "data")
        .filter(|d| kind.starts_with("subscription.") || d.contains_key("subscription_id"))
        .map(|d| {
            let customer = object(d, "customer");
            Subscription {
                subscription_id: text(d, "subscription_id"),
                product_id: text(d, "product_id"),
                customer_id: customer.and_then(|c| text(c, "customer_id")),
                customer_email: customer.and_then(|c| text(c, "email")),
                status: text(d, "status"),
                next_billing_date: text(d, "next_billing_date").as_deref().and_then(parse_time),
                metadata_account_id: object(d, "metadata").and_then(|m| text(m, "account_id")),
            }
        });
    Ok(DodoEvent {
        id: id.to_owned(),
        kind,
        timestamp,
        subscription,
    })
}

// ---- API calls ----------------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum DodoError {
    #[error("could not reach Dodo: {0}")]
    Unreachable(String),
    #[error("Dodo answered {status}: {body}")]
    Rejected { status: u16, body: String },
    #[error("Dodo's answer had no usable {0}")]
    BadResponse(&'static str),
}

#[derive(Deserialize)]
struct CheckoutResponse {
    checkout_url: Option<String>,
}

#[derive(Deserialize)]
struct PortalResponse {
    link: Option<String>,
}

async fn post(
    http: &reqwest::Client,
    cfg: &DodoConfig,
    url: Url,
    body: Option<serde_json::Value>,
) -> Result<String, DodoError> {
    let mut req = http.post(url).bearer_auth(&cfg.api_key);
    if let Some(body) = body {
        req = req.json(&body);
    }
    let res = req
        .send()
        .await
        .map_err(|e| DodoError::Unreachable(e.to_string()))?;
    let status = res.status();
    let text = res
        .text()
        .await
        .map_err(|e| DodoError::Unreachable(e.to_string()))?;
    if !status.is_success() {
        return Err(DodoError::Rejected {
            status: status.as_u16(),
            body: text.chars().take(300).collect(),
        });
    }
    Ok(text)
}

/// A link Dodo gave us must at least be a web address before we redirect to it.
fn web_url(link: Option<String>, field: &'static str) -> Result<String, DodoError> {
    link.filter(|l| {
        Url::parse(l).is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host().is_some())
    })
    .ok_or(DodoError::BadResponse(field))
}

/// Starts a hosted checkout for `product_id`; returns the page to send the customer to.
pub async fn create_checkout(
    http: &reqwest::Client,
    cfg: &DodoConfig,
    product_id: &str,
    email: &str,
    account_id: uuid::Uuid,
    return_url: &str,
) -> Result<String, DodoError> {
    let url = cfg
        .api_url
        .join("checkouts")
        .map_err(|_| DodoError::BadResponse("api url"))?;
    let text = post(
        http,
        cfg,
        url,
        Some(serde_json::json!({
            "product_cart": [{ "product_id": product_id, "quantity": 1 }],
            "customer": { "email": email },
            "return_url": return_url,
            "metadata": { "account_id": account_id.to_string() },
        })),
    )
    .await?;
    let parsed: CheckoutResponse =
        serde_json::from_str(&text).map_err(|_| DodoError::BadResponse("checkout_url"))?;
    web_url(parsed.checkout_url, "checkout_url")
}

/// Opens a customer-portal session; returns the link to send the customer to.
pub async fn create_portal_session(
    http: &reqwest::Client,
    cfg: &DodoConfig,
    customer_id: &str,
    return_url: &str,
) -> Result<String, DodoError> {
    let mut url = cfg
        .api_url
        .join(&format!(
            "customers/{}/customer-portal/session",
            urlencode(customer_id)
        ))
        .map_err(|_| DodoError::BadResponse("api url"))?;
    url.query_pairs_mut().append_pair("return_url", return_url);
    let text = post(http, cfg, url, None).await?;
    let parsed: PortalResponse =
        serde_json::from_str(&text).map_err(|_| DodoError::BadResponse("link"))?;
    web_url(parsed.link, "link")
}

/// Escapes a path segment.
fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    const SECRET: &str = "whsec_dGVzdC1zZWNyZXQtMDEyMzQ1Njc4OQ==";
    const BODY: &[u8] = br#"{"type":"subscription.active"}"#;

    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap()
    }

    fn headers(id: &str, ts: i64, sig: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("webhook-id", HeaderValue::from_str(id).unwrap());
        h.insert(
            "webhook-timestamp",
            HeaderValue::from_str(&ts.to_string()).unwrap(),
        );
        h.insert("webhook-signature", HeaderValue::from_str(sig).unwrap());
        h
    }

    fn signed(ts: i64) -> HeaderMap {
        headers("msg_1", ts, &sign(SECRET, "msg_1", ts, BODY).unwrap())
    }

    #[test]
    fn a_valid_signature_verifies() {
        let ts = now().unix_timestamp();
        assert_eq!(verify(SECRET, &signed(ts), BODY, now()), Ok(()));
    }

    #[test]
    fn the_signature_matches_the_standard_webhooks_reference_vector() {
        // From the Standard Webhooks test suite: secret MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw,
        // id msg_p5jXN8AQM9LWM0D4loKWxJek, timestamp 1614265330.
        let secret = "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw";
        let body = br#"{"test": 2432232314}"#;
        assert_eq!(
            sign(secret, "msg_p5jXN8AQM9LWM0D4loKWxJek", 1_614_265_330, body).unwrap(),
            "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE="
        );
    }

    #[test]
    fn a_wrong_secret_is_refused() {
        let ts = now().unix_timestamp();
        let other = "whsec_b3RoZXItc2VjcmV0LTAxMjM0NTY3ODk=";
        assert_eq!(
            verify(other, &signed(ts), BODY, now()),
            Err(SigError::NoValidSignature)
        );
    }

    #[test]
    fn a_modified_body_is_refused() {
        let ts = now().unix_timestamp();
        assert_eq!(
            verify(
                SECRET,
                &signed(ts),
                br#"{"type":"subscription.expired"}"#,
                now()
            ),
            Err(SigError::NoValidSignature)
        );
    }

    #[test]
    fn a_modified_id_is_refused() {
        let ts = now().unix_timestamp();
        let h = headers("msg_2", ts, &sign(SECRET, "msg_1", ts, BODY).unwrap());
        assert_eq!(
            verify(SECRET, &h, BODY, now()),
            Err(SigError::NoValidSignature)
        );
    }

    #[test]
    fn a_stale_or_future_timestamp_is_refused() {
        let ts = now().unix_timestamp() - TOLERANCE_SECS - 1;
        assert_eq!(
            verify(SECRET, &signed(ts), BODY, now()),
            Err(SigError::Stale)
        );
        let ts = now().unix_timestamp() + TOLERANCE_SECS + 1;
        assert_eq!(
            verify(SECRET, &signed(ts), BODY, now()),
            Err(SigError::Stale)
        );
        // Right at the edge is still fine.
        let ts = now().unix_timestamp() - TOLERANCE_SECS;
        assert_eq!(verify(SECRET, &signed(ts), BODY, now()), Ok(()));
    }

    #[test]
    fn a_missing_header_is_refused() {
        let ts = now().unix_timestamp();
        for name in ["webhook-id", "webhook-timestamp", "webhook-signature"] {
            let mut h = signed(ts);
            h.remove(name);
            assert!(
                matches!(verify(SECRET, &h, BODY, now()), Err(SigError::MissingHeader(n)) if n == name),
                "{name}"
            );
        }
    }

    #[test]
    fn a_garbled_timestamp_or_signature_is_refused() {
        let mut h = signed(now().unix_timestamp());
        h.insert("webhook-timestamp", HeaderValue::from_static("yesterday"));
        assert_eq!(verify(SECRET, &h, BODY, now()), Err(SigError::BadTimestamp));
        let h = headers(
            "msg_1",
            now().unix_timestamp(),
            "v1,%%%not-base64 v2,AAAA nonsense",
        );
        assert_eq!(
            verify(SECRET, &h, BODY, now()),
            Err(SigError::NoValidSignature)
        );
    }

    #[test]
    fn one_valid_signature_among_several_passes() {
        let ts = now().unix_timestamp();
        let good = sign(SECRET, "msg_1", ts, BODY).unwrap();
        let bad = sign("whsec_b3RoZXItc2VjcmV0LTAxMjM0NTY3ODk=", "msg_1", ts, BODY).unwrap();
        for list in [
            format!("{bad} {good}"),
            format!("{good} {bad}"),
            format!("v2,AAAA {bad} {good}"),
        ] {
            let h = headers("msg_1", ts, &list);
            assert_eq!(verify(SECRET, &h, BODY, now()), Ok(()), "{list}");
        }
        let h = headers("msg_1", ts, &format!("{bad} {bad}"));
        assert_eq!(
            verify(SECRET, &h, BODY, now()),
            Err(SigError::NoValidSignature)
        );
    }

    #[test]
    fn an_empty_or_short_key_is_refused_even_with_a_matching_signature() {
        let ts = now().unix_timestamp();
        for short in ["whsec_", "whsec_YQ==", "whsec_c2VjcmV0LTAxMjM0NQ=="] {
            // 0, 1 and 15 bytes: the HMAC would accept them, so `key` must not.
            assert_eq!(
                sign(short, "msg_1", ts, BODY),
                Err(SigError::BadSecret),
                "{short}"
            );
            let h = headers("msg_1", ts, "v1,AAAA");
            assert_eq!(
                verify(short, &h, BODY, now()),
                Err(SigError::BadSecret),
                "{short}"
            );
        }
        // 16 bytes is the smallest accepted.
        assert!(sign("whsec_MDEyMzQ1Njc4OWFiY2RlZg==", "msg_1", ts, BODY).is_ok());
    }

    #[test]
    fn extreme_timestamps_are_refused_without_panicking() {
        for extreme in [i64::MIN, i64::MAX, i64::MIN + 1, 0] {
            let h = headers("msg_1", extreme, "v1,AAAA");
            assert_eq!(
                verify(SECRET, &h, BODY, now()),
                Err(SigError::Stale),
                "{extreme}"
            );
        }
    }

    #[test]
    fn a_field_of_the_wrong_type_is_treated_as_absent() {
        let body = br#"{
            "type": "subscription.active",
            "timestamp": 1760000000,
            "data": {
                "subscription_id": "sub_1",
                "product_id": ["pdt_pro"],
                "status": 7,
                "next_billing_date": {"at": "soon"},
                "customer": "cus_1",
                "metadata": "account"
            }
        }"#;
        let ev = parse_event("m", body).unwrap();
        assert_eq!(ev.kind, "subscription.active");
        assert_eq!(ev.timestamp, None);
        let sub = ev.subscription.unwrap();
        assert_eq!(sub.subscription_id.as_deref(), Some("sub_1"));
        assert_eq!(sub.product_id, None);
        assert_eq!(sub.status, None);
        assert_eq!(sub.next_billing_date, None);
        assert_eq!(sub.customer_id, None);
        assert_eq!(sub.metadata_account_id, None);

        // Only something that isn't a JSON object at all is refused.
        for bad in ["[]", "7", "\"text\"", "null", "not json"] {
            assert!(parse_event("m", bad.as_bytes()).is_err(), "{bad}");
        }
        let ev = parse_event("m", br#"{"type": 5, "data": 5}"#).unwrap();
        assert_eq!(ev.kind, "");
        assert!(ev.subscription.is_none());
    }

    #[test]
    fn the_config_does_not_print_its_secrets() {
        let cfg = DodoConfig {
            api_key: "dodo_key_SUPERSECRET".to_owned(),
            webhook_secret: "whsec_WEBHOOKSECRET".to_owned(),
            product_pro: "pdt_pro".to_owned(),
            product_agency: "pdt_agency".to_owned(),
            api_url: Url::parse("https://test.dodopayments.com").unwrap(),
        };
        let shown = format!("{cfg:?}");
        assert!(
            !shown.contains("SUPERSECRET") && !shown.contains("WEBHOOKSECRET"),
            "{shown}"
        );
        assert!(shown.contains("pdt_pro") && shown.contains("test.dodopayments.com"));
    }

    #[test]
    fn a_secret_that_is_not_whsec_is_a_configuration_error() {
        let ts = now().unix_timestamp();
        assert_eq!(
            verify("plain", &signed(ts), BODY, now()),
            Err(SigError::BadSecret)
        );
    }

    #[test]
    fn a_subscription_event_is_read() {
        let body = br#"{
            "business_id": "bus_1",
            "type": "subscription.active",
            "timestamp": "2026-10-05T10:00:00.123456Z",
            "data": {
                "payload_type": "Subscription",
                "subscription_id": "sub_1",
                "product_id": "pdt_pro",
                "status": "active",
                "next_billing_date": "2026-11-05T10:00:00Z",
                "customer": {"customer_id": "cus_1", "email": "ana@example.com", "name": "Ana"},
                "metadata": {"account_id": "6f1d9a54-6b0e-4a39-9f43-0c2a8b6d8e11"},
                "something_new": {"nested": [1, 2, 3]}
            },
            "also_new": true
        }"#;
        let ev = parse_event("msg_1", body).unwrap();
        assert_eq!(ev.id, "msg_1");
        assert_eq!(ev.kind, "subscription.active");
        assert_eq!(
            ev.timestamp,
            Some(OffsetDateTime::parse("2026-10-05T10:00:00.123456Z", &Rfc3339).unwrap())
        );
        let sub = ev.subscription.unwrap();
        assert_eq!(sub.subscription_id.as_deref(), Some("sub_1"));
        assert_eq!(sub.product_id.as_deref(), Some("pdt_pro"));
        assert_eq!(sub.customer_id.as_deref(), Some("cus_1"));
        assert_eq!(sub.customer_email.as_deref(), Some("ana@example.com"));
        assert_eq!(sub.status.as_deref(), Some("active"));
        assert_eq!(
            sub.next_billing_date,
            Some(OffsetDateTime::parse("2026-11-05T10:00:00Z", &Rfc3339).unwrap())
        );
        assert_eq!(
            sub.metadata_account_id.as_deref(),
            Some("6f1d9a54-6b0e-4a39-9f43-0c2a8b6d8e11")
        );
    }

    #[test]
    fn missing_fields_and_other_events_are_tolerated() {
        let ev = parse_event("m", br#"{"type":"subscription.on_hold","data":{}}"#).unwrap();
        let sub = ev.subscription.unwrap();
        assert_eq!(sub.subscription_id, None);
        assert_eq!(ev.timestamp, None);

        let ev = parse_event(
            "m",
            br#"{"type":"payment.succeeded","data":{"payment_id":"p"}}"#,
        )
        .unwrap();
        assert_eq!(ev.kind, "payment.succeeded");
        assert!(ev.subscription.is_none());

        // A timestamp without an offset is read as UTC; garbage is "unknown".
        let ev = parse_event("m", br#"{"type":"x","timestamp":"2026-10-05T10:00:00"}"#).unwrap();
        assert!(ev.timestamp.is_some());
        let ev = parse_event("m", br#"{"type":"x","timestamp":"soon"}"#).unwrap();
        assert!(ev.timestamp.is_none());

        assert!(parse_event("m", b"not json").is_err());
    }
}
