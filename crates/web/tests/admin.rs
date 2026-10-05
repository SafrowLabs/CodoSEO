//! T6.3: the owner-only `/admin` page: funnel counts, the quick-audit queue and failed jobs.

mod support;

use axum::http::StatusCode;
use support::{TestApp, cloud_config_with};
use uuid::Uuid;

fn admin_config() -> codoseo_web::Config {
    // The second address is a Gmail variant: admins are matched on the canonical email.
    cloud_config_with(&[("ADMIN_EMAILS", "boss@example.com, Other+x@Gmail.com")])
}

async fn event(app: &TestApp, kind: &str, payload: serde_json::Value) {
    sqlx::query("INSERT INTO events (kind, payload) VALUES ($1, $2)")
        .bind(kind)
        .bind(payload)
        .execute(app.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn admins_see_the_funnel_the_queue_and_failed_jobs() {
    let app = TestApp::with_config(admin_config()).await;
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    // Three started (two audits plus one repeat view), two finished, one email.
    for crawl in [a, a, b] {
        event(
            &app,
            "audit_started",
            serde_json::json!({ "crawl_id": crawl }),
        )
        .await;
    }
    for crawl in [a, b] {
        event(
            &app,
            "audit_finished",
            serde_json::json!({ "crawl_id": crawl }),
        )
        .await;
    }
    event(&app, "email_given", serde_json::json!({ "crawl_id": a })).await;
    // Two audits waiting, one failed job.
    for d in ["wait1.com", "wait2.com"] {
        codoseo_store::quick::start(
            app.pool(),
            &codoseo_store::quick::StartRequest {
                domain: d,
                start_url: &format!("https://{d}/"),
                claim_hash: d.as_bytes(),
                ip_hash: None,
                limits: codoseo_store::quick::Limits::NONE,
                source: codoseo_store::quick::Source::Web,
                agent_daily_budget: None,
                previous_ip_hash: None,
            },
        )
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO jobs (kind, payload, status, attempt, last_error) \
         VALUES ('send_email', '{}', 'failed', 5, 'smtp refused the recipient')",
    )
    .execute(app.pool())
    .await
    .unwrap();

    let (_, cookie) = app.login("boss@example.com").await;
    let res = app.get("/admin", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);

    // Every step is listed, in order, with unique counts and conversion from the step before.
    let order: Vec<usize> = [
        "audit_started",
        "audit_finished",
        "email_given",
        "link_clicked",
        "first_full_crawl",
        "active_after_4_weeks",
        "rankorg_click",
    ]
    .iter()
    .map(|k| {
        res.body
            .find(&format!(r#"data-kind="{k}""#))
            .unwrap_or_else(|| panic!("{k}"))
    })
    .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "in funnel order");
    assert!(
        res.body
            .contains(r#"data-kind="audit_started" data-unique="2""#),
        "{}",
        res.body
    );
    assert!(
        res.body
            .contains(r#"data-kind="audit_finished" data-unique="2""#)
    );
    assert!(
        res.body
            .contains(r#"data-kind="email_given" data-unique="1""#)
    );
    assert!(res.body.contains("100%"), "2 of 2 audits finished");
    assert!(
        res.body.contains("50%"),
        "1 of 2 finished audits gave an email"
    );
    assert!(res.body.contains(r#"data-queue-depth="2""#));
    assert!(res.body.contains("smtp refused the recipient"));
    assert!(res.body.contains("send_email"));
    assert!(res.body.contains("Last 7 days") && res.body.contains("Last 30 days"));
}

#[tokio::test]
async fn only_listed_admins_get_in_and_everyone_else_sees_a_404() {
    let app = TestApp::with_config(admin_config()).await;

    // Signed out: to login, like every signed-in screen.
    let res = app.get("/admin", None).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert!(res.location().unwrap().starts_with("/login"));

    // Signed in but not listed: the page doesn't exist.
    let (_, cookie) = app.login("ana@example.com").await;
    assert_eq!(
        app.get("/admin", Some(&cookie)).await.status,
        StatusCode::NOT_FOUND
    );

    // Listed by an email variant.
    let (_, cookie) = app.login("o.ther+seo@gmail.com").await;
    assert_eq!(
        app.get("/admin", Some(&cookie)).await.status,
        StatusCode::OK
    );

    // The account page links to it for admins only.
    let admin_account = app.get("/account", Some(&cookie)).await.body;
    assert!(admin_account.contains(r#"href="/admin""#));
    let (_, ana) = app.login("ana@example.com").await;
    assert!(
        !app.get("/account", Some(&ana))
            .await
            .body
            .contains(r#"href="/admin""#)
    );
}

#[tokio::test]
async fn on_a_self_hosted_instance_the_owner_is_the_admin() {
    let app = TestApp::new().await;
    let (owner, owner_cookie) = app.login("owner@example.com").await;
    assert!(owner.is_owner, "the first signup owns the instance");
    assert_eq!(
        app.get("/admin", Some(&owner_cookie)).await.status,
        StatusCode::OK
    );

    let (_, member_cookie) = app.login("member@example.com").await;
    assert_eq!(
        app.get("/admin", Some(&member_cookie)).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_cloud_owner_flag_alone_does_not_make_an_admin() {
    // In the cloud nobody is flagged as owner, and an empty ADMIN_EMAILS means no admins.
    let app = TestApp::with_config(support::cloud_config()).await;
    let (_, cookie) = app.login("boss@example.com").await;
    assert_eq!(
        app.get("/admin", Some(&cookie)).await.status,
        StatusCode::NOT_FOUND
    );
}
