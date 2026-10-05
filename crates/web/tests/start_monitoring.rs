//! M8 T4: `start_monitoring` end to end. The no-key MCP tool emails a link; opening it (GET)
//! shows a confirm page and changes nothing; pressing the button (POST) creates or reuses the
//! account, adds the weekly site with its first crawl and default alert rules, mints one API
//! key shown once, and starts a session. Plus the email caps and the guards.

mod support;

use std::time::Duration;

use axum::http::StatusCode;
use codoseo_web::agent::keys;
use serde_json::{Value, json};
use support::mcp::{Client, DIRECT_UA, SHARED_UA};
use support::{TestApp, TestResponse, cloud_config, cloud_config_with};

const DOMAIN: &str = "example.com";

async fn cloud() -> TestApp {
    TestApp::with_config(cloud_config()).await
}

fn client(app: &TestApp) -> Client<'_> {
    Client::new(app, Duration::ZERO, Some(SHARED_UA))
}

async fn count(app: &TestApp, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(app.pool()).await.unwrap()
}

/// Asks for monitoring and returns the tool's JSON.
async fn request(app: &TestApp, email: &str) -> Value {
    client(app)
        .call_ok(
            "start_monitoring",
            json!({"url": "https://example.com/blog", "email": email}),
        )
        .await
}

/// The path of the confirmation link in the last email sent to `to`.
fn link_for(app: &TestApp, to: &str) -> String {
    let mail = app.mail.lock().unwrap();
    let email = mail.iter().rev().find(|m| m.to == to).expect("an email");
    let link = email
        .text
        .split_whitespace()
        .find(|w| w.starts_with("https://codoseo.com/monitoring/start/"))
        .unwrap_or_else(|| panic!("no link in {}", email.text));
    link.strip_prefix("https://codoseo.com").unwrap().to_owned()
}

fn mails(app: &TestApp) -> usize {
    app.mail.lock().unwrap().len()
}

/// The API key shown on the result page.
fn key_in(res: &TestResponse) -> String {
    let start = res.body.find("cdo_").expect("a key on the page");
    res.body[start..start + 47].to_owned()
}

#[tokio::test]
async fn the_tool_emails_a_confirmation_link_and_stores_only_its_hash() {
    let app = cloud().await;
    let answer = request(&app, "Owner@Example.org").await;
    assert_eq!(answer["status"], "confirmation_sent");
    assert_eq!(answer["domain"], DOMAIN);
    assert!(
        answer["message"]
            .as_str()
            .unwrap()
            .contains("Owner@Example.org")
    );

    let (to, subject, text) = {
        let mail = app.mail.lock().unwrap();
        assert_eq!(mail.len(), 1);
        (
            mail[0].to.clone(),
            mail[0].subject.clone(),
            mail[0].text.clone(),
        )
    };
    assert_eq!(to, "Owner@Example.org");
    assert_eq!(subject, "Confirm monitoring for example.com");
    assert!(text.contains("24 hours"), "{text}");
    let path = link_for(&app, "Owner@Example.org");
    let token = path.rsplit('/').next().unwrap();
    assert_eq!(token.len(), 43);

    // The token is stored hashed, for 24 hours, with the site and address in its payload.
    let (purpose, hash_len, payload, hours, account): (
        String,
        i32,
        Value,
        f64,
        Option<uuid::Uuid>,
    ) = sqlx::query_as(
        "SELECT purpose::text, length(token_hash), payload, \
                    (EXTRACT(EPOCH FROM expires_at - created_at) / 3600)::float8, account_id \
             FROM login_tokens",
    )
    .fetch_one(app.pool())
    .await
    .unwrap();
    assert_eq!(
        (purpose.as_str(), hash_len, account),
        ("start_monitoring", 32, None)
    );
    assert!((hours - 24.0).abs() < 0.01, "{hours}");
    assert_eq!(payload["email"], "Owner@Example.org");
    assert_eq!(payload["canonical"], "owner@example.org");
    assert_eq!(payload["domain"], DOMAIN);
    assert_eq!(payload["start_url"], "https://example.com/blog");
    assert_eq!(
        count(
            &app,
            &format!("SELECT count(*) FROM login_tokens WHERE payload::text LIKE '%{token}%'")
        )
        .await,
        0
    );

    // The funnel step is recorded as an agent's.
    let event: Value = sqlx::query_scalar("SELECT payload FROM events WHERE kind = 'email_given'")
        .fetch_one(app.pool())
        .await
        .unwrap();
    assert_eq!(event["source"], "agent");
    // Nothing exists yet: no account, no site.
    assert_eq!(count(&app, "SELECT count(*) FROM accounts").await, 0);
    assert_eq!(count(&app, "SELECT count(*) FROM sites").await, 0);
}

