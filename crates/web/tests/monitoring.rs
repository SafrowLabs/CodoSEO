//! The "Keep monitoring?" link: one click turns monitoring back on, once, without signing the
//! visitor in. Email clicks count as activity.

mod support;

use axum::http::StatusCode;
use codoseo_store::auth::{TokenPurpose, create_token};
use codoseo_web::auth::session::hash;
use support::{TestApp, cloud_config};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

/// A Free account that was paused after ignoring the warning, with a live resume token.
async fn paused_account(app: &TestApp, token: &str) -> Uuid {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO accounts (email, plan, paused, keep_monitoring_sent_at) \
         VALUES ('quiet@example.test', 'free', true, now() - interval '8 days') RETURNING id",
    )
    .fetch_one(app.pool())
    .await
    .unwrap();
    create_token(
        app.pool(),
        TokenPurpose::ResumeMonitoring,
        &hash(token),
        Some(id),
        None,
        Duration::days(7),
    )
    .await
    .unwrap();
    id
}

async fn state(app: &TestApp, id: Uuid) -> (bool, Option<OffsetDateTime>, Option<OffsetDateTime>) {
    sqlx::query_as(
        "SELECT paused, keep_monitoring_sent_at, last_email_click_at FROM accounts WHERE id = $1",
    )
    .bind(id)
    .fetch_one(app.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn opening_the_resume_link_only_asks_and_changes_nothing() {
    let app = TestApp::with_config(cloud_config()).await;
    let id = paused_account(&app, "resume-token").await;

    // A mail scanner may open the link any number of times.
    for _ in 0..3 {
        let res = app.get("/monitoring/resume/resume-token", None).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.body);
        assert!(res.body.contains("Keep monitoring"), "{}", res.body);
        assert!(
            res.body
                .contains(r#"action="/monitoring/resume/resume-token""#)
                && res.body.contains(r#"method="post""#),
            "a form that posts back: {}",
            res.body
        );
        assert!(!res.body.contains("Monitoring is back on"));
        let (paused, sent, click) = state(&app, id).await;
        assert!(
            paused && sent.is_some() && click.is_none(),
            "GET changed state"
        );
    }

    // The button consumes the token, once, without signing anyone in.
    let res = app.post("/monitoring/resume/resume-token", "", None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("Monitoring is back on"));
    assert!(res.body.contains(r#"href="/""#), "a link to the dashboard");
    assert!(
        res.cookie("codoseo_session").is_none(),
        "the link does not sign anyone in"
    );
    let (paused, sent, click) = state(&app, id).await;
    assert!(!paused && sent.is_none() && click.is_some());

    // The second use is a friendly dead end, not an error page and not a 500.
    sqlx::query("UPDATE accounts SET paused = true WHERE id = $1")
        .bind(id)
        .execute(app.pool())
        .await
        .unwrap();
    let again = app.post("/monitoring/resume/resume-token", "", None).await;
    assert_eq!(again.status, StatusCode::GONE);
    assert!(again.body.contains("expired or was already used"));
    assert!(again.body.contains("/login"));
    assert!(state(&app, id).await.0, "a spent token changes nothing");
    // And a GET of the spent link no longer offers the button.
    let page = app.get("/monitoring/resume/resume-token", None).await;
    assert_eq!(page.status, StatusCode::GONE);
    assert!(page.body.contains("expired or was already used"));
}

#[tokio::test]
async fn a_cross_origin_post_cannot_resume() {
    let app = TestApp::with_config(cloud_config()).await;
    let id = paused_account(&app, "resume-token").await;
    let res = app
        .post_with_headers(
            "/monitoring/resume/resume-token",
            "",
            None,
            &[("origin", "https://evil.example")],
        )
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert!(state(&app, id).await.0, "still paused");
}

#[tokio::test]
async fn a_bad_expired_or_foreign_token_gets_a_friendly_page() {
    let app = TestApp::with_config(cloud_config()).await;
    let id = paused_account(&app, "good").await;
    create_token(
        app.pool(),
        TokenPurpose::ResumeMonitoring,
        &hash("old"),
        Some(id),
        None,
        Duration::seconds(-60),
    )
    .await
    .unwrap();
    create_token(
        app.pool(),
        TokenPurpose::MagicLink,
        &hash("magic"),
        Some(id),
        None,
        Duration::days(1),
    )
    .await
    .unwrap();
    for token in ["nonsense", "old", "magic", "%20", "a".repeat(500).as_str()] {
        let path = format!("/monitoring/resume/{token}");
        for res in [app.get(&path, None).await, app.post(&path, "", None).await] {
            assert_eq!(res.status, StatusCode::GONE, "{token}: {}", res.body);
            assert!(res.body.contains("expired or was already used"));
        }
    }
    assert!(state(&app, id).await.0, "still paused");
}

#[tokio::test]
async fn a_magic_link_click_counts_as_activity_and_lifts_the_pause() {
    let app = TestApp::with_config(cloud_config()).await;
    let id = paused_account(&app, "unused").await;
    create_token(
        app.pool(),
        TokenPurpose::MagicLink,
        &hash("signin"),
        None,
        Some(serde_json::json!({ "email": "quiet@example.test", "next": "/" })),
        Duration::minutes(15),
    )
    .await
    .unwrap();
    let res = app.post("/auth/magic/signin", "", None).await;
    assert!(res.cookie("codoseo_session").is_some(), "{}", res.body);
    let (paused, sent, click) = state(&app, id).await;
    assert!(!paused, "signing in lifts the pause");
    assert!(sent.is_none());
    assert!(click.is_some(), "a magic link is an email click");
}
