//! T7.4: the alerts settings screen: channels (add, delete, mute, re-enable, test) and the
//! per-site rules grid, with the plan limits and the account boundaries.

mod support;

use std::io;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::http::StatusCode;
use axum::routing::any;
use codoseo_core::change::ChangeKind;
use codoseo_core::crawl::AddressPolicy;
use codoseo_core::plan::Plan;
use codoseo_crawler::guard::Lookup;
use codoseo_notify::{ChannelKey, ChannelTarget, GuardedHttp};
use codoseo_store::accounts::Account;
use codoseo_store::{alert_rules, channels};
use codoseo_testkit::TestServer;
use support::{TestApp, TestResponse, cloud_config};
use uuid::Uuid;

/// Names resolve to a public address, so only the rules under test decide.
struct Public;

impl Lookup for Public {
    async fn lookup(&self, _host: &str) -> io::Result<Vec<IpAddr>> {
        Ok(vec!["93.184.216.34".parse().unwrap()])
    }
}

const SLACK: &str = "https://hooks.slack.com/services/T0/B0/secrettoken123";

fn key() -> ChannelKey {
    ChannelKey::derive("test-secret")
}

async fn cloud() -> TestApp {
    let http = GuardedHttp::with_lookup(AddressPolicy::Public, Public).unwrap();
    TestApp::with_notify_http(cloud_config(), http).await
}

async fn self_hosted() -> TestApp {
    TestApp::new().await
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

async fn add_channel(app: &TestApp, cookie: &str, kind: &str, target: &str) -> TestResponse {
    app.post(
        "/settings/alerts/channels",
        &format!("kind={kind}&target={}", enc(target)),
        Some(cookie),
    )
    .await
}

async fn channel_ids(app: &TestApp, account: &Account) -> Vec<Uuid> {
    channels::list_for_account(app.pool(), &key(), account.id)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.id)
        .collect()
}

async fn state(app: &TestApp, id: Uuid) -> channels::ChannelState {
    channels::state(app.pool(), id).await.unwrap().unwrap()
}

async fn channel_of_kind(app: &TestApp, account: &Account, kind: &str) -> Uuid {
    sqlx::query_scalar(
        "SELECT id FROM alert_channels WHERE account_id = $1 AND kind = $2::alert_channel_kind",
    )
    .bind(account.id)
    .bind(kind)
    .fetch_one(app.pool())
    .await
    .unwrap()
}

// ---- the page ------------------------------------------------------------------------------

#[tokio::test]
async fn the_page_needs_a_sign_in() {
    let app = cloud().await;
    let res = app.get("/settings/alerts", None).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert!(res.location().unwrap().starts_with("/login"));
}

#[tokio::test]
async fn the_page_lists_the_default_email_channel_and_the_default_rules() {
    let app = cloud().await;
    let (account, cookie) = app
        .login_with_plan("ana@example.com", Some(Plan::Pro))
        .await;
    let site = app.site(&account, "example.com").await;

    let res = app.get("/settings/alerts", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("ana@example.com"), "{}", res.body);
    assert!(res.body.contains("Active"));
    assert!(res.body.contains("example.com"));
    // The default channel exists now, with the five default rules on this site.
    let email = channel_of_kind(&app, &account, "email").await;
    for kind in alert_rules::DEFAULT_INSTANT {
        assert_eq!(
            alert_rules::instant_channels_for(app.pool(), site.id, kind)
                .await
                .unwrap(),
            vec![email]
        );
    }
    assert_eq!(res.body.matches(" checked").count(), 5, "{}", res.body);
    // The sidebar links here.
    assert!(res.body.contains("href=\"/settings/alerts\""));
}

#[tokio::test]
async fn the_free_plan_is_offered_email_only_with_an_upgrade_hint() {
    let app = cloud().await;
    let (_, free) = app
        .login_with_plan("free@example.com", Some(Plan::Free))
        .await;
    let res = app.get("/settings/alerts", Some(&free)).await;
    assert!(!res.body.contains("value=\"slack\""));
    assert!(res.body.contains("Upgrade to Pro"));

    let (_, pro) = app
        .login_with_plan("pro@example.com", Some(Plan::Pro))
        .await;
    let res = app.get("/settings/alerts", Some(&pro)).await;
    for kind in ["email", "slack", "discord", "webhook"] {
        assert!(res.body.contains(&format!("value=\"{kind}\"")), "{kind}");
    }
    assert!(!res.body.contains("Upgrade to Pro"));
}

