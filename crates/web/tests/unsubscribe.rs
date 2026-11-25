//! Unsubscribe links: the signed token, the confirm page, the one-click POST from a mail
//! client, and what a suppression stops (notifications, or every email).

mod support;

use axum::http::StatusCode;
use codoseo_store::suppressions::{self, Scope};
use codoseo_web::unsubscribe::{Signer, Unsubscribe};
use support::{TestApp, cloud_config};

const IP_HEADER: &str = "CF-Connecting-IP";

fn signer() -> Signer {
    // `cloud_config` sets SECRET_KEY to this.
    Signer::derive("test-secret")
}

fn link(address: &str, scope: Scope) -> String {
    format!(
        "/email/unsubscribe/{}",
        signer().token(&Unsubscribe {
            address: address.to_owned(),
            scope,
            account_id: None,
        })
    )
}

async fn suppression(app: &TestApp, canonical: &str) -> Option<(String, String)> {
    suppressions::get(app.pool(), canonical)
        .await
        .unwrap()
        .map(|s| (s.scope.as_str().to_owned(), s.reason))
}

#[tokio::test]
async fn the_get_shows_a_button_and_only_the_post_records_the_unsubscribe() {
    let app = TestApp::with_config(cloud_config()).await;
    let path = link("Ana+alerts@Example.com", Scope::Notifications);

    let page = app.get(&path, None).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body
            .contains("Stop CodoSEO emails to Ana+alerts@Example.com?"),
        "{}",
        page.body
    );
    assert!(
        page.body.contains("Sign-in links still work"),
        "{}",
        page.body
    );
    assert_eq!(suppression(&app, "ana@example.com").await, None);

    let done = app.post(&path, "confirm=1", None).await;
    assert_eq!(done.status, StatusCode::OK);
    assert!(done.body.contains("Unsubscribed"), "{}", done.body);
    assert!(done.body.contains("alert settings"), "{}", done.body);
    assert_eq!(
        suppression(&app, "ana@example.com").await,
        Some(("notifications".into(), "unsubscribe".into())),
        "stored under the canonical address"
    );
}

