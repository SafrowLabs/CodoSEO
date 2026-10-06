//! Who is calling the API: an API key in `Authorization: Bearer <key>`, and nothing else. The
//! session cookie never authenticates the API (so no request can ride on a browser login), and a
//! key that is malformed or revoked is refused, never treated as "no key".

use axum::extract::FromRequestParts;
use axum::http::HeaderMap;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use codoseo_store::accounts::Account;
use uuid::Uuid;

use super::error::AgentError;
use super::keys;
use crate::metrics::{self, Surface, Tier};
use crate::state::AppState;

/// A caller holding a live API key: the key's account (as loaded for this request) and the key.
#[derive(Debug, Clone)]
pub struct ApiCaller {
    pub account: Account,
    pub key_id: Uuid,
}

const NO_KEY: &str = "Missing API key. Send it as the header \"Authorization: Bearer <key>\".";
/// The same words for a malformed, an unknown and a revoked key, so nothing says which it was.
const BAD_KEY: &str = "That API key is not valid, or it was revoked.";

/// The key in the `Authorization` header: `Ok(None)` when the header is absent, an error when
/// it is there but isn't a well-formed `Bearer` key.
pub fn bearer_key(headers: &HeaderMap) -> Result<Option<&str>, AgentError> {
    let Some(value) = headers.get(AUTHORIZATION) else {
        return Ok(None);
    };
    let bad = || AgentError::Unauthorized(BAD_KEY.to_owned());
    let value = value.to_str().map_err(|_| bad())?;
    let (scheme, key) = value.split_once(' ').ok_or_else(bad)?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(AgentError::Unauthorized(
            "Use the header \"Authorization: Bearer <key>\".".to_owned(),
        ));
    }
    let key = key.trim();
    if keys::is_well_formed(key) {
        Ok(Some(key))
    } else {
        Err(bad())
    }
}

/// The caller behind a well-formed key: unknown and revoked keys are refused.
pub async fn resolve_key(state: &AppState, key: &str) -> Result<ApiCaller, AgentError> {
    match codoseo_store::api_keys::authenticate(&state.pool, &keys::hash_key(key)).await? {
        Some((key_id, account)) => Ok(ApiCaller { account, key_id }),
        None => Err(AgentError::Unauthorized(BAD_KEY.to_owned())),
    }
}

/// The caller of a request that must carry a key.
pub async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<ApiCaller, AgentError> {
    match bearer_key(headers)? {
        Some(key) => resolve_key(state, key).await,
        None => Err(AgentError::Unauthorized(NO_KEY.to_owned())),
    }
}

/// A handler argument that requires an API key. Rejects with the JSON 401.
impl FromRequestParts<AppState> for ApiCaller {
    type Rejection = AgentError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<ApiCaller, AgentError> {
        let caller = authenticate(state, &parts.headers).await;
        // A refused key never reaches a handler, so this is where REST counts it.
        if let Err(e) = &caller {
            metrics::api_request(Surface::Rest, Tier::Key, e.code());
        }
        caller
    }
}