// ---- adding channels -----------------------------------------------------------------------

#[tokio::test]
async fn adding_a_slack_channel_stores_it_encrypted_and_shows_only_the_host() {
    let app = cloud().await;
    let (account, cookie) = app
        .login_with_plan("ana@example.com", Some(Plan::Pro))
        .await;
    let one = app.site(&account, "one.example.com").await;
    let two = app.site(&account, "two.example.com").await;

    let res = add_channel(&app, &cookie, "slack", SLACK).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.location(), Some("/settings/alerts"));

    let slack = channel_of_kind(&app, &account, "slack").await;
    assert_eq!(
        channels::get_target(app.pool(), &key(), slack)
            .await
            .unwrap(),
        Some(ChannelTarget::Slack {
            url: SLACK.parse().unwrap()
        })
    );
    // Default rules on every site of the account.
    for site in [one.id, two.id] {
        assert!(
            alert_rules::instant_channels_for(app.pool(), site, ChangeKind::ErrorSpike)
                .await
                .unwrap()
                .contains(&slack)
        );
    }

    let page = app.get("/settings/alerts", Some(&cookie)).await;
    assert!(page.body.contains("hooks.slack.com"));
    assert!(
        !page.body.contains("secrettoken123"),
        "the token never shows"
    );
}

#[tokio::test]
async fn a_free_account_cannot_add_slack_in_the_cloud_but_can_add_another_email() {
    let app = cloud().await;
    let (account, cookie) = app
        .login_with_plan("free@example.com", Some(Plan::Free))
        .await;

    let res = add_channel(&app, &cookie, "slack", SLACK).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert!(res.body.contains("Upgrade to Pro"), "{}", res.body);
    assert!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM alert_channels WHERE kind = 'slack'")
            .fetch_one(app.pool())
            .await
            .unwrap()
            == 0
    );

    let res = add_channel(&app, &cookie, "email", "team@example.com").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    let listed = channels::list_for_account(app.pool(), &key(), account.id)
        .await
        .unwrap();
    assert!(listed.iter().any(|c| c.target == "team@example.com"));
}

#[tokio::test]
async fn self_hosted_can_add_every_kind_including_private_addresses() {
    let app = self_hosted().await;
    let (account, cookie) = app.login("owner@example.com").await;
    assert_eq!(
        add_channel(&app, &cookie, "slack", SLACK).await.status,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        add_channel(&app, &cookie, "webhook", "http://10.0.0.5:9000/hook")
            .await
            .status,
        StatusCode::OK,
        "a webhook shows its secret on the page it answers with"
    );
    assert_eq!(
        add_channel(
            &app,
            &cookie,
            "discord",
            "https://discord.com/api/webhooks/1/abc"
        )
        .await
        .status,
        StatusCode::SEE_OTHER
    );
    // The three, and the account's own address (created when the webhook page was shown).
    assert_eq!(channel_ids(&app, &account).await.len(), 4);
}

#[tokio::test]
async fn bad_targets_are_refused_with_a_friendly_inline_error() {
    let app = cloud().await;
    let (account, cookie) = app
        .login_with_plan("ana@example.com", Some(Plan::Pro))
        .await;
    for (kind, target) in [
        ("slack", "https://example.com/not-slack"),
        ("webhook", "http://169.254.169.254/latest"),
        ("webhook", "http://10.0.0.5/hook"),
        ("webhook", "not a url"),
        ("discord", ""),
        ("email", "no-at-sign"),
        ("pagerduty", "https://example.com/"),
    ] {
        let res = app
            .post_hx(
                "/settings/alerts/channels",
                &format!("kind={kind}&target={}", enc(target)),
                Some(&cookie),
            )
            .await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{kind} {target}");
        assert_eq!(res.header("hx-retarget"), Some("#add-channel"));
        assert!(
            res.body.contains("error-text"),
            "{kind} {target}: {}",
            res.body
        );
        assert!(res.body.contains("name=\"target\""), "the form comes back");
    }
    // Only the default email channel (if even that) exists.
    assert!(
        channels::list_for_account(app.pool(), &key(), account.id)
            .await
            .unwrap()
            .iter()
            .all(|c| c.is_default)
    );
}

