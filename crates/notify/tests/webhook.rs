//! Signing, and delivery to a local axum server: exact body and verifiable signature, the
//! address guard at delivery (nothing reaches a blocked target), no redirects, errors on non-2xx.

use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::any;
use codoseo_core::check::Severity;
use codoseo_core::crawl::AddressPolicy;
use codoseo_crawler::guard::Lookup;
use codoseo_notify::deliver::{ChannelTarget, DeliveryError, GuardedHttp, deliver};
use codoseo_notify::message::{AlertItem, AlertMessage};
use codoseo_notify::{Mailer, webhook};
use uuid::Uuid;

#[test]
fn sign_then_verify_succeeds_and_any_change_fails() {
    let body = br#"{"event":"changes"}"#;
    let sig = webhook::sign("s3cr3t", 1_700_000_000, body);
    assert!(sig.starts_with("sha256="), "{sig}");
    assert_eq!(sig.len(), "sha256=".len() + 64);
    assert!(
        sig[7..]
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
    assert!(webhook::verify("s3cr3t", 1_700_000_000, body, &sig));

    assert!(!webhook::verify(
        "s3cr3t",
        1_700_000_000,
        br#"{"event":"changed"}"#,
        &sig
    ));
    assert!(!webhook::verify("s3cr3t", 1_700_000_001, body, &sig));
    assert!(!webhook::verify("other", 1_700_000_000, body, &sig));
    assert!(!webhook::verify("s3cr3t", 1_700_000_000, body, "sha256=zz"));
    assert!(!webhook::verify("s3cr3t", 1_700_000_000, body, "nonsense"));
}

#[test]
fn the_signature_covers_timestamp_dot_body() {
    // HMAC-SHA256("key", "1.abc") computed independently with openssl.
    assert_eq!(
        webhook::sign("key", 1, b"abc"),
        "sha256=90ebc65a2174297bcabd31e7cdb290734ffd97b67d17c09a380b0d4828d3d90c"
    );
}

type Request = (HeaderMap, Vec<u8>);

#[derive(Clone, Default)]
struct Seen {
    requests: Arc<Mutex<Vec<Request>>>,
}

/// A server that records every request and answers `status` (with a `location` for 3xx).
async fn server(status: u16, location: Option<String>) -> (SocketAddr, Seen) {
    let seen = Seen::default();
    let state = seen.clone();
    let app = Router::new().fallback(any(move |headers: HeaderMap, body: Bytes| {
        let state = state.clone();
        let location = location.clone();
        async move {
            state
                .requests
                .lock()
                .unwrap()
                .push((headers, body.to_vec()));
            let mut headers = HeaderMap::new();
            if let Some(l) = location {
                headers.insert(header::LOCATION, l.parse().unwrap());
            }
            (
                StatusCode::from_u16(status).unwrap(),
                headers,
                "oops: the server said no",
            )
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (addr, seen)
}

fn message() -> AlertMessage {
    AlertMessage::changes(
        "example.com",
        "https://codoseo.com/sites/abc",
        Uuid::from_u128(7),
        "1 change on example.com",
        vec![AlertItem {
            severity: Severity::Critical,
            kind: "became_noindex".into(),
            kind_label: "Became noindex".into(),
            url: Some("https://example.com/".into()),
            before: "index".into(),
            after: "noindex".into(),
        }],
    )
}

fn url_for(addr: SocketAddr, path: &str) -> url::Url {
    format!("http://{addr}{path}").parse().unwrap()
}

#[tokio::test]
async fn a_webhook_gets_the_exact_body_and_a_signature_the_server_can_verify() {
    let (addr, seen) = server(200, None).await;
    let http = GuardedHttp::new(AddressPolicy::AllowPrivate).unwrap();
    let target = ChannelTarget::Webhook {
        url: url_for(addr, "/hook"),
        secret: "s3cr3t".into(),
    };
    let msg = message();
    deliver(&http, &Mailer::Log, &target, &msg)
        .await
        .expect("delivered");

    let requests = seen.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let (headers, body) = &requests[0];
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(body).unwrap(),
        webhook::payload(&msg)
    );
    assert_eq!(headers[header::CONTENT_TYPE], "application/json");
    let ts: u64 = headers["x-codoseo-timestamp"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(now.abs_diff(ts) < 60);
    let sig = headers["x-codoseo-signature"].to_str().unwrap();
    assert!(webhook::verify("s3cr3t", ts, body, sig));
    let mut tampered = body.clone();
    tampered.push(b' ');
    assert!(!webhook::verify("s3cr3t", ts, &tampered, sig));
}

#[tokio::test]
async fn slack_and_discord_get_their_json_without_a_signature() {
    let (addr, seen) = server(204, None).await;
    let http = GuardedHttp::new(AddressPolicy::AllowPrivate).unwrap();
    for target in [
        ChannelTarget::Slack {
            url: url_for(addr, "/slack"),
        },
        ChannelTarget::Discord {
            url: url_for(addr, "/discord"),
        },
    ] {
        deliver(&http, &Mailer::Log, &target, &message())
            .await
            .expect("delivered");
    }
    let requests = seen.requests.lock().unwrap();
    let first: serde_json::Value = serde_json::from_slice(&requests[0].1).unwrap();
    let second: serde_json::Value = serde_json::from_slice(&requests[1].1).unwrap();
    assert!(first["blocks"].is_array(), "{first}");
    assert!(second["embeds"].is_array(), "{second}");
    assert!(requests[0].0.get("x-codoseo-signature").is_none());
}

#[tokio::test]
async fn email_goes_through_the_mailer() {
    let (mailer, sent) = Mailer::capture();
    let http = GuardedHttp::new(AddressPolicy::Public).unwrap();
    deliver(
        &http,
        &mailer,
        &ChannelTarget::Email {
            to: "owner@example.com".into(),
        },
        &message(),
    )
    .await
    .unwrap();
    let sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, "owner@example.com");
    assert!(sent[0].html.is_some());
}

#[tokio::test]
async fn a_500_is_a_delivery_error_with_status_and_excerpt() {
    let (addr, _seen) = server(500, None).await;
    let http = GuardedHttp::new(AddressPolicy::AllowPrivate).unwrap();
    let target = ChannelTarget::Slack {
        url: url_for(addr, "/"),
    };
    let err = deliver(&http, &Mailer::Log, &target, &message())
        .await
        .unwrap_err();
    match &err {
        DeliveryError::Status { status, body } => {
            assert_eq!(*status, 500);
            assert!(body.contains("the server said no"), "{body}");
        }
        other => panic!("expected a status error, got {other:?}"),
    }
    assert!(err.to_string().contains("500"));
}

#[tokio::test]
async fn public_policy_refuses_ip_literals_before_any_request() {
    let (addr, seen) = server(200, None).await; // listening on 127.0.0.1
    let http = GuardedHttp::new(AddressPolicy::Public).unwrap();
    for url in [
        url_for(addr, "/"),
        "http://169.254.169.254/latest/meta-data".parse().unwrap(),
        "http://[::1]/".parse().unwrap(),
    ] {
        let target = ChannelTarget::Webhook {
            url,
            secret: "s".into(),
        };
        let err = deliver(&http, &Mailer::Log, &target, &message())
            .await
            .unwrap_err();
        assert!(matches!(err, DeliveryError::Blocked(_)), "{err:?}");
    }
    assert!(seen.requests.lock().unwrap().is_empty());
}

/// Resolves every name to one fixed address.
struct Fixed(IpAddr);

impl Lookup for Fixed {
    fn lookup(&self, _host: &str) -> impl Future<Output = io::Result<Vec<IpAddr>>> + Send {
        let ip = self.0;
        async move { Ok(vec![ip]) }
    }
}

#[tokio::test]
async fn a_hostname_that_resolves_to_a_private_address_is_refused_at_delivery() {
    let (addr, seen) = server(200, None).await;
    // The lookup says the name is 10.0.0.5; nothing may connect to it (nor to the real server).
    let http = GuardedHttp::with_lookup(AddressPolicy::Public, Fixed("10.0.0.5".parse().unwrap()))
        .unwrap();
    let target = ChannelTarget::Webhook {
        url: format!("http://hooks.example.test:{}/", addr.port())
            .parse()
            .unwrap(),
        secret: "s".into(),
    };
    let err = deliver(&http, &Mailer::Log, &target, &message())
        .await
        .unwrap_err();
    assert!(matches!(err, DeliveryError::Request(_)), "{err:?}");
    assert!(seen.requests.lock().unwrap().is_empty());

    // The same name resolving to localhost is just as blocked.
    let http = GuardedHttp::with_lookup(AddressPolicy::Public, Fixed("127.0.0.1".parse().unwrap()))
        .unwrap();
    assert!(
        deliver(&http, &Mailer::Log, &target, &message())
            .await
            .is_err()
    );
    assert!(seen.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_redirect_is_an_error_and_is_never_followed() {
    let (inner, inner_seen) = server(200, None).await;
    let (outer, outer_seen) = server(302, Some(format!("http://{inner}/internal"))).await;
    let http = GuardedHttp::new(AddressPolicy::AllowPrivate).unwrap();
    let target = ChannelTarget::Webhook {
        url: url_for(outer, "/hook"),
        secret: "s".into(),
    };
    let err = deliver(&http, &Mailer::Log, &target, &message())
        .await
        .unwrap_err();
    assert!(
        matches!(err, DeliveryError::Status { status: 302, .. }),
        "{err:?}"
    );
    assert_eq!(outer_seen.requests.lock().unwrap().len(), 1);
    assert!(
        inner_seen.requests.lock().unwrap().is_empty(),
        "the redirect target was contacted"
    );
}

#[test]
fn targets_round_trip_through_json_by_kind() {
    use codoseo_notify::deliver::ChannelKind;
    let cases = [
        (
            ChannelKind::Email,
            serde_json::json!({"to": "a@example.com"}),
        ),
        (
            ChannelKind::Slack,
            serde_json::json!({"url": "https://hooks.slack.com/services/T/B/x"}),
        ),
        (
            ChannelKind::Discord,
            serde_json::json!({"url": "https://discord.com/api/webhooks/1/x"}),
        ),
        (
            ChannelKind::Webhook,
            serde_json::json!({"url": "https://example.com/h", "secret": "s"}),
        ),
    ];
    for (kind, json) in cases {
        let target = ChannelTarget::from_json(kind, &json).expect("parses");
        assert_eq!(target.kind(), kind);
        assert_eq!(target.to_json(), json);
    }
    assert!(ChannelTarget::from_json(ChannelKind::Slack, &serde_json::json!({"to": "x"})).is_err());
    assert!(
        ChannelTarget::from_json(ChannelKind::Slack, &serde_json::json!({"url": "not a url"}))
            .is_err()
    );
}

mod validate {
    use codoseo_core::crawl::AddressPolicy::{AllowPrivate, Public};
    use codoseo_notify::deliver::{ChannelKind, GuardedHttp};

    use codoseo_notify::TargetError;

    use super::Fixed;

    /// Names resolve to a public address, so only the rules under test decide.
    async fn ok(kind: ChannelKind, url: &str, policy: codoseo_core::crawl::AddressPolicy) -> bool {
        resolving_to("93.184.216.34", policy)
            .validate_target(kind, url)
            .await
            .is_ok()
    }

    fn resolving_to(ip: &str, policy: codoseo_core::crawl::AddressPolicy) -> GuardedHttp {
        GuardedHttp::with_lookup(policy, Fixed(ip.parse().unwrap())).unwrap()
    }

    #[tokio::test]
    async fn slack_must_be_hooks_slack_com_over_https() {
        assert!(
            ok(
                ChannelKind::Slack,
                "https://hooks.slack.com/services/T0/B0/xyz",
                Public
            )
            .await
        );
        assert!(
            !ok(
                ChannelKind::Slack,
                "https://example.com/services/T0/B0/xyz",
                Public
            )
            .await
        );
        assert!(
            !ok(
                ChannelKind::Slack,
                "http://hooks.slack.com/services/T0/B0/xyz",
                Public
            )
            .await
        );
        assert!(
            !ok(
                ChannelKind::Slack,
                "https://hooks.slack.com.evil.test/services/x",
                Public
            )
            .await
        );
        assert!(!ok(ChannelKind::Slack, "https://hooks.slack.com/", Public).await);
        assert!(!ok(ChannelKind::Slack, "not a url", Public).await);
        assert!(!ok(ChannelKind::Slack, "https://example.com/x", AllowPrivate).await);
    }

    #[tokio::test]
    async fn discord_must_be_a_discord_webhook_url() {
        assert!(
            ok(
                ChannelKind::Discord,
                "https://discord.com/api/webhooks/1/abc",
                Public
            )
            .await
        );
        assert!(
            ok(
                ChannelKind::Discord,
                "https://discordapp.com/api/webhooks/1/abc",
                Public
            )
            .await
        );
        assert!(
            !ok(
                ChannelKind::Discord,
                "https://discord.com/other/1/abc",
                Public
            )
            .await
        );
        assert!(
            !ok(
                ChannelKind::Discord,
                "https://discord.com.evil.test/api/webhooks/1/abc",
                Public
            )
            .await
        );
    }

    #[tokio::test]
    async fn a_webhook_is_https_and_public_unless_self_hosted() {
        assert!(ok(ChannelKind::Webhook, "https://example.com/hook", Public).await);
        assert!(!ok(ChannelKind::Webhook, "http://example.com/hook", Public).await);
        assert!(
            ok(
                ChannelKind::Webhook,
                "http://localhost:8080/hook",
                AllowPrivate
            )
            .await
        );
        assert!(!ok(ChannelKind::Webhook, "ftp://example.com/hook", AllowPrivate).await);
    }

    #[tokio::test]
    async fn the_cloud_guard_applies_to_every_kind() {
        for url in [
            "https://169.254.169.254/latest/meta-data",
            "https://127.0.0.1/hook",
            "https://[::1]/hook",
            "https://10.0.0.5/hook",
        ] {
            assert!(!ok(ChannelKind::Webhook, url, Public).await, "{url}");
        }
        assert!(ok(ChannelKind::Webhook, "https://10.0.0.5/hook", AllowPrivate).await);
    }

    #[tokio::test]
    async fn email_is_not_a_url_channel() {
        assert!(!ok(ChannelKind::Email, "https://example.com", Public).await);
    }

    #[tokio::test]
    async fn a_host_name_is_resolved_at_save_time_under_public() {
        for (name, ip) in [
            ("hooks.example.test", "10.0.0.5"),
            ("hooks.example.test", "169.254.169.254"),
            ("localhost", "127.0.0.1"),
        ] {
            let http = resolving_to(ip, Public);
            let err = http
                .validate_target(ChannelKind::Webhook, &format!("https://{name}/hook"))
                .await
                .unwrap_err();
            assert!(
                matches!(err, TargetError::Blocked(_)),
                "{name} -> {ip}: {err:?}"
            );
        }
        // The real system resolver refuses localhost too: it only resolves to loopback.
        let http = GuardedHttp::new(Public).unwrap();
        assert!(
            http.validate_target(ChannelKind::Webhook, "https://localhost/hook")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_host_that_does_not_resolve_is_refused_at_save_time() {
        struct Nothing;
        impl codoseo_crawler::guard::Lookup for Nothing {
            async fn lookup(&self, _host: &str) -> std::io::Result<Vec<std::net::IpAddr>> {
                Err(std::io::Error::other("no such host"))
            }
        }
        let http = GuardedHttp::with_lookup(Public, Nothing).unwrap();
        assert!(
            http.validate_target(ChannelKind::Webhook, "https://nope.example.test/hook")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn the_same_names_are_fine_when_self_hosted() {
        let http = resolving_to("10.0.0.5", AllowPrivate);
        assert!(
            http.validate_target(ChannelKind::Webhook, "https://hooks.example.test/hook")
                .await
                .is_ok()
        );
        let http = resolving_to("127.0.0.1", AllowPrivate);
        assert!(
            http.validate_target(ChannelKind::Webhook, "https://localhost/hook")
                .await
                .is_ok()
        );
    }
}
