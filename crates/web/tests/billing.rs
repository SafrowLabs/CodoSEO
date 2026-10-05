//! T7.6: Dodo Payments billing: the signed webhook (replays, ordering, forged and stale
//! requests), checkout and the customer portal against a fake Dodo, and the screens.

mod support;

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::{Request as AxumRequest, State};
use axum::http::{Method, Request, StatusCode, header};
use axum::response::IntoResponse;
use codoseo_core::plan::Plan;
use codoseo_store::accounts::Account;
use codoseo_testkit::TestServer;
use codoseo_web::billing::dodo;
use support::{TestApp, TestResponse, cloud_config_with, set_plan};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};
use tower::ServiceExt;
use uuid::Uuid;

const SECRET: &str = "whsec_dGVzdC13ZWJob29rLXNlY3JldA==";

// ---- a fake Dodo -----------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Call {
    method: String,
    path: String,
    query: Option<String>,
    auth: Option<String>,
    body: serde_json::Value,
}

#[derive(Clone, Default)]
struct Fake {
    calls: Arc<Mutex<Vec<Call>>>,
    broken: Arc<Mutex<bool>>,
}

async fn fake_handler(State(fake): State<Fake>, req: AxumRequest) -> axum::response::Response {
    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    fake.calls.lock().unwrap().push(Call {
        method: parts.method.to_string(),
        path: parts.uri.path().to_owned(),
        query: parts.uri.query().map(str::to_owned),
        auth: parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        body: serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    });
    if *fake.broken.lock().unwrap() {
        return (StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response();
    }
    let path = parts.uri.path();
    if path == "/checkouts" {
        axum::Json(serde_json::json!({
            "session_id": "cks_1",
            "checkout_url": "https://checkout.dodo.test/session/cks_1"
        }))
        .into_response()
    } else if path.ends_with("/customer-portal/session") {
        axum::Json(serde_json::json!({ "link": "https://portal.dodo.test/p/abc" })).into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

async fn fake_dodo() -> (TestServer, Fake) {
    let fake = Fake::default();
    let router = Router::new()
        .fallback(fake_handler)
        .with_state(fake.clone());
    (TestServer::start(router).await, fake)
}

async fn billing_app() -> (TestApp, Fake, TestServer) {
    let (server, fake) = fake_dodo().await;
    let app = TestApp::with_config(cloud_config_with(&[
        ("DODO_API_KEY", "dodo_key_123"),
        ("DODO_WEBHOOK_SECRET", SECRET),
        ("DODO_PRODUCT_PRO", "pdt_pro"),
        ("DODO_PRODUCT_AGENCY", "pdt_agency"),
        ("DODO_API_URL", &server.base()),
    ]))
    .await;
    (app, fake, server)
}

// ---- webhook helpers --------------------------------------------------------------------------

fn iso(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap()
}

/// The moment event "n minutes after the start" happened.
fn base() -> OffsetDateTime {
    OffsetDateTime::now_utc() - Duration::hours(2)
}

fn event_body(
    kind: &str,
    account: Option<Uuid>,
    product: &str,
    happened: OffsetDateTime,
) -> String {
    serde_json::json!({
        "business_id": "bus_1",
        "type": kind,
        "timestamp": iso(happened),
        "data": {
            "payload_type": "Subscription",
            "subscription_id": "sub_1",
            "product_id": product,
            "status": "active",
            "next_billing_date": iso(happened + Duration::days(30)),
            "customer": {"customer_id": "cus_1", "email": "ana@example.com", "name": "Ana"},
            "metadata": account.map(|a| serde_json::json!({"account_id": a.to_string()})).unwrap_or_default(),
        }
    })
    .to_string()
}

struct Hook<'a> {
    id: &'a str,
    body: &'a str,
    secret: &'a str,
    sent_at: OffsetDateTime,
}

impl<'a> Hook<'a> {
    fn new(id: &'a str, body: &'a str) -> Hook<'a> {
        Hook {
            id,
            body,
            secret: SECRET,
            sent_at: OffsetDateTime::now_utc(),
        }
    }

    fn request(&self) -> Request<Body> {
        let ts = self.sent_at.unix_timestamp();
        let sig = dodo::sign(self.secret, self.id, ts, self.body.as_bytes()).unwrap();
        Request::builder()
            .method(Method::POST)
            .uri("/billing/webhook")
            .header(header::CONTENT_TYPE, "application/json")
            .header("webhook-id", self.id)
            .header("webhook-timestamp", ts.to_string())
            .header("webhook-signature", sig)
            .body(Body::from(self.body.to_owned()))
            .unwrap()
    }

    /// Straight into the router: no `Origin`, like Dodo's servers.
    async fn send(&self, app: &TestApp) -> StatusCode {
        send_raw(app, self.request()).await.status()
    }
}

async fn send_raw(app: &TestApp, req: Request<Body>) -> axum::response::Response {
    app.router.clone().oneshot(req).await.unwrap()
}

async fn account_state(app: &TestApp, id: Uuid) -> (String, Option<OffsetDateTime>) {
    sqlx::query_as("SELECT plan::text, plan_expires_at FROM accounts WHERE id = $1")
        .bind(id)
        .fetch_one(app.pool())
        .await
        .unwrap()
}

async fn site_state(app: &TestApp, id: Uuid) -> (bool, Option<String>) {
    sqlx::query_as("SELECT monitoring_active, schedule FROM sites WHERE id = $1")
        .bind(id)
        .fetch_one(app.pool())
        .await
        .unwrap()
}

async fn events_stored(app: &TestApp) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM billing_events")
        .fetch_one(app.pool())
        .await
        .unwrap()
}