#[tokio::test]
async fn a_webhook_shows_its_signing_secret_once() {
    let app = cloud().await;
    let (account, cookie) = app
        .login_with_plan("ana@example.com", Some(Plan::Pro))
        .await;
    let res = add_channel(&app, &cookie, "webhook", "https://alerts.example.com/hook").await;
    assert_eq!(res.status, StatusCode::OK);
    let id = channel_of_kind(&app, &account, "webhook").await;
    let Some(ChannelTarget::Webhook { secret, .. }) =
        channels::get_target(app.pool(), &key(), id).await.unwrap()
    else {
        panic!("a webhook target");
    };
    assert!(secret.len() >= 32, "{secret}");
    assert!(res.body.contains(&secret), "shown on creation");
    assert_eq!(res.header("cache-control"), Some("no-store"));

    let later = app.get("/settings/alerts", Some(&cookie)).await;
    assert!(!later.body.contains(&secret), "never again");
    assert!(later.body.contains("alerts.example.com"));
}

// ---- managing channels ---------------------------------------------------------------------

#[tokio::test]
async fn channels_can_be_muted_unmuted_re_enabled_and_deleted_but_the_default_stays() {
    let app = self_hosted().await;
    let (account, cookie) = app.login("owner@example.com").await;
    app.get("/settings/alerts", Some(&cookie)).await; // creates the default channel
    add_channel(&app, &cookie, "slack", SLACK).await;
    let slack = channel_of_kind(&app, &account, "slack").await;
    let default = channel_of_kind(&app, &account, "email").await;

    let res = app
        .post(
            &format!("/settings/alerts/channels/{slack}/mute"),
            "muted=true",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert!(state(&app, slack).await.muted);
    assert!(
        app.get("/settings/alerts", Some(&cookie))
            .await
            .body
            .contains("Muted")
    );
    app.post(
        &format!("/settings/alerts/channels/{slack}/mute"),
        "muted=false",
        Some(&cookie),
    )
    .await;
    assert!(!state(&app, slack).await.muted);
    // The default channel can be muted too.
    app.post(
        &format!("/settings/alerts/channels/{default}/mute"),
        "muted=true",
        Some(&cookie),
    )
    .await;
    assert!(state(&app, default).await.muted);

    // Switched off after failures: the page says why, and "Turn on" resets it.
    channels::record_failure(app.pool(), slack, "HTTP 500: boom")
        .await
        .unwrap();
    channels::disable(app.pool(), slack, "HTTP 500: boom")
        .await
        .unwrap();
    let page = app.get("/settings/alerts", Some(&cookie)).await;
    assert!(page.body.contains("Turned off"));
    assert!(page.body.contains("HTTP 500: boom"));
    let res = app
        .post(
            &format!("/settings/alerts/channels/{slack}/enable"),
            "",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert!(state(&app, slack).await.enabled);
    let listed = channels::list_for_account(app.pool(), &key(), account.id)
        .await
        .unwrap();
    let s = listed.iter().find(|c| c.id == slack).unwrap();
    assert_eq!((s.consecutive_failures, s.last_error.clone()), (0, None));

    // Deleting: the default is refused, the other goes (with its rules).
    let res = app
        .post(
            &format!("/settings/alerts/channels/{default}/delete"),
            "",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert!(
        channels::state(app.pool(), default)
            .await
            .unwrap()
            .is_some()
    );
    let res = app
        .post(
            &format!("/settings/alerts/channels/{slack}/delete"),
            "",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert!(channels::state(app.pool(), slack).await.unwrap().is_none());
}

// ---- send test -----------------------------------------------------------------------------

async fn hook_server(status: u16) -> (TestServer, Arc<Mutex<Vec<serde_json::Value>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let state = seen.clone();
    let app = Router::new().fallback(any(move |body: axum::body::Bytes| {
        let state = state.clone();
        async move {
            if let Ok(v) = serde_json::from_slice(&body) {
                state.lock().unwrap().push(v);
            }
            StatusCode::from_u16(status).unwrap()
        }
    }));
    (TestServer::start(app).await, seen)
}

#[tokio::test]
async fn send_test_posts_a_test_message_and_swaps_in_sent() {
    let app = self_hosted().await;
    let (account, cookie) = app.login("owner@example.com").await;
    app.site(&account, "example.com").await;
    let (server, seen) = hook_server(200).await;
    let res = add_channel(&app, &cookie, "webhook", server.url("/hook").as_str()).await;
    assert_eq!(res.status, StatusCode::OK);
    let id = channel_of_kind(&app, &account, "webhook").await;

    let res = app
        .post_hx(
            &format!("/settings/alerts/channels/{id}/test"),
            "",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("Sent"), "{}", res.body);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["event"], "test");
    assert_eq!(seen[0]["site"], "example.com");
}

#[tokio::test]
async fn send_test_to_a_failing_endpoint_shows_the_error() {
    let app = self_hosted().await;
    let (account, cookie) = app.login("owner@example.com").await;
    let (server, _) = hook_server(500).await;
    add_channel(&app, &cookie, "webhook", server.url("/hook").as_str()).await;
    let id = channel_of_kind(&app, &account, "webhook").await;

    let res = app
        .post_hx(
            &format!("/settings/alerts/channels/{id}/test"),
            "",
            Some(&cookie),
        )
        .await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "an error line, not an error page"
    );
    assert!(res.body.contains("500"), "{}", res.body);
    assert!(!res.body.contains("Sent"));
    assert!(
        !res.body.contains(server.url("/hook").as_str()),
        "no URL in the error"
    );
}

#[tokio::test]
async fn send_test_to_an_email_channel_sends_a_test_email() {
    let app = self_hosted().await;
    let (_, cookie) = app.login("owner@example.com").await;
    app.get("/settings/alerts", Some(&cookie)).await;
    let id: Uuid = sqlx::query_scalar("SELECT id FROM alert_channels WHERE is_default")
        .fetch_one(app.pool())
        .await
        .unwrap();
    let res = app
        .post_hx(
            &format!("/settings/alerts/channels/{id}/test"),
            "",
            Some(&cookie),
        )
        .await;
    assert!(res.body.contains("Sent"), "{}", res.body);
    let mail = app.mail.lock().unwrap();
    assert_eq!(mail.len(), 1);
    assert_eq!(mail[0].to, "owner@example.com");
    assert!(
        mail[0].subject.contains("Test notification"),
        "{}",
        mail[0].subject
    );
}

#[tokio::test]
async fn send_test_is_refused_for_an_address_the_cloud_guard_blocks() {
    // A channel saved while self-hosted and carried to the cloud, or a stored private target:
    // the test button goes through the same guard as delivery.
    let app = cloud().await;
    let (account, cookie) = app
        .login_with_plan("ana@example.com", Some(Plan::Pro))
        .await;
    let (server, seen) = hook_server(200).await;
    let id = channels::create(
        app.pool(),
        &key(),
        account.id,
        &ChannelTarget::Webhook {
            url: server.url("/hook"),
            secret: "s".into(),
        },
        None,
        false,
    )
    .await
    .unwrap();
    let res = app
        .post_hx(
            &format!("/settings/alerts/channels/{id}/test"),
            "",
            Some(&cookie),
        )
        .await;
    assert!(!res.body.contains("Sent"), "{}", res.body);
    assert!(
        seen.lock().unwrap().is_empty(),
        "nothing reached the server"
    );
}

// ---- the rules grid --------------------------------------------------------------------------

#[tokio::test]
async fn grid_toggles_persist() {
    let app = self_hosted().await;
    let (account, cookie) = app.login("owner@example.com").await;
    let site = app.site(&account, "example.com").await;
    app.get("/settings/alerts", Some(&cookie)).await;
    let email = channel_of_kind(&app, &account, "email").await;
    let rule = |kind: &str, instant: bool| {
        format!(
            "site={}&kind={kind}&channel={email}{}",
            site.id,
            if instant { "&instant=1" } else { "" }
        )
    };

    // A default off, a digest-by-default kind on.
    let res = app
        .post_hx(
            "/settings/alerts/rules",
            &rule("error_spike", false),
            Some(&cookie),
        )
        .await;
    assert!(res.status.is_success(), "{}", res.status);
    app.post_hx(
        "/settings/alerts/rules",
        &rule("title_changed", true),
        Some(&cookie),
    )
    .await;

    let instant = |kind| {
        let pool = app.pool().clone();
        let site = site.id;
        async move {
            alert_rules::instant_channels_for(&pool, site, kind)
                .await
                .unwrap()
                .contains(&email)
        }
    };
    assert!(!instant(ChangeKind::ErrorSpike).await);
    assert!(instant(ChangeKind::TitleChanged).await);
    assert!(instant(ChangeKind::SiteMoved).await);

    // And the page shows it: 5 defaults - 1 + 1 = 5 boxes checked.
    let page = app.get("/settings/alerts", Some(&cookie)).await;
    assert_eq!(page.body.matches(" checked").count(), 5);
    // Back on.
    app.post_hx(
        "/settings/alerts/rules",
        &rule("error_spike", true),
        Some(&cookie),
    )
    .await;
    assert!(instant(ChangeKind::ErrorSpike).await);
}

#[tokio::test]
async fn an_unknown_change_kind_is_a_bad_request() {
    let app = self_hosted().await;
    let (account, cookie) = app.login("owner@example.com").await;
    let site = app.site(&account, "example.com").await;
    app.get("/settings/alerts", Some(&cookie)).await;
    let email = channel_of_kind(&app, &account, "email").await;
    let res = app
        .post(
            "/settings/alerts/rules",
            &format!("site={}&kind=nonsense&channel={email}&instant=1", site.id),
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
}

// ---- accounts and origins --------------------------------------------------------------------

#[tokio::test]
async fn another_accounts_channel_or_site_is_a_404() {
    let app = self_hosted().await;
    let (mine, mine_cookie) = app.login("me@example.com").await;
    let (theirs, their_cookie) = app.login("them@example.com").await;
    let their_site = app.site(&theirs, "theirs.example.com").await;
    let my_site = app.site(&mine, "mine.example.com").await;
    app.get("/settings/alerts", Some(&their_cookie)).await;
    app.get("/settings/alerts", Some(&mine_cookie)).await;
    let their_channel = channel_of_kind(&app, &theirs, "email").await;
    let my_channel = channel_of_kind(&app, &mine, "email").await;

    for action in ["delete", "mute", "enable", "test"] {
        let res = app
            .post(
                &format!("/settings/alerts/channels/{their_channel}/{action}"),
                "muted=true",
                Some(&mine_cookie),
            )
            .await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{action}");
    }
    assert!(
        channels::state(app.pool(), their_channel)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        !channels::state(app.pool(), their_channel)
            .await
            .unwrap()
            .unwrap()
            .muted
    );

    // A rule can't join my site to their channel, or their site to mine.
    for (site, channel) in [(my_site.id, their_channel), (their_site.id, my_channel)] {
        let res = app
            .post(
                "/settings/alerts/rules",
                &format!("site={site}&kind=error_spike&channel={channel}&instant=1"),
                Some(&mine_cookie),
            )
            .await;
        assert_eq!(res.status, StatusCode::NOT_FOUND);
    }
    // Neither shows on the other's page.
    let page = app.get("/settings/alerts", Some(&mine_cookie)).await;
    assert!(!page.body.contains("them@example.com"));
    assert!(!page.body.contains("theirs.example.com"));
}

#[tokio::test]
async fn a_cross_origin_post_is_rejected() {
    let app = self_hosted().await;
    let (account, cookie) = app.login("owner@example.com").await;
    app.get("/settings/alerts", Some(&cookie)).await;
    let default = channel_of_kind(&app, &account, "email").await;
    for (path, body) in [
        (
            "/settings/alerts/channels".to_owned(),
            "kind=email&target=x@example.com".to_owned(),
        ),
        (
            format!("/settings/alerts/channels/{default}/mute"),
            "muted=true".to_owned(),
        ),
        (
            format!("/settings/alerts/channels/{default}/test"),
            String::new(),
        ),
        ("/settings/alerts/rules".to_owned(), "x=1".to_owned()),
    ] {
        let req = axum::http::Request::builder()
            .method("POST")
            .uri(&path)
            .header("cookie", &cookie)
            .header("origin", "https://evil.example")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(axum::body::Body::from(body))
            .unwrap();
        let res = app.send(req).await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{path}");
    }
    assert!(
        !channels::state(app.pool(), default)
            .await
            .unwrap()
            .unwrap()
            .muted
    );
}

// ---- default rules when a site is added -------------------------------------------------------

#[tokio::test]
async fn adding_a_site_turns_on_the_default_rules_for_the_default_email_channel() {
    let app = self_hosted().await;
    let (account, cookie) = app.login("owner@example.com").await;
    let res = app.post("/sites", "url=example.com", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    let site: Uuid = sqlx::query_scalar("SELECT id FROM sites WHERE account_id = $1")
        .bind(account.id)
        .fetch_one(app.pool())
        .await
        .unwrap();
    let email = channel_of_kind(&app, &account, "email").await;
    for kind in alert_rules::DEFAULT_INSTANT {
        assert_eq!(
            alert_rules::instant_channels_for(app.pool(), site, kind)
                .await
                .unwrap(),
            vec![email],
            "{kind:?}"
        );
    }
}

#[tokio::test]
async fn a_site_added_after_a_slack_channel_gets_its_default_rules_too() {
    let app = cloud().await;
    let (account, cookie) = app
        .login_with_plan("ana@example.com", Some(Plan::Pro))
        .await;
    app.site(&account, "one.example.com").await;
    add_channel(&app, &cookie, "slack", SLACK).await;
    let slack = channel_of_kind(&app, &account, "slack").await;

    let res = app
        .post("/sites", "url=two.example.com", Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    let two: Uuid = sqlx::query_scalar("SELECT id FROM sites WHERE domain = 'two.example.com'")
        .fetch_one(app.pool())
        .await
        .unwrap();
    let email = channel_of_kind(&app, &account, "email").await;
    for kind in alert_rules::DEFAULT_INSTANT {
        let mut on = alert_rules::instant_channels_for(app.pool(), two, kind)
            .await
            .unwrap();
        on.sort();
        let mut want = vec![email, slack];
        want.sort();
        assert_eq!(on, want, "{kind:?}");
    }
}

#[tokio::test]
async fn send_test_is_limited_to_five_a_hour_per_account() {
    let app = self_hosted().await;
    let (_, cookie) = app.login("owner@example.com").await;
    let (_, other_cookie) = app.login("other@example.com").await;
    app.get("/settings/alerts", Some(&cookie)).await;
    app.get("/settings/alerts", Some(&other_cookie)).await;
    let id: Uuid = sqlx::query_scalar(
        "SELECT c.id FROM alert_channels c JOIN accounts a ON a.id = c.account_id WHERE a.email = 'owner@example.com'",
    )
    .fetch_one(app.pool())
    .await
    .unwrap();
    let other: Uuid = sqlx::query_scalar(
        "SELECT c.id FROM alert_channels c JOIN accounts a ON a.id = c.account_id WHERE a.email = 'other@example.com'",
    )
    .fetch_one(app.pool())
    .await
    .unwrap();

    for n in 1..=5 {
        let res = app
            .post_hx(
                &format!("/settings/alerts/channels/{id}/test"),
                "",
                Some(&cookie),
            )
            .await;
        assert_eq!(res.status, StatusCode::OK, "send {n}");
        assert!(res.body.contains("Sent"));
    }
    let res = app
        .post_hx(
            &format!("/settings/alerts/channels/{id}/test"),
            "",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        res.body.contains("Too many test messages") && res.body.contains("try again in"),
        "{}",
        res.body
    );
    assert_eq!(app.mail.lock().unwrap().len(), 5, "the sixth sent nothing");

    // Another account has its own allowance.
    let res = app
        .post_hx(
            &format!("/settings/alerts/channels/{other}/test"),
            "",
            Some(&other_cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);

    // An hour later the allowance is back.
    sqlx::query(
        "UPDATE events SET created_at = now() - interval '61 minutes' WHERE kind = 'channel_test'",
    )
    .execute(app.pool())
    .await
    .unwrap();
    let res = app
        .post_hx(
            &format!("/settings/alerts/channels/{id}/test"),
            "",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
}
