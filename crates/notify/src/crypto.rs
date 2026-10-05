//! Encryption for channel targets (webhook URLs and their secrets).
//!
//! AES-256-GCM with a key derived from `SECRET_KEY`; the key itself is never stored. The stored
//! form is a random 12-byte nonce followed by the ciphertext (which ends in the 16-byte tag).

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::Sha256;

/// The fixed label the channel key is derived under, so the same `SECRET_KEY` can serve other
/// purposes later without sharing a key.
const KEY_LABEL: &[u8] = b"codoseo channel key v1";
const NONCE_LEN: usize = 12;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("the stored value is too short to be valid")]
    TooShort,
    #[error("the stored value could not be decrypted (wrong key or damaged data)")]
    Decrypt,
}

/// The AES-256-GCM key for channel targets.
#[derive(Clone)]
pub struct ChannelKey {
    cipher: Aes256Gcm,
}

impl std::fmt::Debug for ChannelKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChannelKey(..)")
    }
}

impl ChannelKey {
    /// HMAC-SHA256 of the fixed label, keyed by `secret_key`: 32 bytes.
    pub fn derive(secret_key: &str) -> ChannelKey {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret_key.as_bytes())
            .expect("HMAC accepts keys of any length");
        mac.update(KEY_LABEL);
        let key = mac.finalize().into_bytes();
        ChannelKey {
            cipher: Aes256Gcm::new_from_slice(&key).expect("HMAC-SHA256 output is 32 bytes"),
        }
    }

    /// Nonce (12 random bytes) followed by the ciphertext and tag.
    pub fn encrypt(&self, plaintext: &[u8]) -> Vec<u8> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::try_from(&nonce_bytes[..]).expect("nonce is 12 bytes");
        let sealed = self
            .cipher
            .encrypt(&nonce, plaintext)
            .expect("AES-GCM encryption of an in-memory buffer cannot fail");
        let mut out = Vec::with_capacity(NONCE_LEN + sealed.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&sealed);
        out
    }

    /// Reverses [`ChannelKey::encrypt`]; fails on a wrong key or any changed byte.
    pub fn decrypt(&self, bytes: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if bytes.len() <= NONCE_LEN {
            return Err(CryptoError::TooShort);
        }
        let (nonce_bytes, sealed) = bytes.split_at(NONCE_LEN);
        let nonce = Nonce::try_from(nonce_bytes).map_err(|_| CryptoError::TooShort)?;
        self.cipher
            .decrypt(&nonce, sealed)
            .map_err(|_| CryptoError::Decrypt)
    }
}