async fn free_account(app: &TestApp, email: &str) -> (Account, String) {
    app.login_with_plan(email, Some(Plan::Free)).await
}

// ---- the webhook ------------------------------------------------------------------------------

#[tokio::test]
async fn a_valid_active_event_sets_pro_with_daily_schedules_and_no_origin_is_needed() {
    let (app, _, _server) = billing_app().await;
    let (account, _) = free_account(&app, "ana@example.com").await;
    let site = app.site(&account, "example.com").await;
    assert_eq!(site_state(&app, site.id).await.1.as_deref(), Some("weekly"));

    let when = base();
    let body = event_body("subscription.active", Some(account.id), "pdt_pro", when);
    let hook = Hook::new("msg_1", &body);
    assert!(!hook.request().headers().contains_key(header::ORIGIN));
    assert_eq!(hook.send(&app).await, StatusCode::OK);

    let (plan, expires) = account_state(&app, account.id).await;
    assert_eq!(plan, "pro");
    let expected = (when + Duration::days(33)).unix_timestamp();
    assert_eq!(expires.unwrap().unix_timestamp(), expected);
    let (customer, sub): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT dodo_customer_id, dodo_subscription_id FROM accounts WHERE id = $1")
            .bind(account.id)
            .fetch_one(app.pool())
            .await
            .unwrap();
    assert_eq!(customer.as_deref(), Some("cus_1"));
    assert_eq!(sub.as_deref(), Some("sub_1"));
    assert_eq!(site_state(&app, site.id).await.1.as_deref(), Some("daily"));
    assert_eq!(events_stored(&app).await, 1);
}

#[tokio::test]
async fn the_agency_product_sets_agency() {
    let (app, _, _server) = billing_app().await;
    let (account, _) = free_account(&app, "ana@example.com").await;
    let body = event_body(
        "subscription.active",
        Some(account.id),
        "pdt_agency",
        base(),
    );
    assert_eq!(Hook::new("msg_1", &body).send(&app).await, StatusCode::OK);
    assert_eq!(account_state(&app, account.id).await.0, "agency");
}

