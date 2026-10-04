//! T6.2: abuse controls on the no-signup audit: per-IP limits with salted hashes, Turnstile,
//! the queue position, disposable email domains and the cap on unlock emails.

mod support;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Form, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use codoseo_testkit::TestServer;
use support::{TestApp, TestResponse, cloud_config, cloud_config_with};
use uuid::Uuid;

const IP_HEADER: &str = "CF-Connecting-IP";

fn encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

async fn submit_from(app: &TestApp, domain: &str, ip: &str) -> TestResponse {
    app.post_with_headers(
        "/audit",
        &format!("url={}", encode(domain)),
        None,
        &[(IP_HEADER, ip)],
    )
    .await
}

async fn count(app: &TestApp, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(app.pool()).await.unwrap()
}

fn crawl_id(res: &TestResponse) -> Uuid {
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    Uuid::parse_str(res.location().unwrap().strip_prefix("/audit/").unwrap()).unwrap()
}

#[tokio::test]
async fn an_ip_is_limited_to_three_audits_an_hour_with_a_friendly_429() {
    let app = TestApp::with_config(cloud_config()).await;
    for i in 0..3 {
        let res = submit_from(&app, &format!("site{i}.com"), "203.0.113.9").await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{i}");
    }
    let res = submit_from(&app, "site3.com", "203.0.113.9").await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
    let retry: i64 = res
        .header("retry-after")
        .expect("Retry-After")
        .parse()
        .unwrap();
    assert!((3590..=3600).contains(&retry), "{retry}");
    assert!(res.body.contains("3 audits an hour"), "{}", res.body);
    assert!(
        res.body.contains("minutes"),
        "tells them when to come back: {}",
        res.body
    );
    assert!(
        res.body.contains(r#"action="/audit""#),
        "the form is still there"
    );
    assert_eq!(count(&app, "SELECT count(*) FROM crawls").await, 3);

    // Someone else, and a site that's already being audited, are both fine.
    assert_eq!(
        submit_from(&app, "site3.com", "198.51.100.4").await.status,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        submit_from(&app, "site0.com", "203.0.113.9").await.status,
        StatusCode::SEE_OTHER
    );
}

#[tokio::test]
async fn an_ip_is_limited_to_ten_audits_a_day() {
    let app = TestApp::with_config(cloud_config()).await;
    for round in 0..3 {
        for i in 0..3 {
            let res = submit_from(&app, &format!("r{round}s{i}.com"), "203.0.113.9").await;
            assert_eq!(res.status, StatusCode::SEE_OTHER, "{round}/{i}");
        }
        sqlx::query("UPDATE crawls SET created_at = now() - interval '2 hours'")
            .execute(app.pool())
            .await
            .unwrap();
    }
    assert_eq!(
        submit_from(&app, "tenth.com", "203.0.113.9").await.status,
        StatusCode::SEE_OTHER
    );
    let res = submit_from(&app, "eleventh.com", "203.0.113.9").await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(res.body.contains("10 audits a day"), "{}", res.body);
    assert!(res.header("retry-after").is_some());
}

#[tokio::test]
async fn the_stored_ip_is_a_salted_daily_hash_never_the_address() {
    let app = TestApp::with_config(cloud_config()).await;
    let crawl = crawl_id(&submit_from(&app, "example.com", "203.0.113.9").await);
    let stored: Vec<u8> = sqlx::query_scalar("SELECT requester_ip_hash FROM crawls WHERE id = $1")
        .bind(crawl)
        .fetch_one(app.pool())
        .await
        .unwrap();
    assert_eq!(stored.len(), 32);
    assert!(!String::from_utf8_lossy(&stored).contains("203.0.113.9"));

    let ip = "203.0.113.9".parse().unwrap();
    let today = time::OffsetDateTime::now_utc().date();
    assert_eq!(
        stored,
        codoseo_web::abuse::ip_hash("test-secret", ip, today)
    );
    let tomorrow = today.next_day().unwrap();
    assert_ne!(
        codoseo_web::abuse::ip_hash("test-secret", ip, today),
        codoseo_web::abuse::ip_hash("test-secret", ip, tomorrow),
        "yesterday's hash can't be matched to today's"
    );
    assert_ne!(
        codoseo_web::abuse::ip_hash("test-secret", ip, today),
        codoseo_web::abuse::ip_hash("another-secret", ip, today)
    );
}

#[tokio::test]
async fn the_waiting_page_shows_the_place_in_line() {
    let app = TestApp::with_config(cloud_config()).await;
    let mut ids = Vec::new();
    for (i, ip) in ["198.51.100.1", "198.51.100.2", "198.51.100.3"]
        .iter()
        .enumerate()
    {
        ids.push(crawl_id(
            &submit_from(&app, &format!("site{i}.com"), ip).await,
        ));
        sqlx::query(
            "UPDATE crawls SET queued_at = now() - ($2 || ' seconds')::interval WHERE id = $1",
        )
        .bind(ids[i])
        .bind((10 - i).to_string())
        .execute(app.pool())
        .await
        .unwrap();
    }
    let third = app.get(&format!("/audit/{}", ids[2]), None).await.body;
    assert!(third.contains("#3 in line"), "{third}");
    let first = app
        .get_hx(&format!("/audit/{}/live", ids[0]), None)
        .await
        .body;
    assert!(first.contains("next in line"), "{first}");

    sqlx::query("UPDATE crawls SET status = 'running', started_at = now() WHERE id = $1")
        .bind(ids[0])
        .execute(app.pool())
        .await
        .unwrap();
    let running = app.get(&format!("/audit/{}", ids[0]), None).await.body;
    assert!(!running.contains("in line"), "{running}");
}

/// A stand-in for Cloudflare's siteverify endpoint: `good-token` passes, anything else fails.
async fn fake_turnstile() -> (TestServer, Arc<Mutex<Vec<HashMap<String, String>>>>) {
    type Seen = Arc<Mutex<Vec<HashMap<String, String>>>>;
    async fn verify(
        State(seen): State<Seen>,
        Form(form): Form<HashMap<String, String>>,
    ) -> Json<serde_json::Value> {
        let ok = form.get("response").map(String::as_str) == Some("good-token");
        seen.lock().unwrap().push(form);
        Json(serde_json::json!({ "success": ok }))
    }
    let seen: Seen = Arc::default();
    let router = Router::new()
        .route("/verify", post(verify))
        .with_state(seen.clone());
    (TestServer::start(router).await, seen)
}

fn turnstile_config(verify_url: &str) -> codoseo_web::Config {
    cloud_config_with(&[
        ("TURNSTILE_SITE_KEY", "site-key-123"),
        ("TURNSTILE_SECRET", "shh-secret"),
        ("TURNSTILE_VERIFY_URL", verify_url),
    ])
}

#[tokio::test]
async fn turnstile_gates_the_form_when_configured() {
    let (server, seen) = fake_turnstile().await;
    let app = TestApp::with_config(turnstile_config(&format!("{}/verify", server.base()))).await;

    // The widget and its script are on the landing page only when keys are set.
    let landing = app.get("/", None).await.body;
    assert!(
        landing.contains("challenges.cloudflare.com/turnstile/v0/api.js"),
        "{landing}"
    );
    assert!(landing.contains(r#"data-sitekey="site-key-123""#));
    assert!(
        !landing.contains("shh-secret"),
        "the secret never reaches the page"
    );

    let post = |token: Option<&str>| {
        let form = match token {
            Some(t) => format!("url=example.com&cf-turnstile-response={t}"),
            None => "url=example.com".to_owned(),
        };
        let app = &app;
        async move {
            app.post_with_headers("/audit", &form, None, &[(IP_HEADER, "203.0.113.9")])
                .await
        }
    };
    let missing = post(None).await;
    assert_eq!(missing.status, StatusCode::FORBIDDEN);
    assert!(missing.body.contains("human"), "{}", missing.body);
    let bad = post(Some("forged")).await;
    assert_eq!(bad.status, StatusCode::FORBIDDEN);
    assert_eq!(
        count(&app, "SELECT count(*) FROM crawls").await,
        0,
        "nothing starts without a pass"
    );

    let good = post(Some("good-token")).await;
    assert_eq!(good.status, StatusCode::SEE_OTHER);
    assert_eq!(count(&app, "SELECT count(*) FROM crawls").await, 1);

    // Cloudflare was sent our secret and the visitor's address; a missing token costs no call.
    let calls = seen.lock().unwrap().clone();
    assert_eq!(
        calls.len(),
        2,
        "forged and good (the missing token never left)"
    );
    assert_eq!(
        calls[1].get("secret").map(String::as_str),
        Some("shh-secret")
    );
    assert_eq!(
        calls[1].get("remoteip").map(String::as_str),
        Some("203.0.113.9")
    );
}

#[tokio::test]
async fn turnstile_fails_closed_when_the_verifier_is_down() {
    let app = TestApp::with_config(turnstile_config("http://127.0.0.1:9/verify")).await;
    let res = app
        .post_with_headers(
            "/audit",
            "url=example.com&cf-turnstile-response=good-token",
            None,
            &[(IP_HEADER, "203.0.113.9")],
        )
        .await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(res.body.contains("try again"), "{}", res.body);
    assert_eq!(count(&app, "SELECT count(*) FROM crawls").await, 0);
}

#[tokio::test]
async fn without_turnstile_keys_the_form_just_works() {
    let app = TestApp::with_config(cloud_config()).await;
    assert_eq!(
        submit_from(&app, "example.com", "203.0.113.9").await.status,
        StatusCode::SEE_OTHER
    );
}

async fn unlock(app: &TestApp, crawl: Uuid, email: &str) -> TestResponse {
    app.post_hx(
        &format!("/audit/{crawl}/unlock"),
        &format!("email={}", encode(email)),
        None,
    )
    .await
}

#[tokio::test]
async fn disposable_email_domains_are_refused_at_unlock() {
    let app = TestApp::with_config(cloud_config()).await;
    let crawl = crawl_id(&submit_from(&app, "example.com", "203.0.113.9").await);
    for address in [
        "x@mailinator.com",
        "X@MAILINATOR.COM",
        "x@sub.mailinator.com",
        "x@guerrillamail.com",
        "x@10minutemail.com",
        "x@yopmail.com",
    ] {
        let res = unlock(&app, crawl, address).await;
        assert!(
            res.body.contains("permanent email"),
            "{address}: {}",
            res.body
        );
    }
    assert!(
        app.mail.lock().unwrap().is_empty(),
        "no mail to throwaway inboxes"
    );

    // Real providers, and look-alike names, are fine.
    for address in [
        "ana@gmail.com",
        "ana@mailinator.example.org",
        "ana@notmailinator.com",
    ] {
        let res = unlock(&app, crawl, address).await;
        assert!(
            res.body.contains("Check your email"),
            "{address}: {}",
            res.body
        );
    }
}

#[tokio::test]
async fn at_most_three_unlock_emails_per_audit_per_hour() {
    let app = TestApp::with_config(cloud_config()).await;
    let crawl = crawl_id(&submit_from(&app, "example.com", "203.0.113.9").await);
    let other = crawl_id(&submit_from(&app, "other.com", "203.0.113.9").await);
    for i in 0..3 {
        let res = unlock(&app, crawl, &format!("person{i}@example.com")).await;
        assert!(res.body.contains("Check your email"), "{i}");
    }
    let res = unlock(&app, crawl, "person3@example.com").await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(res.body.contains("already sent"), "{}", res.body);
    assert_eq!(app.mail.lock().unwrap().len(), 3);

    // Another audit has its own allowance.
    let res = unlock(&app, other, "person4@example.com").await;
    assert!(res.body.contains("Check your email"));
}
