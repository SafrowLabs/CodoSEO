//! Amazon SNS message authentication, for the SES feedback hook ([`crate::routes::ses_hook`]).
//!
//! Every message SNS posts carries its `SigningCertURL` and a `Signature` over a canonical
//! string of its fields (RSA PKCS#1 v1.5, SHA-1 for `SignatureVersion` 1 and SHA-256 for 2).
//! The certificate may only be fetched from an `sns.<region>.amazonaws.com` address, or else
//! anyone could sign a message with a key of their own and point us at it.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ring::signature::{
    RSA_PKCS1_2048_8192_SHA1_FOR_LEGACY_USE_ONLY, RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey,
};
use serde::Deserialize;
use url::Url;
use x509_cert::Certificate;
use x509_cert::der::DecodePem;

/// An SNS message as posted to an HTTPS subscription (raw message delivery off).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Envelope {
    /// `Notification`, `SubscriptionConfirmation` or `UnsubscribeConfirmation`.
    #[serde(rename = "Type")]
    pub kind: String,
    pub message_id: String,
    pub topic_arn: String,
    #[serde(default)]
    pub subject: Option<String>,
    pub message: String,
    pub timestamp: String,
    pub signature_version: String,
    pub signature: String,
    #[serde(rename = "SigningCertURL")]
    pub signing_cert_url: String,
    #[serde(rename = "SubscribeURL", default)]
    pub subscribe_url: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SnsError {
    #[error("the message type {0:?} has no canonical form")]
    Kind(String),
    #[error("a confirmation message without SubscribeURL or Token")]
    Incomplete,
    #[error("unsupported SignatureVersion {0:?}")]
    Version(String),
    #[error("the signature is not base64")]
    Encoding,
    #[error("the signing certificate can't be read: {0}")]
    Certificate(String),
    #[error("the signature does not match")]
    Mismatch,
    #[error("{0}")]
    Address(String),
}

impl Envelope {
    /// The string SNS signed: `Name\nValue\n` for a fixed set of fields, in order.
    pub fn canonical(&self) -> Result<String, SnsError> {
        let mut out = String::new();
        let mut push = |name: &str, value: &str| {
            out.push_str(name);
            out.push('\n');
            out.push_str(value);
            out.push('\n');
        };
        match self.kind.as_str() {
            "Notification" => {
                push("Message", &self.message);
                push("MessageId", &self.message_id);
                if let Some(subject) = &self.subject {
                    push("Subject", subject);
                }
                push("Timestamp", &self.timestamp);
                push("TopicArn", &self.topic_arn);
                push("Type", &self.kind);
            }
            "SubscriptionConfirmation" | "UnsubscribeConfirmation" => {
                let (Some(url), Some(token)) = (&self.subscribe_url, &self.token) else {
                    return Err(SnsError::Incomplete);
                };
                push("Message", &self.message);
                push("MessageId", &self.message_id);
                push("SubscribeURL", url);
                push("Timestamp", &self.timestamp);
                push("Token", token);
                push("TopicArn", &self.topic_arn);
                push("Type", &self.kind);
            }
            other => return Err(SnsError::Kind(other.to_owned())),
        }
        Ok(out)
    }
}

/// Checks that `raw` is an address SNS itself would use: `https://sns.<region>.amazonaws.com/`
/// (or `.amazonaws.com.cn`), or anything under `base` when one is set (tests point it at a
/// stand-in). With `pem`, the path must name a `.pem` file, as a signing certificate does.
pub fn aws_url(raw: &str, base: Option<&Url>, pem: bool) -> Result<Url, SnsError> {
    let bad = |why: &str| SnsError::Address(format!("{why}: {raw:?}"));
    let url = Url::parse(raw).map_err(|_| bad("not a URL"))?;
    if pem && !url.path().ends_with(".pem") {
        return Err(bad("the signing certificate URL does not name a .pem file"));
    }
    if let Some(base) = base {
        if !url.as_str().starts_with(base.as_str()) {
            return Err(bad("not under the configured SNS address"));
        }
        return Ok(url);
    }
    if url.scheme() != "https" {
        return Err(bad("not https"));
    }
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let region = host
        .strip_prefix("sns.")
        .and_then(|rest| {
            rest.strip_suffix(".amazonaws.com")
                .or_else(|| rest.strip_suffix(".amazonaws.com.cn"))
        })
        .filter(|region| {
            !region.is_empty()
                && region
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        });
    if region.is_none() {
        return Err(bad("not an sns.<region>.amazonaws.com address"));
    }
    Ok(url)
}