#[tokio::test]
async fn an_account_is_found_by_the_customers_email_when_the_metadata_is_missing() {
    let (app, _, _server) = billing_app().await;
    let (account, _) = free_account(&app, "Ana@Example.com").await;
    let body = event_body("subscription.active", None, "pdt_pro", base());
    assert_eq!(Hook::new("msg_1", &body).send(&app).await, StatusCode::OK);
    assert_eq!(account_state(&app, account.id).await.0, "pro");
}

#[tokio::test]
async fn forged_and_stale_requests_are_401_and_change_nothing() {
    let (app, _, _server) = billing_app().await;
    let (account, _) = free_account(&app, "ana@example.com").await;
    let body = event_body("subscription.active", Some(account.id), "pdt_pro", base());

    // A wrong secret.
    let wrong = Hook {
        secret: "whsec_b3RoZXItc2VjcmV0LTAxMjM0NTY3ODk=",
        ..Hook::new("msg_1", &body)
    };
    assert_eq!(wrong.send(&app).await, StatusCode::UNAUTHORIZED);

    // A body changed after signing.
    let mut req = Hook::new("msg_2", &body).request();
    let tampered = body.replace("pdt_pro", "pdt_agency");
    *req.body_mut() = Body::from(tampered);
    assert_eq!(send_raw(&app, req).await.status(), StatusCode::UNAUTHORIZED);

    // Signed more than five minutes ago, and more than five minutes ahead.
    for skew in [Duration::minutes(-6), Duration::minutes(6)] {
        let stale = Hook {
            sent_at: OffsetDateTime::now_utc() + skew,
            ..Hook::new("msg_3", &body)
        };
        assert_eq!(stale.send(&app).await, StatusCode::UNAUTHORIZED, "{skew}");
    }

    // A missing header, one at a time.
    for name in ["webhook-id", "webhook-timestamp", "webhook-signature"] {
        let mut req = Hook::new("msg_4", &body).request();
        req.headers_mut().remove(name);
        assert_eq!(
            send_raw(&app, req).await.status(),
            StatusCode::UNAUTHORIZED,
            "{name}"
        );
    }

    // No signature at all.
    let bare = Request::builder()
        .method(Method::POST)
        .uri("/billing/webhook")
        .body(Body::from(body.clone()))
        .unwrap();
    assert_eq!(
        send_raw(&app, bare).await.status(),
        StatusCode::UNAUTHORIZED
    );

    assert_eq!(account_state(&app, account.id).await.0, "free");
    assert_eq!(events_stored(&app).await, 0, "nothing unverified is stored");
}

#[tokio::test]
async fn several_signatures_where_one_is_valid_pass() {
    let (app, _, _server) = billing_app().await;
    let (account, _) = free_account(&app, "ana@example.com").await;
    let body = event_body("subscription.active", Some(account.id), "pdt_pro", base());
    let mut req = Hook::new("msg_1", &body).request();
    let good = req.headers()["webhook-signature"]
        .to_str()
        .unwrap()
        .to_owned();
    let bad = dodo::sign(
        "whsec_b3RoZXItc2VjcmV0LTAxMjM0NTY3ODk=",
        "msg_1",
        OffsetDateTime::now_utc().unix_timestamp(),
        body.as_bytes(),
    )
    .unwrap();
    req.headers_mut().insert(
        "webhook-signature",
        format!("{bad} {good}").parse().unwrap(),
    );
    assert_eq!(send_raw(&app, req).await.status(), StatusCode::OK);
    assert_eq!(account_state(&app, account.id).await.0, "pro");
}

#[tokio::test]
async fn a_replayed_event_id_changes_nothing_and_still_answers_200() {
    let (app, _, _server) = billing_app().await;
    let (account, _) = free_account(&app, "ana@example.com").await;
    let body = event_body("subscription.active", Some(account.id), "pdt_pro", base());
    assert_eq!(Hook::new("msg_1", &body).send(&app).await, StatusCode::OK);
    set_plan(app.pool(), account.id, Plan::Agency).await;

    assert_eq!(Hook::new("msg_1", &body).send(&app).await, StatusCode::OK);
    assert_eq!(account_state(&app, account.id).await.0, "agency");
    assert_eq!(events_stored(&app).await, 1);
}

