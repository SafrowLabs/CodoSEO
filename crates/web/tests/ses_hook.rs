//! `/hooks/ses`: SES bounce and complaint feedback through SNS. A stand-in SNS serves the
//! signing certificate and takes the subscription confirmation; messages are signed here with
//! the certificate's key, so their URLs can point at the stand-in.

mod support;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use codoseo_store::suppressions;
use codoseo_testkit::TestServer;
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde_json::{Value, json};
use support::{TestApp, TestResponse, cloud_config, cloud_config_with};

const TOPIC: &str = "arn:aws:sns:eu-north-1:123456789012:codoseo-ses-feedback";
const CERT: &[u8] = include_bytes!("data/sns/cert.pem");
const KEY: &[u8] = include_bytes!("data/sns/key.der");

type Confirmed = Arc<Mutex<Vec<String>>>;

/// Serves the signing certificate and records subscription confirmations.
async fn fake_sns() -> (TestServer, Confirmed) {
    async fn cert() -> &'static [u8] {
        CERT
    }
    async fn confirm(
        State(seen): State<Confirmed>,
        Query(q): Query<HashMap<String, String>>,
    ) -> StatusCode {
        seen.lock()
            .unwrap()
            .push(q.get("Token").cloned().unwrap_or_default());
        StatusCode::OK
    }
    let seen: Confirmed = Arc::default();
    let router = Router::new()
        .route("/SimpleNotificationService-test.pem", get(cert))
        .route("/", get(confirm))
        .with_state(seen.clone());
    (TestServer::start(router).await, seen)
}

async fn app_with(sns: &TestServer) -> TestApp {
    TestApp::with_config(cloud_config_with(&[
        ("SES_SNS_TOPIC_ARN", TOPIC),
        ("SNS_BASE_URL", &sns.base()),
    ]))
    .await
}

fn sign(canonical: &str) -> String {
    let key = RsaKeyPair::from_pkcs8(KEY).expect("test key");
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        canonical.as_bytes(),
        &mut signature,
    )
    .unwrap();
    STANDARD.encode(signature)
}

fn cert_url(sns: &TestServer) -> String {
    format!("{}/SimpleNotificationService-test.pem", sns.base())
}

/// A signed `Notification` carrying `message`, as SNS posts it.
fn notification(sns: &TestServer, message: &str) -> Value {
    let canonical = format!(
        "Message\n{message}\nMessageId\nm-1\nTimestamp\n2026-11-25T13:00:00.000Z\n\
         TopicArn\n{TOPIC}\nType\nNotification\n"
    );
    json!({
        "Type": "Notification",
        "MessageId": "m-1",
        "TopicArn": TOPIC,
        "Message": message,
        "Timestamp": "2026-11-25T13:00:00.000Z",
        "SignatureVersion": "2",
        "Signature": sign(&canonical),
        "SigningCertURL": cert_url(sns),
        "UnsubscribeURL": format!("{}/?Action=Unsubscribe", sns.base()),
    })
}

fn confirmation(sns: &TestServer) -> Value {
    let url = format!(
        "{}/?Action=ConfirmSubscription&TopicArn={TOPIC}&Token=tok-1",
        sns.base()
    );
    let message = "You have chosen to subscribe to the topic.";
    let canonical = format!(
        "Message\n{message}\nMessageId\nm-0\nSubscribeURL\n{url}\nTimestamp\n\
         2026-11-25T12:59:00.000Z\nToken\ntok-1\nTopicArn\n{TOPIC}\nType\n\
         SubscriptionConfirmation\n"
    );
    json!({
        "Type": "SubscriptionConfirmation",
        "MessageId": "m-0",
        "Token": "tok-1",
        "TopicArn": TOPIC,
        "Message": message,
        "SubscribeURL": url,
        "Timestamp": "2026-11-25T12:59:00.000Z",
        "SignatureVersion": "2",
        "Signature": sign(&canonical),
        "SigningCertURL": cert_url(sns),
    })
}

fn ses_event(kind: &str, detail: Value) -> String {
    let mut event = json!({
        "notificationType": kind,
        "mail": { "messageId": "0100-abc", "destination": ["x@example.com"] }
    });
    event[kind.to_ascii_lowercase()] = detail;
    event.to_string()
}

fn bounce(bounce_type: &str, addresses: &[&str]) -> String {
    ses_event(
        "Bounce",
        json!({
            "bounceType": bounce_type,
            "bounceSubType": "General",
            "bouncedRecipients": addresses.iter().map(|a| json!({ "emailAddress": a })).collect::<Vec<_>>(),
        }),
    )
}

fn complaint(feedback: Option<&str>, addresses: &[&str]) -> String {
    let mut detail = json!({
        "complainedRecipients": addresses.iter().map(|a| json!({ "emailAddress": a })).collect::<Vec<_>>(),
    });
    if let Some(f) = feedback {
        detail["complaintFeedbackType"] = json!(f);
    }
    ses_event("Complaint", detail)
}

/// Posts as SNS does: `text/plain`, from Amazon's servers, no `Origin` of ours.
async fn post(app: &TestApp, envelope: &Value) -> TestResponse {
    app.post_with_headers(
        "/hooks/ses",
        &envelope.to_string(),
        None,
        &[
            ("content-type", "text/plain; charset=UTF-8"),
            ("origin", "https://sns.amazonaws.com"),
            ("user-agent", "Amazon Simple Notification Service Agent"),
        ],
    )
    .await
}

async fn reason(app: &TestApp, canonical: &str) -> Option<(String, String)> {
    suppressions::get(app.pool(), canonical)
        .await
        .unwrap()
        .map(|s| (s.scope.as_str().to_owned(), s.reason))
}

