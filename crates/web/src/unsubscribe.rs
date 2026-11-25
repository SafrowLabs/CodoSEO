//! Unsubscribe links: `/email/unsubscribe/{token}`, where the token names the address and
//! what to stop, signed with a key derived from `SECRET_KEY`. Nothing is stored when the link
//! is made, so a link in a digest from last year still works, and a token can't be forged for
//! someone else's address.
//!
//! The GET shows a page with a button (mail scanners open every link); the POST, from that
//! button or from a mail client's own unsubscribe button (RFC 8058 one-click), records the
//! suppression. The route is in [`crate::routes::email`].

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use codoseo_store::suppressions::Scope;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;
use uuid::Uuid;

pub const PATH_PREFIX: &str = "/email/unsubscribe";

/// What a link stops, for whom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsubscribe {
    /// The address as it was emailed (shown back to the person); the suppression is stored
    /// under its canonical form.
    pub address: String,
    pub scope: Scope,
    /// The account the email was about, when there is one.
    pub account_id: Option<Uuid>,
}

#[derive(Serialize, Deserialize)]
struct Claims {
    a: String,
    s: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    u: Option<Uuid>,
}

/// Makes and checks tokens. Derive it once per process from `SECRET_KEY`.
#[derive(Clone)]
pub struct Signer {
    key: [u8; 32],
}

impl Signer {
    pub fn derive(secret: &str) -> Signer {
        Signer {
            key: Sha256::digest(format!("codoseo.unsubscribe:{secret}").as_bytes()).into(),
        }
    }

    fn mac(&self, claims: &[u8]) -> Vec<u8> {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.key).expect("HMAC accepts keys of any length");
        mac.update(claims);
        mac.finalize().into_bytes().to_vec()
    }

    /// `<claims>.<signature>`, both URL-safe base64 without padding.
    pub fn token(&self, u: &Unsubscribe) -> String {
        let claims = serde_json::to_vec(&Claims {
            a: u.address.clone(),
            s: u.scope.as_str().to_owned(),
            u: u.account_id,
        })
        .expect("claims serialize");
        format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(&claims),
            URL_SAFE_NO_PAD.encode(self.mac(&claims))
        )
    }

    /// The full link under `base_url`.
    pub fn link(&self, base_url: &Url, u: &Unsubscribe) -> Url {
        let mut link = base_url.clone();
        link.set_path(&format!("{PATH_PREFIX}/{}", self.token(u)));
        link.set_query(None);
        link
    }

    /// The claims behind a token, if the signature is ours. The comparison is constant-time.
    pub fn parse(&self, token: &str) -> Option<Unsubscribe> {
        let token = token.trim();
        if token.len() > 1024 {
            return None;
        }
        let (claims, signature) = token.split_once('.')?;
        let claims = URL_SAFE_NO_PAD.decode(claims).ok()?;
        let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.key).expect("HMAC accepts keys of any length");
        mac.update(&claims);
        mac.verify_slice(&signature).ok()?;
        let c: Claims = serde_json::from_slice(&claims).ok()?;
        Some(Unsubscribe {
            address: c.a,
            scope: Scope::parse(&c.s)?,
            account_id: c.u,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims() -> Unsubscribe {
        Unsubscribe {
            address: "Ana+seo@Example.com".into(),
            scope: Scope::Notifications,
            account_id: Some(Uuid::nil()),
        }
    }

    #[test]
    fn a_token_round_trips_and_sits_in_the_link_path() {
        let signer = Signer::derive("k");
        let u = claims();
        assert_eq!(signer.parse(&signer.token(&u)), Some(u.clone()));
        let link = signer.link(&Url::parse("https://codoseo.com/").unwrap(), &u);
        assert!(
            link.as_str()
                .starts_with("https://codoseo.com/email/unsubscribe/"),
            "{link}"
        );
        assert_eq!(link.query(), None);
        let token = link.path().rsplit('/').next().unwrap();
        assert!(!token.contains('='), "no padding in the path: {token}");
        assert_eq!(signer.parse(token), Some(u));
        let no_account = Unsubscribe {
            account_id: None,
            scope: Scope::All,
            ..claims()
        };
        assert_eq!(signer.parse(&signer.token(&no_account)), Some(no_account));
    }

    #[test]
    fn a_tampered_or_foreign_token_is_refused() {
        let signer = Signer::derive("k");
        let token = signer.token(&claims());
        assert!(Signer::derive("other").parse(&token).is_none());
        // Another address under our own signature: the claims change, the MAC doesn't match.
        let (_, sig) = token.split_once('.').unwrap();
        let forged = format!(
            "{}.{sig}",
            URL_SAFE_NO_PAD.encode(br#"{"a":"victim@example.com","s":"all"}"#)
        );
        assert!(signer.parse(&forged).is_none());
        assert!(signer.parse("").is_none());
        assert!(signer.parse("nodot").is_none());
        assert!(signer.parse("not base64!.x").is_none());
        assert!(signer.parse(&"a".repeat(2000)).is_none());
    }
}