#[tokio::test]
async fn an_older_active_after_an_applied_expired_is_ignored() {
    let (app, _, _server) = billing_app().await;
    let (account, _) = free_account(&app, "ana@example.com").await;
    let t0 = base();
    let active = event_body("subscription.active", Some(account.id), "pdt_pro", t0);
    assert_eq!(Hook::new("msg_1", &active).send(&app).await, StatusCode::OK);
    let expired = event_body(
        "subscription.expired",
        Some(account.id),
        "pdt_pro",
        t0 + Duration::minutes(40),
    );
    assert_eq!(
        Hook::new("msg_3", &expired).send(&app).await,
        StatusCode::OK
    );
    assert_eq!(
        account_state(&app, account.id).await,
        ("free".to_owned(), None)
    );

    // The renewal that happened between them arrives last.
    let late = event_body(
        "subscription.active",
        Some(account.id),
        "pdt_pro",
        t0 + Duration::minutes(20),
    );
    assert_eq!(Hook::new("msg_2", &late).send(&app).await, StatusCode::OK);
    assert_eq!(
        account_state(&app, account.id).await,
        ("free".to_owned(), None)
    );
}

#[tokio::test]
async fn cancelled_keeps_the_plan_and_expired_downgrades_and_the_owner_picks_the_site() {
    let (app, _, _server) = billing_app().await;
    let (account, cookie) = free_account(&app, "ana@example.com").await;
    let oldest = app.site(&account, "a.example.com").await;
    let middle = app.site(&account, "b.example.com").await;
    let newest = app.site(&account, "c.example.com").await;
    sqlx::query("UPDATE sites SET created_at = now() - make_interval(days => $2) WHERE id = $1")
        .bind(oldest.id)
        .bind(30)
        .execute(app.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE sites SET created_at = now() - make_interval(days => $2) WHERE id = $1")
        .bind(middle.id)
        .bind(20)
        .execute(app.pool())
        .await
        .unwrap();

    let t0 = base();
    let active = event_body("subscription.active", Some(account.id), "pdt_pro", t0);
    Hook::new("msg_1", &active).send(&app).await;
    let before = account_state(&app, account.id).await;
    assert_eq!(before.0, "pro");

    let cancelled = event_body(
        "subscription.cancelled",
        Some(account.id),
        "pdt_pro",
        t0 + Duration::minutes(5),
    );
    assert_eq!(
        Hook::new("msg_2", &cancelled).send(&app).await,
        StatusCode::OK
    );
    assert_eq!(account_state(&app, account.id).await, before);

    let expired = event_body(
        "subscription.expired",
        Some(account.id),
        "pdt_pro",
        t0 + Duration::minutes(10),
    );
    assert_eq!(
        Hook::new("msg_3", &expired).send(&app).await,
        StatusCode::OK
    );
    assert_eq!(
        account_state(&app, account.id).await,
        ("free".to_owned(), None)
    );
    assert!(site_state(&app, oldest.id).await.0);
    assert!(!site_state(&app, middle.id).await.0);
    assert!(!site_state(&app, newest.id).await.0);

    // The sites screen explains it and lets the owner choose another.
    let page = app.get("/billing/sites", Some(&cookie)).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("a.example.com") && page.body.contains("c.example.com"));
    assert!(page.body.contains("1 site"), "{}", page.body);

    let res = app
        .post(
            "/billing/sites",
            &format!("keep={}", newest.id),
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert!(!site_state(&app, oldest.id).await.0);
    assert!(site_state(&app, newest.id).await.0);

    // Two is more than Free allows.
    let res = app
        .post(
            "/billing/sites",
            &format!("keep={}&keep={}", oldest.id, newest.id),
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert!(site_state(&app, newest.id).await.0);
    assert!(!site_state(&app, oldest.id).await.0);
}

#[tokio::test]
async fn sites_belonging_to_someone_else_cannot_be_kept() {
    let (app, _, _server) = billing_app().await;
    let (_, cookie) = free_account(&app, "ana@example.com").await;
    let (other, _) = free_account(&app, "bo@example.com").await;
    let theirs = app.site(&other, "theirs.example.com").await;
    let res = app
        .post(
            "/billing/sites",
            &format!("keep={}", theirs.id),
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    let res = app
        .post("/billing/sites", "keep=not-a-uuid", Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn failed_and_hold_events_and_unknown_ones_are_answered_200() {
    let (app, _, _server) = billing_app().await;
    let (account, _) = free_account(&app, "ana@example.com").await;
    let t0 = base();
    let active = event_body("subscription.active", Some(account.id), "pdt_pro", t0);
    Hook::new("msg_1", &active).send(&app).await;
    let hold = event_body(
        "subscription.on_hold",
        Some(account.id),
        "pdt_pro",
        t0 + Duration::minutes(1),
    );
    assert_eq!(Hook::new("msg_2", &hold).send(&app).await, StatusCode::OK);
    assert_eq!(account_state(&app, account.id).await.0, "pro");
    let odd = r#"{"type":"payment.succeeded","timestamp":"2026-01-01T00:00:00Z","data":{"payment_id":"pay_1"}}"#;
    assert_eq!(Hook::new("msg_3", odd).send(&app).await, StatusCode::OK);
    let failed = event_body(
        "subscription.failed",
        Some(account.id),
        "pdt_pro",
        t0 + Duration::minutes(2),
    );
    assert_eq!(Hook::new("msg_4", &failed).send(&app).await, StatusCode::OK);
    assert_eq!(account_state(&app, account.id).await.0, "free");
    assert_eq!(events_stored(&app).await, 4);
}

#[tokio::test]
async fn an_event_for_nobody_is_stored_and_answered_200() {
    let (app, _, _server) = billing_app().await;
    let body = event_body(
        "subscription.active",
        Some(Uuid::new_v4()),
        "pdt_pro",
        base(),
    );
    assert_eq!(Hook::new("msg_1", &body).send(&app).await, StatusCode::OK);
    assert_eq!(events_stored(&app).await, 1);
}

#[tokio::test]
async fn a_signed_body_that_is_not_json_is_a_400() {
    let (app, _, _server) = billing_app().await;
    assert_eq!(
        Hook::new("msg_1", "this is not json").send(&app).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(events_stored(&app).await, 0);
}

// ---- the Origin check -------------------------------------------------------------------------

#[tokio::test]
async fn only_the_webhook_skips_the_origin_check() {
    let (app, _, _server) = billing_app().await;
    let (_, cookie) = free_account(&app, "ana@example.com").await;
    let no_origin = |path: &str| {
        Request::builder()
            .method(Method::POST)
            .uri(path)
            .header(header::COOKIE, &cookie)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from("plan=pro"))
            .unwrap()
    };
    for path in [
        "/billing/checkout",
        "/billing/portal",
        "/billing/sites",
        "/account/signups",
        "/logout",
    ] {
        let res = send_raw(&app, no_origin(path)).await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{path}");
    }
    // A path that merely starts like the webhook's is not exempt either.
    let res = send_raw(&app, no_origin("/billing/webhook/extra")).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    // The webhook itself gets as far as its signature check.
    let res = send_raw(&app, no_origin("/billing/webhook")).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

// ---- checkout and portal ------------------------------------------------------------------------

async fn post_form(app: &TestApp, path: &str, form: &str, cookie: &str) -> TestResponse {
    app.post(path, form, Some(cookie)).await
}

#[tokio::test]
async fn checkout_posts_the_right_body_and_redirects_to_the_checkout_url() {
    let (app, fake, _server) = billing_app().await;
    let (account, cookie) = free_account(&app, "ana@example.com").await;

    let res = post_form(&app, "/billing/checkout", "plan=agency", &cookie).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(
        res.location(),
        Some("https://checkout.dodo.test/session/cks_1")
    );

    let calls = fake.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(
        (call.method.as_str(), call.path.as_str()),
        ("POST", "/checkouts")
    );
    assert_eq!(call.auth.as_deref(), Some("Bearer dodo_key_123"));
    assert_eq!(
        call.body,
        serde_json::json!({
            "product_cart": [{"product_id": "pdt_agency", "quantity": 1}],
            "customer": {"email": "ana@example.com"},
            "return_url": "https://codoseo.com/billing?status=success",
            "metadata": {"account_id": account.id.to_string()},
        })
    );

    let res = post_form(&app, "/billing/checkout", "plan=pro", &cookie).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(
        fake.calls.lock().unwrap()[1].body["product_cart"][0]["product_id"],
        "pdt_pro"
    );
}

#[tokio::test]
async fn checkout_refuses_unknown_plans_and_accounts_that_already_pay() {
    let (app, fake, _server) = billing_app().await;
    let (_, cookie) = free_account(&app, "ana@example.com").await;
    for plan in ["free", "self_hosted", "gold", ""] {
        let res = post_form(&app, "/billing/checkout", &format!("plan={plan}"), &cookie).await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{plan}");
    }
    let (_, paid) = app
        .login_with_plan("pro@example.com", Some(Plan::Pro))
        .await;
    let res = post_form(&app, "/billing/checkout", "plan=agency", &paid).await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_dodo_error_shows_a_friendly_message_on_the_billing_page() {
    let (app, fake, _server) = billing_app().await;
    let (_, cookie) = free_account(&app, "ana@example.com").await;
    *fake.broken.lock().unwrap() = true;
    let res = post_form(&app, "/billing/checkout", "plan=pro", &cookie).await;
    assert_eq!(res.status, StatusCode::BAD_GATEWAY);
    assert!(res.body.contains("payment provider"), "{}", res.body);
    assert!(
        !res.body.contains("boom"),
        "the provider's text isn't shown"
    );
    assert!(res.body.contains("Upgrade"), "the page is still usable");
}

#[tokio::test]
async fn the_portal_redirects_to_the_link_and_needs_a_customer() {
    let (app, fake, _server) = billing_app().await;
    let (account, cookie) = app
        .login_with_plan("ana@example.com", Some(Plan::Pro))
        .await;

    // No customer yet: nothing to open.
    let res = post_form(&app, "/billing/portal", "", &cookie).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert!(fake.calls.lock().unwrap().is_empty());

    sqlx::query("UPDATE accounts SET dodo_customer_id = 'cus_9' WHERE id = $1")
        .bind(account.id)
        .execute(app.pool())
        .await
        .unwrap();
    let res = post_form(&app, "/billing/portal", "", &cookie).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.location(), Some("https://portal.dodo.test/p/abc"));
    let calls = fake.calls.lock().unwrap().clone();
    assert_eq!(calls[0].path, "/customers/cus_9/customer-portal/session");
    assert_eq!(calls[0].auth.as_deref(), Some("Bearer dodo_key_123"));
    assert_eq!(
        calls[0].query.as_deref(),
        Some("return_url=https%3A%2F%2Fcodoseo.com%2Fbilling")
    );
}

#[tokio::test]
async fn billing_needs_a_sign_in_and_checkout_works_for_nobody_else() {
    let (app, fake, _server) = billing_app().await;
    for path in ["/billing", "/billing/sites"] {
        let res = app.get(path, None).await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{path}");
        assert!(res.location().unwrap().starts_with("/login"));
    }
    for path in ["/billing/checkout", "/billing/portal", "/billing/sites"] {
        let res = app.post(path, "plan=pro", None).await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{path}");
        assert!(res.location().unwrap().starts_with("/login"));
    }
    assert!(fake.calls.lock().unwrap().is_empty());
}

// ---- the screens ------------------------------------------------------------------------------

#[tokio::test]
async fn the_page_shows_the_plan_the_cards_and_the_nav_entry() {
    let (app, _, _server) = billing_app().await;
    let (_, cookie) = free_account(&app, "ana@example.com").await;
    let res = app.get("/billing", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("Free"));
    assert!(res.body.contains("Pro") && res.body.contains("Agency"));
    assert!(res.body.contains("action=\"/billing/checkout\""));
    assert!(res.body.contains("value=\"pro\"") && res.body.contains("value=\"agency\""));
    assert!(!res.body.contains("Manage billing"), "no customer yet");
    assert!(!res.body.contains("isn't configured"));
    assert!(
        res.body.contains("href=\"/billing\""),
        "the sidebar links here"
    );
}

#[tokio::test]
async fn a_paid_account_sees_its_renewal_date_and_manage_billing() {
    let (app, _, _server) = billing_app().await;
    let (account, cookie) = free_account(&app, "ana@example.com").await;
    let body = event_body(
        "subscription.active",
        Some(account.id),
        "pdt_pro",
        OffsetDateTime::now_utc(),
    );
    Hook::new("msg_1", &body).send(&app).await;
    let res = app.get("/billing", Some(&cookie)).await;
    assert!(res.body.contains("Manage billing"));
    assert!(res.body.contains("action=\"/billing/portal\""));
    assert!(res.body.contains("Current plan"));
    assert!(res.body.contains("Renews on"), "{}", res.body);
    assert!(!res.body.contains("Your plan ends on"));
    assert!(
        !res.body.contains("action=\"/billing/checkout\""),
        "a paying account changes plan in the portal"
    );
}

#[tokio::test]
async fn the_return_url_shows_a_thank_you() {
    let (app, _, _server) = billing_app().await;
    let (_, cookie) = free_account(&app, "ana@example.com").await;
    let res = app.get("/billing?status=success", Some(&cookie)).await;
    assert!(res.body.contains("your plan updates in a few seconds"));
    let res = app.get("/billing", Some(&cookie)).await;
    assert!(!res.body.contains("your plan updates in a few seconds"));
}

#[tokio::test]
async fn without_dodo_keys_the_page_says_so_and_checkout_is_off() {
    let app = TestApp::with_config(support::cloud_config()).await;
    let (_, cookie) = free_account(&app, "ana@example.com").await;
    let res = app.get("/billing", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("Billing isn't configured"));
    assert!(!res.body.contains("action=\"/billing/checkout\""));
    let res = post_form(&app, "/billing/checkout", "plan=pro", &cookie).await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    let res = post_form(&app, "/billing/portal", "", &cookie).await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    // And the webhook has nothing to verify with.
    let body = event_body("subscription.active", None, "pdt_pro", base());
    assert_eq!(
        Hook::new("msg_1", &body).send(&app).await,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn self_hosted_has_no_billing_at_all() {
    let app = TestApp::new().await;
    let (_, cookie) = app.login("ana@example.com").await;
    for path in ["/billing", "/billing/sites"] {
        let res = app.get(path, Some(&cookie)).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{path}");
    }
    for path in [
        "/billing/checkout",
        "/billing/portal",
        "/billing/sites",
        "/billing/webhook",
    ] {
        let res = app.post(path, "plan=pro", Some(&cookie)).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{path}");
    }
    // Signed out, it is a 404 too (not a login redirect).
    assert_eq!(
        app.get("/billing", None).await.status,
        StatusCode::NOT_FOUND
    );
    // No nav entry on any screen.
    let res = app.get("/account", Some(&cookie)).await;
    assert!(!res.body.contains("/billing"), "{}", res.body);
    let res = app.get("/sites", Some(&cookie)).await;
    assert!(!res.body.contains("/billing"));
}

#[tokio::test]
async fn the_sites_list_links_to_the_picker_when_a_site_was_stopped_by_the_plan() {
    let (app, _, _server) = billing_app().await;
    let (account, cookie) = free_account(&app, "ana@example.com").await;
    let one = app.site(&account, "one.example.com").await;
    let res = app.get("/sites", Some(&cookie)).await;
    assert!(!res.body.contains("/billing/sites"));

    sqlx::query("UPDATE sites SET monitoring_active = false WHERE id = $1")
        .bind(one.id)
        .execute(app.pool())
        .await
        .unwrap();
    let res = app.get("/sites", Some(&cookie)).await;
    assert!(res.body.contains("href=\"/billing/sites\""), "{}", res.body);
}

#[tokio::test]
async fn a_cancelled_subscription_says_when_the_plan_ends_until_it_is_renewed() {
    let (app, _, _server) = billing_app().await;
    let (account, cookie) = free_account(&app, "ana@example.com").await;
    let t0 = OffsetDateTime::now_utc() - Duration::hours(1);
    let active = event_body("subscription.active", Some(account.id), "pdt_pro", t0);
    Hook::new("msg_1", &active).send(&app).await;
    let date = codoseo_web::fmt::date(t0 + Duration::days(30));

    let cancelled = event_body(
        "subscription.cancelled",
        Some(account.id),
        "pdt_pro",
        t0 + Duration::minutes(5),
    );
    assert_eq!(
        Hook::new("msg_2", &cancelled).send(&app).await,
        StatusCode::OK
    );
    let res = app.get("/billing", Some(&cookie)).await;
    assert!(
        res.body.contains(&format!("Your plan ends on {date}")),
        "{}",
        res.body
    );
    assert!(!res.body.contains("Renews on"));

    let renewed = event_body(
        "subscription.renewed",
        Some(account.id),
        "pdt_pro",
        t0 + Duration::minutes(10),
    );
    Hook::new("msg_3", &renewed).send(&app).await;
    let res = app.get("/billing", Some(&cookie)).await;
    assert!(res.body.contains("Renews on"), "{}", res.body);
    assert!(!res.body.contains("Your plan ends on"));
}

#[tokio::test]
async fn a_verified_event_with_wrongly_typed_fields_is_stored_and_answered_200() {
    let (app, _, _server) = billing_app().await;
    let (account, _) = free_account(&app, "ana@example.com").await;
    let body = serde_json::json!({
        "type": "subscription.active",
        "timestamp": 12345,
        "data": {
            "subscription_id": "sub_1",
            "product_id": "pdt_pro",
            "status": 7,
            "customer": "cus_1",
            "metadata": {"account_id": account.id.to_string()},
        }
    })
    .to_string();
    assert_eq!(Hook::new("msg_1", &body).send(&app).await, StatusCode::OK);
    assert_eq!(events_stored(&app).await, 1);
    // `status: 7` is unusable, so it counts as absent and an activating event takes it as active.
    assert_eq!(account_state(&app, account.id).await.0, "pro");

    // Only a body that isn't a JSON object is refused.
    for (n, bad) in ["[1,2]", "\"x\"", "42"].iter().enumerate() {
        let id = format!("msg_bad{n}");
        assert_eq!(
            Hook::new(&id, bad).send(&app).await,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
    assert_eq!(events_stored(&app).await, 1);
}

#[tokio::test]
async fn extreme_webhook_timestamps_are_401_not_a_panic() {
    let (app, _, _server) = billing_app().await;
    let body = event_body("subscription.active", None, "pdt_pro", base());
    for ts in [i64::MIN, i64::MAX] {
        let mut req = Hook::new("msg_1", &body).request();
        req.headers_mut()
            .insert("webhook-timestamp", ts.to_string().parse().unwrap());
        assert_eq!(
            send_raw(&app, req).await.status(),
            StatusCode::UNAUTHORIZED,
            "{ts}"
        );
    }
}