async fn count(app: &TestApp) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM email_suppressions")
        .fetch_one(app.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn the_subscription_is_confirmed_by_opening_the_subscribe_url() {
    let (sns, confirmed) = fake_sns().await;
    let app = app_with(&sns).await;
    let res = post(&app, &confirmation(&sns)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(confirmed.lock().unwrap().as_slice(), ["tok-1"]);

    // A SubscribeURL elsewhere is never opened, even under a valid signature.
    let mut elsewhere = confirmation(&sns);
    let url = "https://evil.example/?Action=ConfirmSubscription&Token=tok-2";
    let canonical = format!(
        "Message\nYou have chosen to subscribe to the topic.\nMessageId\nm-0\nSubscribeURL\n{url}\n\
         Timestamp\n2026-11-25T12:59:00.000Z\nToken\ntok-1\nTopicArn\n{TOPIC}\nType\n\
         SubscriptionConfirmation\n"
    );
    elsewhere["SubscribeURL"] = json!(url);
    elsewhere["Signature"] = json!(sign(&canonical));
    let res = post(&app, &elsewhere).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.body);
    assert_eq!(confirmed.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn permanent_bounces_and_complaints_suppress_every_email_to_the_address() {
    let (sns, _) = fake_sns().await;
    let app = app_with(&sns).await;

    let res = post(
        &app,
        &notification(
            &sns,
            &bounce(
                "Permanent",
                &["Bounced.Person+x@Example.com", "\"Ana\" <ana@example.com>"],
            ),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(
        reason(&app, "bounced.person@example.com").await,
        Some(("all".into(), "bounce".into())),
        "stored under the canonical address"
    );
    assert_eq!(
        reason(&app, "ana@example.com").await,
        Some(("all".into(), "bounce".into()))
    );

    let res = post(
        &app,
        &notification(&sns, &complaint(Some("abuse"), &["angry@yahoo.com"])),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(
        reason(&app, "angry@yahoo.com").await,
        Some(("all".into(), "complaint".into()))
    );
    // A complaint without a feedback type (some providers send none) still counts.
    post(
        &app,
        &notification(&sns, &complaint(None, &["silent@example.com"])),
    )
    .await;
    assert!(reason(&app, "silent@example.com").await.is_some());
    assert_eq!(count(&app).await, 4);

    // From now on the sign-in form sends nothing to the complainer and says why.
    let res = app.post_hx("/login", "email=Angry%40yahoo.com", None).await;
    assert!(
        res.body.contains("asked us not to email it"),
        "{}",
        res.body
    );
    assert!(app.mail.lock().unwrap().is_empty());

    // Their alert settings say what happened, with the way back.
    let (_account, cookie) = app.login("angry@yahoo.com").await;
    let page = app.get("/settings/alerts", Some(&cookie)).await.body;
    assert!(page.contains("reported a CodoSEO email as spam"), "{page}");
    assert!(page.contains("Turn email back on"), "{page}");
}

#[tokio::test]
async fn transient_bounces_deliveries_and_not_spam_reports_change_nothing() {
    let (sns, _) = fake_sns().await;
    let app = app_with(&sns).await;
    for message in [
        bounce("Transient", &["full@example.com"]),
        complaint(Some("not-spam"), &["sorry@example.com"]),
        ses_event("Delivery", json!({ "recipients": ["fine@example.com"] })),
        json!({ "notificationType": "Bounce" }).to_string(),
        "not even JSON".to_owned(),
    ] {
        let res = post(&app, &notification(&sns, &message)).await;
        assert_eq!(res.status, StatusCode::OK, "{message}: {}", res.body);
    }
    assert_eq!(count(&app).await, 0);
}

#[tokio::test]
async fn forged_messages_are_refused_and_change_nothing() {
    let (sns, confirmed) = fake_sns().await;
    let app = app_with(&sns).await;
    let good = notification(&sns, &bounce("Permanent", &["victim@example.com"]));

    // Another topic, even with a signature that would otherwise be ours.
    let mut other_topic = good.clone();
    other_topic["TopicArn"] = json!("arn:aws:sns:eu-north-1:123456789012:other");
    assert_eq!(
        post(&app, &other_topic).await.status,
        StatusCode::UNAUTHORIZED
    );
    // A changed message under the old signature.
    let mut tampered = good.clone();
    tampered["Message"] = json!(bounce("Permanent", &["someone-else@example.com"]));
    assert_eq!(post(&app, &tampered).await.status, StatusCode::UNAUTHORIZED);
    // A certificate from anywhere but SNS.
    let mut own_cert = good.clone();
    own_cert["SigningCertURL"] = json!("https://evil.example/cert.pem");
    assert_eq!(post(&app, &own_cert).await.status, StatusCode::UNAUTHORIZED);
    let mut not_pem = good.clone();
    not_pem["SigningCertURL"] = json!(format!("{}/cert.txt", sns.base()));
    assert_eq!(post(&app, &not_pem).await.status, StatusCode::UNAUTHORIZED);
    // Not an SNS message at all.
    let res = app
        .post_with_headers(
            "/hooks/ses",
            "{\"hello\": 1}",
            None,
            &[("origin", "https://x.example")],
        )
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(count(&app).await, 0);
    assert!(confirmed.lock().unwrap().is_empty());

    // The real one still works, which also shows the certificate was cached from the refusals
    // above without harm.
    assert_eq!(post(&app, &good).await.status, StatusCode::OK);
    assert_eq!(count(&app).await, 1);
}

#[tokio::test]
async fn without_a_topic_the_hook_does_not_exist() {
    let (sns, _) = fake_sns().await;
    let app = TestApp::with_config(cloud_config()).await;
    let res = post(
        &app,
        &notification(&sns, &bounce("Permanent", &["x@example.com"])),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}