/// Checks `envelope`'s signature against the certificate at its `SigningCertURL` (PEM).
pub fn verify(cert_pem: &[u8], envelope: &Envelope) -> Result<(), SnsError> {
    let canonical = envelope.canonical()?;
    let algorithm = match envelope.signature_version.as_str() {
        "1" => &RSA_PKCS1_2048_8192_SHA1_FOR_LEGACY_USE_ONLY,
        "2" => &RSA_PKCS1_2048_8192_SHA256,
        other => return Err(SnsError::Version(other.to_owned())),
    };
    let signature = STANDARD
        .decode(envelope.signature.trim())
        .map_err(|_| SnsError::Encoding)?;
    let cert = Certificate::from_pem(cert_pem).map_err(|e| SnsError::Certificate(e.to_string()))?;
    // The BIT STRING inside the SubjectPublicKeyInfo is the PKCS#1 RSAPublicKey ring reads.
    let key = cert
        .tbs_certificate
        .subject_public_key_info
        .subject_public_key
        .as_bytes()
        .ok_or_else(|| SnsError::Certificate("the public key has unused bits".to_owned()))?;
    UnparsedPublicKey::new(algorithm, key)
        .verify(canonical.as_bytes(), &signature)
        .map_err(|_| SnsError::Mismatch)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CERT: &[u8] = include_bytes!("../tests/data/sns/cert.pem");

    fn fixture(name: &str) -> Envelope {
        let path = format!("{}/tests/data/sns/{name}", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn signatures_made_by_openssl_verify_for_both_versions() {
        // SignatureVersion 2 (SHA-256), with and without a Subject, and a confirmation.
        for name in [
            "bounce_permanent.json",
            "delivery.json",
            "subscription_confirmation.json",
        ] {
            verify(CERT, &fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
        // SignatureVersion 1 (SHA-1) with a Subject.
        let v1 = fixture("complaint.json");
        assert_eq!(v1.signature_version, "1");
        assert!(v1.subject.is_some());
        verify(CERT, &v1).unwrap();
    }

    #[test]
    fn a_changed_field_a_wrong_key_or_a_wrong_version_fails() {
        let good = fixture("bounce_permanent.json");
        let mut tampered = good.clone();
        tampered.message = tampered.message.replace("Permanent", "Transient");
        assert!(matches!(verify(CERT, &tampered), Err(SnsError::Mismatch)));
        let mut other_topic = good.clone();
        other_topic.topic_arn = "arn:aws:sns:eu-north-1:1:other".into();
        assert!(matches!(
            verify(CERT, &other_topic),
            Err(SnsError::Mismatch)
        ));
        let mut version = good.clone();
        version.signature_version = "1".into();
        assert!(matches!(verify(CERT, &version), Err(SnsError::Mismatch)));
        let mut unknown = good.clone();
        unknown.signature_version = "3".into();
        assert!(matches!(verify(CERT, &unknown), Err(SnsError::Version(_))));
        let mut garbage = good.clone();
        garbage.signature = "not base64!".into();
        assert!(matches!(verify(CERT, &garbage), Err(SnsError::Encoding)));
        let mut kind = good;
        kind.kind = "Something".into();
        assert!(matches!(verify(CERT, &kind), Err(SnsError::Kind(_))));
        assert!(matches!(
            verify(
                b"-----BEGIN CERTIFICATE-----\nnope\n-----END CERTIFICATE-----\n",
                &fixture("delivery.json")
            ),
            Err(SnsError::Certificate(_))
        ));
    }

    #[test]
    fn only_sns_addresses_may_serve_the_certificate() {
        for ok in [
            "https://sns.eu-north-1.amazonaws.com/SimpleNotificationService-abc.pem",
            "https://sns.us-east-1.amazonaws.com/SimpleNotificationService-abc.pem",
            "https://sns.cn-north-1.amazonaws.com.cn/SimpleNotificationService-abc.pem",
        ] {
            aws_url(ok, None, true).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for bad in [
            "http://sns.eu-north-1.amazonaws.com/x.pem",
            "https://sns.eu-north-1.amazonaws.com.evil.example/x.pem",
            "https://evil.example/sns.eu-north-1.amazonaws.com/x.pem",
            "https://sns.eu-north-1.amazonaws.com/x.txt",
            "https://sns..amazonaws.com/x.pem",
            "https://sns.EU-north_1.amazonaws.com/x.pem",
            "not a url",
        ] {
            assert!(aws_url(bad, None, true).is_err(), "{bad}");
        }
        // A SubscribeURL has no .pem; a configured base replaces the host rule.
        aws_url(
            "https://sns.eu-north-1.amazonaws.com/?Action=ConfirmSubscription&Token=x",
            None,
            false,
        )
        .unwrap();
        let base = Url::parse("http://127.0.0.1:9/sns/").unwrap();
        aws_url("http://127.0.0.1:9/sns/cert.pem", Some(&base), true).unwrap();
        assert!(aws_url("http://127.0.0.1:9/other/cert.pem", Some(&base), true).is_err());
        assert!(
            aws_url(
                "https://sns.eu-north-1.amazonaws.com/x.pem",
                Some(&base),
                true
            )
            .is_err()
        );
    }
}
