//! Channel-target encryption: AES-256-GCM, nonce ‖ ciphertext, key derived from `SECRET_KEY`.

use codoseo_notify::ChannelKey;

#[test]
fn encrypt_then_decrypt_round_trips() {
    let key = ChannelKey::derive("a secret");
    let plain = br#"{"url":"https://hooks.example/abc","secret":"s3"}"#;
    let sealed = key.encrypt(plain);
    assert_eq!(key.decrypt(&sealed).expect("decrypts"), plain);
}

#[test]
fn stored_bytes_are_a_12_byte_nonce_then_ciphertext_and_tag() {
    let key = ChannelKey::derive("a secret");
    let sealed = key.encrypt(b"hello");
    // 12-byte nonce + 5 bytes of ciphertext + 16-byte GCM tag.
    assert_eq!(sealed.len(), 12 + 5 + 16);
    assert!(!sealed.windows(5).any(|w| w == b"hello"));
}

#[test]
fn two_encryptions_of_the_same_text_differ() {
    let key = ChannelKey::derive("a secret");
    assert_ne!(key.encrypt(b"same"), key.encrypt(b"same"));
}

#[test]
fn a_tampered_byte_fails() {
    let key = ChannelKey::derive("a secret");
    let sealed = key.encrypt(b"payload");
    for i in 0..sealed.len() {
        let mut bad = sealed.clone();
        bad[i] ^= 1;
        assert!(key.decrypt(&bad).is_err(), "flipping byte {i} must fail");
    }
}

#[test]
fn a_wrong_key_fails() {
    let sealed = ChannelKey::derive("one").encrypt(b"payload");
    assert!(ChannelKey::derive("two").decrypt(&sealed).is_err());
}

#[test]
fn truncated_input_fails_cleanly() {
    let key = ChannelKey::derive("a secret");
    assert!(key.decrypt(&[]).is_err());
    assert!(key.decrypt(&[0u8; 11]).is_err());
    assert!(key.decrypt(&[0u8; 12]).is_err());
}

#[test]
fn the_same_secret_derives_the_same_key() {
    let sealed = ChannelKey::derive("stable").encrypt(b"x");
    assert_eq!(ChannelKey::derive("stable").decrypt(&sealed).unwrap(), b"x");
}
