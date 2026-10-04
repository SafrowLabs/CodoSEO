//! Random tokens, their hashes, and the session cookie.
//!
//! Spec section 8: sessions are random IDs in HttpOnly / Secure / SameSite=Lax cookies, stored
//! hashed. The same 32-byte random token and SHA-256 hash serve magic links.

use axum::http::{HeaderMap, HeaderValue, header};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::AppError;
use crate::state::AppState;

pub const SESSION_COOKIE: &str = "codoseo_session";
pub const SESSION_TTL: time::Duration = time::Duration::days(30);

/// 32 random bytes as URL-safe base64 (43 characters).
pub fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// What gets stored instead of the token itself.
pub fn hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// Reads one cookie from the request.
pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_owned())
}

/// A `Set-Cookie` value. `max_age` of zero clears the cookie.
pub fn set_cookie(name: &str, value: &str, max_age_secs: i64, secure: bool) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{name}={value}; Path=/; Max-Age={max_age_secs}; HttpOnly; SameSite=Lax{secure}"
    ))
    .expect("cookie values are base64 or empty")
}

/// Creates a session for the account and returns the `Set-Cookie` header value.
pub async fn start(state: &AppState, account_id: Uuid) -> Result<HeaderValue, AppError> {
    let token = random_token();
    codoseo_store::auth::create_session(&state.pool, account_id, &hash(&token), SESSION_TTL)
        .await?;
    codoseo_store::accounts::touch_login(&state.pool, account_id).await?;
    Ok(set_cookie(
        SESSION_COOKIE,
        &token,
        SESSION_TTL.whole_seconds(),
        state.config.secure_cookies(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_and_hash_to_32_bytes() {
        let a = random_token();
        let b = random_token();
        assert_ne!(a, b);
        assert_eq!(a.len(), 43);
        assert_eq!(hash(&a).len(), 32);
        assert_eq!(hash(&a), hash(&a));
    }

    #[test]
    fn reads_a_cookie_among_others() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("theme=dark; codoseo_session=abc; x=1"),
        );
        assert_eq!(cookie(&h, SESSION_COOKIE).as_deref(), Some("abc"));
        assert_eq!(cookie(&h, "missing"), None);
    }

    #[test]
    fn cookie_flags() {
        let v = set_cookie("s", "v", 60, true);
        let s = v.to_str().unwrap();
        assert!(s.contains("HttpOnly") && s.contains("SameSite=Lax") && s.contains("Secure"));
        assert!(
            !set_cookie("s", "v", 60, false)
                .to_str()
                .unwrap()
                .contains("Secure")
        );
    }
}