#[tokio::test]
async fn opening_the_link_shows_the_confirm_page_and_changes_nothing() {
    let app = cloud().await;
    request(&app, "owner@example.org").await;
    let path = link_for(&app, "owner@example.org");

    let before = (
        count(&app, "SELECT count(*) FROM accounts").await,
        count(&app, "SELECT count(*) FROM sites").await,
        count(&app, "SELECT count(*) FROM crawls").await,
        count(&app, "SELECT count(*) FROM api_keys").await,
        count(&app, "SELECT count(*) FROM sessions").await,
        count(&app, "SELECT count(*) FROM events").await,
    );
    // A mail scanner opens it, twice, and so does the person.
    for _ in 0..3 {
        let res = app.get(&path, None).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.body);
        assert!(res.body.contains("Monitor example.com?"), "{}", res.body);
        assert!(res.body.contains("owner@example.org"));
        assert!(
            res.body
                .contains(&format!(r#"method="post" action="{path}""#))
        );
        assert!(res.body.contains("Start monitoring"));
        // The button is swallowed after the first press, so a double click can't use the link
        // up on a request whose answer (the key) the person never sees.
        assert!(res.body.contains("data-once"), "{}", res.body);
        assert!(!res.body.contains("cdo_"));
        assert!(res.cookie("codoseo_session").is_none());
        assert_eq!(res.header("cache-control"), Some("no-store"));
    }
    let after = (
        count(&app, "SELECT count(*) FROM accounts").await,
        count(&app, "SELECT count(*) FROM sites").await,
        count(&app, "SELECT count(*) FROM crawls").await,
        count(&app, "SELECT count(*) FROM api_keys").await,
        count(&app, "SELECT count(*) FROM sessions").await,
        count(&app, "SELECT count(*) FROM events").await,
    );
    assert_eq!(before, after);
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM login_tokens WHERE used_at IS NULL"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn confirming_creates_the_account_the_weekly_site_the_first_crawl_and_one_key() {
    let app = cloud().await;
    request(&app, "Owner@Example.org").await;
    let path = link_for(&app, "Owner@Example.org");

    let res = app.post(&path, "", None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.header("cache-control"), Some("no-store"));
    assert!(
        res.body.contains("Monitoring started for example.com"),
        "{}",
        res.body
    );
    assert!(res.body.contains("owner@example.org") || res.body.contains("Owner@Example.org"));

    // A Free account for the address, signed in.
    let (account, plan, clicked): (uuid::Uuid, String, bool) =
        sqlx::query_as("SELECT id, plan::text, last_email_click_at IS NOT NULL FROM accounts")
            .fetch_one(app.pool())
            .await
            .unwrap();
    assert_eq!(plan, "free");
    assert!(clicked, "the click counts as activity");
    let cookie = res.cookie("codoseo_session").expect("a session");
    assert_eq!(count(&app, "SELECT count(*) FROM sessions").await, 1);

    // The site is weekly and monitored; its first crawl is queued in lane 1.
    let (site, schedule, active, start_url): (uuid::Uuid, Option<String>, bool, String) =
        sqlx::query_as(
            "SELECT id, schedule, monitoring_active, start_url FROM sites WHERE account_id = $1",
        )
        .bind(account)
        .fetch_one(app.pool())
        .await
        .unwrap();
    assert_eq!(schedule.as_deref(), Some("weekly"));
    assert!(active);
    assert_eq!(start_url, "https://example.com/blog");
    let (trigger, priority, status): (String, i16, String) = sqlx::query_as(
        "SELECT trigger::text, priority, status::text FROM crawls WHERE site_id = $1",
    )
    .bind(site)
    .fetch_one(app.pool())
    .await
    .unwrap();
    assert_eq!(
        (trigger.as_str(), priority, status.as_str()),
        ("first", 1, "queued")
    );
    // Default alert rules on the account's email channel.
    assert!(count(&app, "SELECT count(*) FROM alert_rules").await > 0);
    assert!(count(&app, "SELECT count(*) FROM alert_channels").await >= 1);

    // Exactly one key, shown once, stored only as its hash.
    let key = key_in(&res);
    assert!(keys::is_well_formed(&key), "{key}");
    assert_eq!(
        res.body.matches(&key).count(),
        6,
        "the key, the command and the JSON, each shown and in its copy button"
    );
    assert!(res.body.contains(&format!(
        "claude mcp add --transport http codoseo https://codoseo.com/mcp --header &#34;Authorization: Bearer {key}&#34;"
    )), "{}", res.body);
    assert!(res.body.contains("&#34;mcpServers&#34;"));
    assert!(res.body.contains("https://codoseo.com/mcp"));
    let (name, hash): (String, Vec<u8>) = sqlx::query_as("SELECT name, key_hash FROM api_keys")
        .fetch_one(app.pool())
        .await
        .unwrap();
    assert_eq!(name, "Agent (start_monitoring)");
    assert_eq!(hash, keys::hash_key(&key));
    assert_eq!(
        count(
            &app,
            &format!("SELECT count(*) FROM api_keys k WHERE k::text LIKE '%{key}%'")
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &app,
            &format!("SELECT count(*) FROM login_tokens t WHERE t::text LIKE '%{key}%'")
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &app,
            &format!("SELECT count(*) FROM events e WHERE e::text LIKE '%{key}%'")
        )
        .await,
        0
    );

    // The funnel: the click is an agent's, with the account and site.
    let event: (Option<uuid::Uuid>, Option<uuid::Uuid>, Value) = sqlx::query_as(
        "SELECT account_id, site_id, payload FROM events WHERE kind = 'link_clicked'",
    )
    .fetch_one(app.pool())
    .await
    .unwrap();
    assert_eq!((event.0, event.1), (Some(account), Some(site)));
    assert_eq!(event.2["source"], "agent");

    // The session and the key both work.
    let page = app.get(&format!("/s/{site}/audit"), Some(&cookie)).await;
    assert_eq!(page.status, StatusCode::OK);
    let sites = app
        .send(
            axum::http::Request::builder()
                .uri("/api/v1/sites")
                .header("authorization", format!("Bearer {key}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(sites.status, StatusCode::OK, "{}", sites.body);
    assert!(sites.body.contains(DOMAIN));
    assert!(res.body.contains(&format!(r#"href="/s/{site}/audit""#)));
}

#[tokio::test]
async fn a_second_click_is_already_used_and_makes_no_second_key_or_site() {
    let app = cloud().await;
    request(&app, "owner@example.org").await;
    let path = link_for(&app, "owner@example.org");
    let first = app.post(&path, "", None).await;
    assert_eq!(first.status, StatusCode::OK);

    for _ in 0..2 {
        let again = app.post(&path, "", None).await;
        assert_eq!(again.status, StatusCode::GONE, "{}", again.body);
        assert!(
            again.body.contains("expired or was already used"),
            "{}",
            again.body
        );
        assert!(!again.body.contains("cdo_"));
        assert!(again.cookie("codoseo_session").is_none());
    }
    let get = app.get(&path, None).await;
    assert_eq!(get.status, StatusCode::GONE);
    assert!(!get.body.contains("Start monitoring"));
    assert_eq!(count(&app, "SELECT count(*) FROM api_keys").await, 1);
    assert_eq!(count(&app, "SELECT count(*) FROM sites").await, 1);
    assert_eq!(count(&app, "SELECT count(*) FROM crawls").await, 1);
    assert_eq!(count(&app, "SELECT count(*) FROM sessions").await, 1);
}

#[tokio::test]
async fn the_used_link_page_points_to_settings_and_a_signed_in_visitor_is_not_asked_to_sign_in() {
    let app = cloud().await;
    request(&app, "owner@example.org").await;
    let path = link_for(&app, "owner@example.org");
    let first = app.post(&path, "", None).await;
    let session = first.cookie("codoseo_session").expect("signed in");

    let note = "If you already confirmed, your key was shown once. You can create a new one \
                under <a href=\"/settings/api-keys\">Settings";
    let signed_out = app.get(&path, None).await;
    assert_eq!(signed_out.status, StatusCode::GONE);
    assert!(signed_out.body.contains(note), "{}", signed_out.body);
    assert!(signed_out.body.contains(r#"href="/login""#));

    for res in [
        app.get(&path, Some(&session)).await,
        app.post(&path, "", Some(&session)).await,
    ] {
        assert_eq!(res.status, StatusCode::GONE);
        assert!(res.body.contains(note), "{}", res.body);
        assert!(!res.body.contains(r#"href="/login""#), "{}", res.body);
        assert!(!res.body.contains("cdo_"));
    }
}

#[tokio::test]
async fn two_clicks_at_once_make_one_key_and_one_site() {
    let app = cloud().await;
    request(&app, "owner@example.org").await;
    let path = link_for(&app, "owner@example.org");
    let results = futures_util::future::join_all((0..6).map(|_| app.post(&path, "", None))).await;
    let ok = results
        .iter()
        .filter(|r| r.status == StatusCode::OK)
        .count();
    assert_eq!(
        ok,
        1,
        "{:?}",
        results.iter().map(|r| r.status).collect::<Vec<_>>()
    );
    assert_eq!(count(&app, "SELECT count(*) FROM api_keys").await, 1);
    assert_eq!(count(&app, "SELECT count(*) FROM sites").await, 1);
}

#[tokio::test]
async fn a_cross_site_post_is_refused_and_does_not_use_the_token() {
    let app = cloud().await;
    request(&app, "owner@example.org").await;
    let path = link_for(&app, "owner@example.org");
    let res = app
        .post_with_headers(&path, "", None, &[("origin", "https://evil.example")])
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert_eq!(count(&app, "SELECT count(*) FROM accounts").await, 0);
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM login_tokens WHERE used_at IS NULL"
        )
        .await,
        1
    );
    assert_eq!(app.post(&path, "", None).await.status, StatusCode::OK);
}

#[tokio::test]
async fn an_expired_or_unknown_token_gets_the_expired_page() {
    let app = cloud().await;
    request(&app, "owner@example.org").await;
    let path = link_for(&app, "owner@example.org");
    sqlx::query("UPDATE login_tokens SET expires_at = now() - interval '1 minute'")
        .execute(app.pool())
        .await
        .unwrap();
    for res in [
        app.get(&path, None).await,
        app.post(&path, "", None).await,
        app.get("/monitoring/start/not-a-token", None).await,
        app.post("/monitoring/start/not-a-token", "", None).await,
    ] {
        assert_eq!(res.status, StatusCode::GONE, "{}", res.body);
        assert!(res.body.contains("This link has run out"), "{}", res.body);
    }
    assert_eq!(count(&app, "SELECT count(*) FROM accounts").await, 0);
    // A sign-in link is not a start-monitoring link.
    let magic = codoseo_web::auth::session::random_token();
    codoseo_store::auth::create_token(
        app.pool(),
        codoseo_store::auth::TokenPurpose::MagicLink,
        &codoseo_web::auth::session::hash(&magic),
        None,
        Some(json!({"email": "x@example.org", "next": "/"})),
        time::Duration::minutes(5),
    )
    .await
    .unwrap();
    let res = app
        .post(&format!("/monitoring/start/{magic}"), "", None)
        .await;
    assert_eq!(res.status, StatusCode::GONE);
    assert_eq!(count(&app, "SELECT count(*) FROM accounts").await, 0);
}

#[tokio::test]
async fn self_hosted_has_no_start_monitoring_route() {
    let app = TestApp::new().await;
    assert_eq!(
        app.get("/monitoring/start/abc", None).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.post("/monitoring/start/abc", "", None).await.status,
        StatusCode::NOT_FOUND
    );
}

// ---- an existing account ----

#[tokio::test]
async fn an_existing_account_gets_the_site_added_and_a_key_without_losing_anything() {
    let app = cloud().await;
    let (account, _) = app.login("owner@example.org").await;
    request(&app, "Owner@Example.org").await;
    let res = app
        .post(&link_for(&app, "Owner@Example.org"), "", None)
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("Monitoring started for example.com"));
    assert_eq!(
        count(&app, "SELECT count(*) FROM accounts").await,
        1,
        "the account is reused"
    );
    let owner: uuid::Uuid = sqlx::query_scalar("SELECT account_id FROM sites")
        .fetch_one(app.pool())
        .await
        .unwrap();
    assert_eq!(owner, account.id);
    assert_eq!(count(&app, "SELECT count(*) FROM api_keys").await, 1);
}

#[tokio::test]
async fn an_account_that_already_monitors_the_site_gets_no_second_site_or_crawl() {
    let app = cloud().await;
    let (account, _) = app.login("owner@example.org").await;
    let site = app.site(&account, DOMAIN).await;
    // The domain matches even at the plan's site limit (Free has one).
    request(&app, "owner@example.org").await;
    let res = app
        .post(&link_for(&app, "owner@example.org"), "", None)
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(
        res.body.contains("example.com is already monitored"),
        "{}",
        res.body
    );
    assert_eq!(count(&app, "SELECT count(*) FROM sites").await, 1);
    assert_eq!(count(&app, "SELECT count(*) FROM crawls").await, 0);
    assert!(
        res.body
            .contains(&format!(r#"href="/s/{}/audit""#, site.id))
    );
    assert_eq!(count(&app, "SELECT count(*) FROM api_keys").await, 1);
}

#[tokio::test]
async fn an_account_at_its_site_limit_gets_no_site_and_a_clear_message() {
    let app = cloud().await;
    let (account, _) = app.login("owner@example.org").await;
    app.site(&account, "other.com").await;
    request(&app, "owner@example.org").await;
    let res = app
        .post(&link_for(&app, "owner@example.org"), "", None)
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("Your plan is full"), "{}", res.body);
    assert!(
        res.body.contains("Your plan includes 1 site, so"),
        "{}",
        res.body
    );
    assert!(res.body.contains("example.com</b> was not added"));
    assert!(!res.body.contains("Monitoring started"));
    assert_eq!(count(&app, "SELECT count(*) FROM sites").await, 1);
    assert_eq!(count(&app, "SELECT count(*) FROM crawls").await, 0);
    // The key is still made: the account can use the API for its own site.
    assert!(res.body.contains("cdo_"));
    assert_eq!(count(&app, "SELECT count(*) FROM api_keys").await, 1);
}

#[tokio::test]
async fn an_account_with_twenty_keys_gets_no_twenty_first() {
    let app = cloud().await;
    let (account, _) = app.login("owner@example.org").await;
    for n in 0..20 {
        let key = keys::generate();
        codoseo_store::api_keys::create(
            app.pool(),
            account.id,
            &format!("k{n}"),
            &key.hash,
            &key.prefix,
            20,
        )
        .await
        .unwrap();
    }
    request(&app, "owner@example.org").await;
    let res = app
        .post(&link_for(&app, "owner@example.org"), "", None)
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("Monitoring started"));
    assert!(
        res.body.contains("already have 20 API keys"),
        "{}",
        res.body
    );
    assert!(!res.body.contains("cdo_"));
    assert_eq!(count(&app, "SELECT count(*) FROM api_keys").await, 20);
}

#[tokio::test]
async fn the_answer_is_the_same_whether_or_not_the_address_has_an_account() {
    let app = cloud().await;
    app.login("known@example.org").await;
    let known = request(&app, "known@example.org").await;
    let unknown = request(&app, "stranger@example.org").await;
    let normalise = |v: &Value, addr: &str| v.to_string().replace(addr, "ADDRESS");
    assert_eq!(
        normalise(&known, "known@example.org"),
        normalise(&unknown, "stranger@example.org")
    );
    assert_eq!(mails(&app), 2);
}

// ---- refusals ----

#[tokio::test]
async fn bad_and_throwaway_addresses_get_no_email() {
    let app = cloud().await;
    let c = client(&app);
    for (email, expect) in [
        ("not an email", "email address"),
        ("a@@example.org", "email address"),
        ("", "email address"),
        ("someone@mailinator.com", "permanent email"),
        ("someone@sub.guerrillamail.com", "permanent email"),
    ] {
        let err = c
            .call_err(
                "start_monitoring",
                json!({"url": "example.com", "email": email}),
            )
            .await;
        assert!(err.contains(expect), "{email}: {err}");
    }
    assert_eq!(mails(&app), 0);
    assert_eq!(count(&app, "SELECT count(*) FROM login_tokens").await, 0);
}

#[tokio::test]
async fn an_address_gets_three_emails_an_hour_whoever_asks() {
    let app = cloud().await;
    for _ in 0..3 {
        request(&app, "victim@example.org").await;
    }
    // Spelling variants are the same address.
    let c = client(&app);
    for variant in ["victim@example.org", "VICTIM+x@Example.org"] {
        let err = c
            .call_err(
                "start_monitoring",
                json!({"url": "example.com", "email": variant}),
            )
            .await;
        assert!(
            err.contains("several emails to that address"),
            "{variant}: {err}"
        );
    }
    assert_eq!(mails(&app), 3);
    assert_eq!(count(&app, "SELECT count(*) FROM login_tokens").await, 3);
    // Another address is unaffected, and the hour rolls over.
    request(&app, "other@example.org").await;
    sqlx::query("UPDATE login_tokens SET created_at = now() - interval '2 hours'")
        .execute(app.pool())
        .await
        .unwrap();
    request(&app, "victim@example.org").await;
}

#[tokio::test]
async fn a_direct_client_is_capped_per_ip_and_a_connector_is_not() {
    let app = cloud().await;
    let ip = "203.0.113.9";
    let direct = Client::new(&app, Duration::ZERO, Some(DIRECT_UA)).with_ip(ip);
    for n in 0..3 {
        direct
            .call_ok(
                "start_monitoring",
                json!({"url": "example.com", "email": format!("p{n}@example.org")}),
            )
            .await;
    }
    let err = direct
        .call_err(
            "start_monitoring",
            json!({"url": "example.com", "email": "p9@example.org"}),
        )
        .await;
    assert!(err.contains("This client has asked for several"), "{err}");
    // Another address is its own client.
    Client::new(&app, Duration::ZERO, Some(DIRECT_UA))
        .with_ip("203.0.113.10")
        .call_ok(
            "start_monitoring",
            json!({"url": "example.com", "email": "p9@example.org"}),
        )
        .await;
    // A hosted connector from the capped address is not capped per client.
    let shared = Client::new(&app, Duration::ZERO, Some(SHARED_UA)).with_ip(ip);
    for n in 0..5 {
        shared
            .call_ok(
                "start_monitoring",
                json!({"url": "example.com", "email": format!("q{n}@example.org")}),
            )
            .await;
    }
    assert_eq!(mails(&app), 9);
}

#[tokio::test]
async fn the_daily_email_cap_stops_everyone_and_an_eighth_of_it_is_the_hourly_cap() {
    let app = TestApp::with_config(cloud_config_with(&[("MCP_ANON_DAILY_EMAILS", "2")])).await;
    // An hourly share of 1: let each request leave the hour, not the day.
    for email in ["a@example.org", "b@example.org"] {
        request(&app, email).await;
        sqlx::query("UPDATE login_tokens SET created_at = now() - interval '2 hours'")
            .execute(app.pool())
            .await
            .unwrap();
    }
    let err = client(&app)
        .call_err(
            "start_monitoring",
            json!({"url": "example.com", "email": "c@example.org"}),
        )
        .await;
    assert!(err.contains("all the monitoring emails"), "{err}");
    assert_eq!(mails(&app), 2);

    // 32 a day is 4 an hour; twelve at once get four, and the rest hear it is the hour's cap.
    let app = TestApp::with_config(cloud_config_with(&[("MCP_ANON_DAILY_EMAILS", "32")])).await;
    let c = client(&app);
    let emails: Vec<String> = (0..12).map(|n| format!("p{n}@example.org")).collect();
    let results = futures_util::future::join_all(emails.iter().map(|e| {
        c.call(
            "start_monitoring",
            json!({"url": "example.com", "email": e}),
        )
    }))
    .await;
    assert_eq!(results.iter().filter(|(is_error, _)| !is_error).count(), 4);
    assert!(
        results
            .iter()
            .filter(|(is_error, _)| *is_error)
            .all(|(_, text)| text.contains("this hour")),
        "{results:?}"
    );
    assert_eq!(mails(&app), 4);
}

#[tokio::test]
async fn an_address_gets_five_emails_in_a_day_not_three_an_hour_all_day() {
    let app = cloud().await;
    for _ in 0..3 {
        request(&app, "victim@example.org").await;
    }
    sqlx::query("UPDATE login_tokens SET created_at = now() - interval '2 hours'")
        .execute(app.pool())
        .await
        .unwrap();
    for _ in 0..2 {
        request(&app, "victim@example.org").await;
    }
    let err = client(&app)
        .call_err(
            "start_monitoring",
            json!({"url": "example.com", "email": "Victim@example.org"}),
        )
        .await;
    assert!(err.contains("for today"), "{err}");
    assert_eq!(mails(&app), 5);
}

// ---- review fixes ----

#[tokio::test]
async fn a_confirmation_that_fails_half_way_leaves_the_link_usable_and_makes_nothing_twice() {
    let app = cloud().await;
    request(&app, "owner@example.org").await;
    let path = link_for(&app, "owner@example.org");

    // The key table is gone for a moment: the account and site are made, the key is not.
    sqlx::query("ALTER TABLE api_keys RENAME TO api_keys_off")
        .execute(app.pool())
        .await
        .unwrap();
    let failed = app.post(&path, "", None).await;
    assert_eq!(
        failed.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{}",
        failed.body
    );
    assert!(!failed.body.contains("cdo_"));
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM login_tokens WHERE used_at IS NULL"
        )
        .await,
        1
    );
    // The link opens its confirm page again.
    assert_eq!(app.get(&path, None).await.status, StatusCode::OK);
    sqlx::query("ALTER TABLE api_keys_off RENAME TO api_keys")
        .execute(app.pool())
        .await
        .unwrap();

    // Pressing it again finishes the job: the site made before is reused, one key is made.
    let res = app.post(&path, "", None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(
        res.body.contains("example.com is already monitored"),
        "{}",
        res.body
    );
    assert!(res.body.contains("cdo_"));
    assert_eq!(count(&app, "SELECT count(*) FROM api_keys").await, 1);
    assert_eq!(count(&app, "SELECT count(*) FROM sites").await, 1);
    assert_eq!(count(&app, "SELECT count(*) FROM crawls").await, 1);
    // And it is single use from then on.
    assert_eq!(app.post(&path, "", None).await.status, StatusCode::GONE);
}

#[tokio::test]
async fn the_first_crawl_is_an_agents_in_the_funnel_and_the_websites_funnel_stays_clean() {
    let app = cloud().await;
    request(&app, "owner@example.org").await;
    app.post(&link_for(&app, "owner@example.org"), "", None)
        .await;
    let (crawl, source): (uuid::Uuid, Option<String>) =
        sqlx::query_as("SELECT id, source FROM crawls WHERE trigger = 'first'")
            .fetch_one(app.pool())
            .await
            .unwrap();
    assert_eq!(source.as_deref(), Some("agent"));

    // The worker finishes it: the funnel step is recorded, as an agent's.
    app.finalize_crawl(
        crawl,
        vec![support::page(DOMAIN, "/")],
        Vec::new(),
        codoseo_core::output::StopReason::Completed,
    )
    .await;
    let event: Value =
        sqlx::query_scalar("SELECT payload FROM events WHERE kind = 'first_full_crawl'")
            .fetch_one(app.pool())
            .await
            .unwrap();
    assert_eq!(event["source"], "agent");

    use codoseo_store::events::{self, EventKind};
    let step = |counts: &[events::FunnelCount], kind| {
        counts.iter().find(|c| c.kind == kind).unwrap().events
    };
    let web = events::funnel_counts(app.pool(), 30).await.unwrap();
    let agents = events::agent_funnel_counts(app.pool(), 30).await.unwrap();
    for kind in [
        EventKind::EmailGiven,
        EventKind::LinkClicked,
        EventKind::FirstFullCrawl,
    ] {
        assert_eq!(step(&web, kind), 0, "{kind:?} is not the website's");
        assert_eq!(step(&agents, kind), 1, "{kind:?} is the agents'");
    }
}

#[tokio::test]
async fn a_start_monitoring_site_with_a_trailing_dot_is_stored_without_it() {
    let app = cloud().await;
    client(&app)
        .call_ok(
            "start_monitoring",
            json!({"url": "https://Example.com./blog", "email": "owner@example.org"}),
        )
        .await;
    let payload: Value = sqlx::query_scalar("SELECT payload FROM login_tokens")
        .fetch_one(app.pool())
        .await
        .unwrap();
    assert_eq!(payload["domain"], "example.com");
    assert_eq!(payload["start_url"], "https://example.com/blog");
    app.post(&link_for(&app, "owner@example.org"), "", None)
        .await;
    let domain: String = sqlx::query_scalar("SELECT domain FROM sites")
        .fetch_one(app.pool())
        .await
        .unwrap();
    assert_eq!(domain, "example.com");
}
