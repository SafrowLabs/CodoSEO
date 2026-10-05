//! M8 T1: the API keys screen: list, create (shown once, only the hash stored), revoke, the
//! usage line and the account and origin boundaries.

mod support;

use axum::http::StatusCode;
use codoseo_core::plan::Plan;
use codoseo_store::api_keys::{self, CreateKeyOutcome};
use codoseo_web::agent::keys;
use support::{TestApp, TestResponse, cloud_config};
use uuid::Uuid;

async fn cloud() -> TestApp {
    TestApp::with_config(cloud_config()).await
}

/// The key shown on a create response.
fn shown_key(res: &TestResponse) -> String {
    let at = res.body.find("cdo_").expect("a key on the page");
    res.body[at..at + 47].to_owned()
}

async fn make_key(app: &TestApp, account: Uuid, name: &str) -> Uuid {
    let key = keys::generate();
    match api_keys::create(
        app.pool(),
        account,
        name,
        &key.hash,
        &key.prefix,
        api_keys::MAX_LIVE_KEYS,
    )
    .await
    .unwrap()
    {
        CreateKeyOutcome::Created(k) => k.id,
        CreateKeyOutcome::LimitReached => panic!("at the cap"),
    }
}

#[tokio::test]
async fn the_page_needs_a_sign_in() {
    let app = cloud().await;
    let res = app.get("/settings/api-keys", None).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert!(res.location().unwrap().starts_with("/login"));
    let res = app.post("/settings/api-keys", "name=x", None).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn the_page_lists_the_keys_and_the_mcp_setup() {
    let app = cloud().await;
    let (account, cookie) = app.login("ana@example.com").await;
    let (other, _) = app.login("bob@example.com").await;
    make_key(&app, account.id, "Claude Code").await;
    make_key(&app, other.id, "Bobs key").await;

    let res = app.get("/settings/api-keys", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("Claude Code"));
    assert!(res.body.contains("never used"));
    assert!(res.body.contains("cdo_"), "the prefix shows");
    assert!(!res.body.contains("Bobs key"), "only this account's keys");
    assert!(
        res.body
            .contains("claude mcp add --transport http codoseo https://codoseo.com/mcp --header")
    );
    assert!(res.body.contains("Authorization: Bearer"));
    // The sidebar links here, beside Alerts.
    assert!(res.body.contains("href=\"/settings/api-keys\""));
    assert!(res.body.contains("href=\"/settings/alerts\""));
    let alerts = app.get("/settings/alerts", Some(&cookie)).await;
    assert!(alerts.body.contains("href=\"/settings/api-keys\""));
}

#[tokio::test]
async fn an_empty_list_says_so() {
    let app = cloud().await;
    let (_, cookie) = app.login("ana@example.com").await;
    let res = app.get("/settings/api-keys", Some(&cookie)).await;
    assert!(res.body.contains("No keys yet"));
}

#[tokio::test]
async fn creating_a_key_shows_it_once_and_stores_only_its_hash() {
    let app = cloud().await;
    let (account, cookie) = app.login("ana@example.com").await;

    let res = app
        .post("/settings/api-keys", "name=Claude+Code", Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let key = shown_key(&res);
    assert!(keys::is_well_formed(&key), "{key}");
    assert_eq!(res.header("cache-control"), Some("no-store"));
    assert_eq!(res.header("hx-replace-url"), Some("/settings/api-keys"));
    assert!(
        res.body.contains(r#"hx-history="false""#),
        "htmx must not keep the key in its history snapshot: {}",
        res.body
    );
    assert!(res.body.contains("shown once"));
    assert!(res.body.contains("Claude Code"));
    // A ready command with the key in it.
    assert!(
        res.body.contains(&format!("Bearer {key}"))
            && res.body.contains("codoseo https://codoseo.com/mcp"),
        "a ready command with the key in it: {}",
        res.body
    );

    // The database holds the hash and the prefix and nothing that contains the key.
    let (hash, prefix): (Vec<u8>, String) =
        sqlx::query_as("SELECT key_hash, prefix FROM api_keys WHERE account_id = $1")
            .bind(account.id)
            .fetch_one(app.pool())
            .await
            .unwrap();
    assert_eq!(hash, keys::hash_key(&key));
    assert_eq!(prefix, key[..12]);
    let rows: Vec<String> = sqlx::query_scalar("SELECT row_to_json(k)::text FROM api_keys k")
        .fetch_all(app.pool())
        .await
        .unwrap();
    assert!(rows.iter().all(|row| !row.contains(&key)), "{rows:?}");
    let (id, who) = api_keys::authenticate(app.pool(), &keys::hash_key(&key))
        .await
        .unwrap()
        .expect("the key works");
    assert_eq!(who.id, account.id);
    assert_eq!(
        api_keys::list_for_account(app.pool(), account.id)
            .await
            .unwrap()[0]
            .id,
        id
    );

    // Never again: the list shows the prefix only.
    let later = app.get("/settings/api-keys", Some(&cookie)).await;
    assert!(!later.body.contains(&key));
    assert!(later.body.contains(&key[..12]));
    assert!(!later.body.contains("New key:"));
}

#[tokio::test]
async fn every_key_is_different() {
    let app = cloud().await;
    let (_, cookie) = app.login("ana@example.com").await;
    let one = app
        .post("/settings/api-keys", "name=one", Some(&cookie))
        .await;
    let two = app
        .post("/settings/api-keys", "name=two", Some(&cookie))
        .await;
    assert_ne!(shown_key(&one), shown_key(&two));
}

#[tokio::test]
async fn a_name_of_one_to_sixty_characters_is_required() {
    let app = cloud().await;
    let (account, cookie) = app.login("ana@example.com").await;
    for body in ["name=", "name=+++", "", &format!("name={}", "a".repeat(61))] {
        let res = app.post("/settings/api-keys", body, Some(&cookie)).await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{body}");
        let res = app.post_hx("/settings/api-keys", body, Some(&cookie)).await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(res.header("hx-retarget"), Some("#add-key"));
        assert!(res.body.contains("1 to 60 characters"), "{}", res.body);
    }
    assert!(
        api_keys::list_for_account(app.pool(), account.id)
            .await
            .unwrap()
            .is_empty()
    );
    let ok = app
        .post(
            "/settings/api-keys",
            &format!("name={}", "a".repeat(60)),
            Some(&cookie),
        )
        .await;
    assert_eq!(ok.status, StatusCode::OK);
}

#[tokio::test]
async fn a_name_is_escaped() {
    let app = cloud().await;
    let (_, cookie) = app.login("ana@example.com").await;
    let res = app
        .post(
            "/settings/api-keys",
            "name=%3Cscript%3Ealert(1)%3C%2Fscript%3E",
            Some(&cookie),
        )
        .await;
    assert!(!res.body.contains("<script>alert"));
}

#[tokio::test]
async fn the_twenty_first_key_is_refused_with_a_message() {
    let app = cloud().await;
    let (account, cookie) = app.login("ana@example.com").await;
    for n in 0..20 {
        make_key(&app, account.id, &format!("key {n}")).await;
    }
    let res = app
        .post("/settings/api-keys", "name=one+more", Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert!(res.body.contains("up to 20 API keys"), "{}", res.body);
    assert!(!res.body.contains("cdo_"), "no key was made");

    let res = app
        .post_hx("/settings/api-keys", "name=one+more", Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert_eq!(res.header("hx-retarget"), Some("#add-key"));
    assert!(res.body.contains("up to 20 API keys"));
    assert_eq!(
        api_keys::list_for_account(app.pool(), account.id)
            .await
            .unwrap()
            .len(),
        20
    );
}

#[tokio::test]
async fn revoking_a_key_ends_it() {
    let app = cloud().await;
    let (account, cookie) = app.login("ana@example.com").await;
    let created = app
        .post("/settings/api-keys", "name=CI", Some(&cookie))
        .await;
    let key = shown_key(&created);
    let id = api_keys::list_for_account(app.pool(), account.id)
        .await
        .unwrap()[0]
        .id;

    let res = app
        .post(
            &format!("/settings/api-keys/{id}/revoke"),
            "",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.location(), Some("/settings/api-keys"));
    assert!(
        api_keys::authenticate(app.pool(), &keys::hash_key(&key))
            .await
            .unwrap()
            .is_none()
    );
    let page = app.get("/settings/api-keys", Some(&cookie)).await;
    assert!(page.body.contains("No keys yet"));

    // Revoking again: gone, so 404.
    let again = app
        .post(
            &format!("/settings/api-keys/{id}/revoke"),
            "",
            Some(&cookie),
        )
        .await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn another_accounts_key_is_a_404_and_stays_live() {
    let app = cloud().await;
    let (mine, my_cookie) = app.login("me@example.com").await;
    let (theirs, _) = app.login("them@example.com").await;
    let their_key = make_key(&app, theirs.id, "Theirs").await;
    let _ = mine;

    for id in [their_key, Uuid::new_v4()] {
        let res = app
            .post(
                &format!("/settings/api-keys/{id}/revoke"),
                "",
                Some(&my_cookie),
            )
            .await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{id}");
    }
    assert_eq!(
        api_keys::list_for_account(app.pool(), theirs.id)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn a_cross_origin_post_is_rejected() {
    let app = cloud().await;
    let (account, cookie) = app.login("ana@example.com").await;
    let id = make_key(&app, account.id, "CI").await;
    for (path, body) in [
        ("/settings/api-keys".to_owned(), "name=evil".to_owned()),
        (format!("/settings/api-keys/{id}/revoke"), String::new()),
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
    let keys = api_keys::list_for_account(app.pool(), account.id)
        .await
        .unwrap();
    assert_eq!(keys.len(), 1, "nothing created, nothing revoked");
}

#[tokio::test]
async fn the_usage_line_follows_the_plan() {
    let app = cloud().await;
    for (plan, line) in [
        (Plan::Free, "37 of 100 API calls used today"),
        (Plan::Pro, "37 of 2,000 API calls used today"),
        (Plan::Agency, "37 of 10,000 API calls used today"),
    ] {
        let (account, cookie) = app
            .login_with_plan(&format!("{plan:?}@example.com"), Some(plan))
            .await;
        for _ in 0..37 {
            api_keys::charge(app.pool(), account.id, None)
                .await
                .unwrap();
        }
        let res = app.get("/settings/api-keys", Some(&cookie)).await;
        assert!(res.body.contains(line), "{plan:?}: {}", res.body);
    }

    let own = TestApp::new().await;
    let (account, cookie) = own.login("owner@example.com").await;
    let res = own.get("/settings/api-keys", Some(&cookie)).await;
    assert!(res.body.contains("Unlimited API calls, 0 used today"));
    api_keys::charge(own.pool(), account.id, None)
        .await
        .unwrap();
    let res = own.get("/settings/api-keys", Some(&cookie)).await;
    assert!(res.body.contains("Unlimited API calls, 1 used today"));
}