#[tokio::test]
async fn a_mail_clients_one_click_post_needs_no_page_and_no_origin() {
    let app = TestApp::with_config(cloud_config()).await;
    let path = link("owner@example.com", Scope::Notifications);
    // Gmail posts from its own servers: a foreign Origin (or none), the RFC 8058 body.
    let res = app
        .post_with_headers(
            &path,
            "List-Unsubscribe=One-Click",
            None,
            &[("origin", "https://mail.google.com")],
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body, "Unsubscribed");
    assert_eq!(
        suppression(&app, "owner@example.com").await,
        Some(("notifications".into(), "unsubscribe".into()))
    );
    // Other POSTs still need our Origin: the exemption is the unsubscribe path only.
    let res = app
        .post_with_headers(
            "/login",
            "email=owner%40example.com",
            None,
            &[("origin", "https://mail.google.com")],
        )
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_token_that_is_not_ours_is_a_friendly_404_and_changes_nothing() {
    let app = TestApp::with_config(cloud_config()).await;
    let good = link("owner@example.com", Scope::Notifications);
    for path in [
        "/email/unsubscribe/nonsense".to_owned(),
        format!("{good}x"),
        format!(
            "/email/unsubscribe/{}",
            Signer::derive("other").token(&Unsubscribe {
                address: "owner@example.com".into(),
                scope: Scope::All,
                account_id: None,
            })
        ),
    ] {
        let page = app.get(&path, None).await;
        assert_eq!(page.status, StatusCode::NOT_FOUND, "{path}");
        assert!(page.body.contains("isn't one of ours"), "{}", page.body);
        let post = app.post(&path, "confirm=1", None).await;
        assert_eq!(post.status, StatusCode::NOT_FOUND, "{path}");
    }
    assert_eq!(suppression(&app, "owner@example.com").await, None);
}

#[tokio::test]
async fn report_emails_carry_an_opt_out_link_that_stops_every_email() {
    let app = TestApp::with_config(cloud_config()).await;
    // A sign-in link someone asked for themselves carries no stop link...
    let res = app
        .post_hx("/login", "email=someone%40example.com", None)
        .await;
    assert!(res.body.contains("Check your email"), "{}", res.body);
    let signin = app.mail.lock().unwrap()[0].clone();
    assert!(!signin.text.contains("Stop all email"), "{}", signin.text);
    assert!(
        signin
            .html
            .as_deref()
            .is_some_and(|h| h.contains("email-logo.png"))
    );
    // ...but a report link, which anyone may have asked for, does.
    let audit = app
        .post_with_headers(
            "/audit",
            "url=example.com",
            None,
            &[(IP_HEADER, "203.0.113.9")],
        )
        .await;
    let crawl = audit
        .location()
        .unwrap()
        .strip_prefix("/audit/")
        .unwrap()
        .to_owned();
    app.post_hx(
        &format!("/audit/{crawl}/unlock"),
        "email=someone%40example.com",
        None,
    )
    .await;
    let text = app.mail.lock().unwrap()[1].text.clone();
    let opt_out = text
        .lines()
        .find_map(|l| l.strip_prefix("Stop all email from CodoSEO: "))
        .expect("the opt-out line")
        .to_owned();
    assert!(
        opt_out.starts_with("https://codoseo.com/email/unsubscribe/"),
        "{opt_out}"
    );
    let path = &opt_out["https://codoseo.com".len()..];

    let page = app.get(path, None).await.body;
    assert!(page.contains("no sign-in links"), "{page}");
    app.post(path, "confirm=1", None).await;
    assert_eq!(
        suppression(&app, "someone@example.com").await,
        Some(("all".into(), "opt_out".into()))
    );

    // From now on the sign-in form and an audit's unlock form send nothing and say why.
    let res = app
        .post_hx("/login", "email=Someone%40example.com", None)
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(
        res.body.contains("asked us not to email it"),
        "{}",
        res.body
    );
    let res = app
        .post_hx(
            &format!("/audit/{crawl}/unlock"),
            "email=someone%2Bx%40example.com",
            None,
        )
        .await;
    assert!(
        res.body.contains("asked us not to email it"),
        "{}",
        res.body
    );
    assert_eq!(
        app.mail.lock().unwrap().len(),
        2,
        "only the first two emails went"
    );
}

#[tokio::test]
async fn an_unsubscribe_widens_to_an_opt_out_but_never_narrows() {
    let app = TestApp::with_config(cloud_config()).await;
    app.post(
        &link("a@example.com", Scope::Notifications),
        "confirm=1",
        None,
    )
    .await;
    app.post(&link("a@example.com", Scope::All), "confirm=1", None)
        .await;
    assert_eq!(
        suppression(&app, "a@example.com").await,
        Some(("all".into(), "opt_out".into()))
    );
    app.post(
        &link("a@example.com", Scope::Notifications),
        "confirm=1",
        None,
    )
    .await;
    assert_eq!(
        suppression(&app, "a@example.com").await.map(|s| s.0),
        Some("all".into()),
        "a later unsubscribe link does not lift the opt-out"
    );
}

#[tokio::test]
async fn the_alert_settings_page_says_email_is_off_and_turns_it_back_on() {
    let app = TestApp::with_config(cloud_config()).await;
    let (_account, cookie) = app.login("owner@example.com").await;
    let page = app.get("/settings/alerts", Some(&cookie)).await.body;
    assert!(!page.contains("Turn email back on"), "{page}");

    app.post(
        &link("Owner@example.com", Scope::Notifications),
        "confirm=1",
        None,
    )
    .await;
    let page = app.get("/settings/alerts", Some(&cookie)).await.body;
    assert!(page.contains("Email to owner@example.com is off"), "{page}");
    assert!(page.contains("Turn email back on"), "{page}");

    let res = app
        .post("/settings/alerts/email/resume", "", Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(suppression(&app, "owner@example.com").await, None);
    let page = app.get("/settings/alerts", Some(&cookie)).await.body;
    assert!(!page.contains("Turn email back on"), "{page}");
    // Signed out, the route only sends you to sign in.
    app.post(
        &link("Owner@example.com", Scope::Notifications),
        "confirm=1",
        None,
    )
    .await;
    let res = app.post("/settings/alerts/email/resume", "", None).await;
    assert!(
        res.location().is_some_and(|l| l.starts_with("/login")),
        "{} {:?}",
        res.status,
        res.location()
    );
    assert!(suppression(&app, "owner@example.com").await.is_some());
}
