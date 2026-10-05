//! The generic webhook: stable JSON, signed with the channel's secret.
//!
//! Receivers verify `X-CodoSEO-Signature: sha256=<hex>` — an HMAC-SHA256 with the channel secret
//! over `{timestamp}.{body}`, where `timestamp` is the `X-CodoSEO-Timestamp` header (unix
//! seconds) and `body` the raw request body. Reject timestamps that are too old to stop replays.

use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::Sha256;

use crate::message::AlertMessage;

pub const SIGNATURE_HEADER: &str = "X-CodoSEO-Signature";
pub const TIMESTAMP_HEADER: &str = "X-CodoSEO-Timestamp";

/// `{event, site, dashboard_url, crawl_id, changes: [...], more}`. `event` is `changes`,
/// `unreachable` or `test`; `crawl_id` is null for a test.
pub fn payload(msg: &AlertMessage) -> Value {
    let changes: Vec<Value> = msg
        .items
        .iter()
        .map(|i| {
            json!({
                "severity": i.severity,
                "kind": i.kind,
                "label": i.kind_label,
                "url": i.url,
                "before": i.before,
                "after": i.after,
            })
        })
        .collect();
    json!({
        "event": msg.kind.as_str(),
        "site": msg.site_domain,
        "dashboard_url": msg.site_url,
        "crawl_id": msg.crawl_id,
        "changes": changes,
        "more": msg.more,
    })
}

fn mac(secret: &str, timestamp: u64, body: &[u8]) -> Hmac<Sha256> {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    mac
}

/// `sha256=<lowercase hex>` over `{timestamp}.{body}`.
pub fn sign(secret: &str, timestamp: u64, body: &[u8]) -> String {
    format!(
        "sha256={}",
        hex::encode(mac(secret, timestamp, body).finalize().into_bytes())
    )
}

/// Checks a signature header value in constant time.
pub fn verify(secret: &str, timestamp: u64, body: &[u8], signature: &str) -> bool {
    let Some(hex_sig) = signature.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(bytes) = hex::decode(hex_sig) else {
        return false;
    };
    mac(secret, timestamp, body).verify_slice(&bytes).is_ok()
}
