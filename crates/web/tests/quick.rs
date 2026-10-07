//! T6.1: the cloud landing page and the no-signup audit flow, end to end against real Postgres
//! and a fake mailer: submit a URL, watch the report page through its states, unlock it with an
//! email, and land in an account with the site attached.

mod support;

use axum::http::StatusCode;
use codoseo_core::output::StopReason;
use codoseo_core::page::Indexability;
use codoseo_core::plan::Plan;
use sha2::{Digest, Sha256};
use support::{TestApp, TestResponse, cloud_config, cloud_config_with, page};
use uuid::Uuid;

const CLAIM_COOKIE: &str = "codoseo_audit";

async fn cloud() -> TestApp {
    TestApp::with_config(cloud_config()).await
}

async fn submit(app: &TestApp, url: &str) -> TestResponse {
    app.post("/audit", &format!("url={}", encode(url)), None)
        .await
}

fn encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// The crawl id out of the redirect to `/audit/{id}`.
fn crawl_id(res: &TestResponse) -> Uuid {
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let location = res.location().expect("redirect");
    let id = location
        .strip_prefix("/audit/")
        .unwrap_or_else(|| panic!("{location}"));
    Uuid::parse_str(id).expect("crawl id")
}

fn claim_token(res: &TestResponse) -> String {
    res.cookie(CLAIM_COOKIE)
        .expect("claim cookie")
        .split_once('=')
        .unwrap()
        .1
        .to_owned()
}

async fn count(app: &TestApp, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(app.pool()).await.unwrap()
}

/// A site with a spread of defects, so the audit has more than five failing checks.
fn messy_pages(domain: &str) -> Vec<codoseo_core::page::PageRecord> {
    let mut pages = vec![page(domain, "/")];
    for i in 0..4 {
        let mut p = page(domain, &format!("/messy-{i}"));
        p.fields.title = None;
        p.fields.title_count = 0;
        p.fields.meta_description = None;
        p.fields.h1.clear();
        p.fields.word_count = 20;
        p.fields.images_missing_alt = 3;
        pages.push(p);
    }
    let mut gone = page(domain, "/gone");
    gone.status = 404;
    gone.indexability = Indexability::ClientError;
    pages.push(gone);
    let mut broken = page(domain, "/broken");
    broken.status = 500;
    broken.indexability = Indexability::ServerError;
    pages.push(broken);
    let mut hidden = page(domain, "/hidden-secret-path");
    hidden.indexability = Indexability::Noindex;
    hidden.fields.meta_robots = Some("noindex".to_owned());
    pages.push(hidden);
    pages
}

async fn finish(app: &TestApp, crawl: Uuid, pages: Vec<codoseo_core::page::PageRecord>) {
    app.finalize_crawl(crawl, pages, Vec::new(), StopReason::Completed)
        .await;
}

#[tokio::test]
async fn the_cloud_landing_shows_the_url_box_and_self_hosted_is_unchanged() {
    let app = cloud().await;
    let res = app.get("/", None).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains(r#"action="/audit""#), "{}", res.body);
    assert!(res.body.contains(r#"name="url""#));
    assert!(
        res.body
            .contains(r#"rel="canonical" href="https://codoseo.com/""#)
    );
    assert!(res.body.contains("application/ld+json"));
    for needle in [
        r#"<meta name="robots" content="index, follow, max-image-preview:large">"#,
        r#"<meta property="og:image" content="https://codoseo.com/og.png">"#,
        r#"<meta property="og:image:width" content="1200">"#,
        r#"<meta property="og:image:height" content="630">"#,
        r#"<meta property="og:locale" content="en_US">"#,
        r#"<meta name="twitter:card" content="summary_large_image">"#,
        r#"<meta name="twitter:image" content="https://codoseo.com/og.png">"#,
        r#""@type":"SoftwareApplication""#,
        r#""logo":"https://codoseo.com/assets/icon-512."#,
        // The hero mascot that watches the cursor.
        "data-watch",
    ] {
        assert!(res.body.contains(needle), "{needle}: {}", res.body);
    }
    assert!(
        !res.body.contains(r#"content="noindex"#),
        "the landing page is for search engines"
    );
    assert!(
        !res.body.contains("challenges.cloudflare.com"),
        "no Turnstile script without keys"
    );

    // Signed in: straight to the app, as before.
    let (_, cookie) = app.login("ana@example.com").await;
    let res = app.get("/", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.location(), Some("/sites"));

    // Self-hosted: signed-out visitors still go to login, and the public routes don't exist.
    let selfhost = TestApp::new().await;
    let res = selfhost.get("/", None).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert!(res.location().unwrap().starts_with("/login"));
    for path in [
        "/bot",
        "/robots.txt",
        "/llms.txt",
        "/audit/00000000-0000-0000-0000-000000000000",
    ] {
        assert_eq!(
            selfhost.get(path, None).await.status,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    let res = selfhost.post("/audit", "url=example.com", None).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(count(&selfhost, "SELECT count(*) FROM crawls").await, 0);
}

#[tokio::test]
async fn submitting_a_url_starts_an_audit_and_sets_the_claim_cookie() {
    let app = cloud().await;
    let res = submit(&app, "Example.com/blog").await;
    let crawl = crawl_id(&res);
    let set_cookie = res
        .headers
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap().to_owned())
        .find(|v| v.starts_with(&format!("{CLAIM_COOKIE}=")))
        .expect("claim cookie");
    assert!(set_cookie.contains("HttpOnly") && set_cookie.contains("SameSite=Lax"));
    assert!(
        set_cookie.contains("Secure"),
        "https base url: {set_cookie}"
    );
    assert!(set_cookie.contains("Max-Age=604800"), "{set_cookie}");

    let (domain, start_url, account, claim, trigger, priority): (
        String,
        String,
        Option<Uuid>,
        Option<Vec<u8>>,
        String,
        i16,
    ) = sqlx::query_as(
        "SELECT s.domain, s.start_url, s.account_id, s.claim_token_hash, c.trigger::text, c.priority \
         FROM crawls c JOIN sites s ON s.id = c.site_id WHERE c.id = $1",
    )
    .bind(crawl)
    .fetch_one(app.pool())
    .await
    .unwrap();
    assert_eq!(domain, "example.com");
    assert_eq!(start_url, "https://example.com/blog");
    assert_eq!(account, None);
    assert_eq!((trigger.as_str(), priority), ("quick", 0));
    assert_eq!(
        claim.unwrap(),
        Sha256::digest(claim_token(&res).as_bytes()).to_vec(),
        "only the hash of the cookie's token is stored"
    );
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM events WHERE kind = 'audit_started'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn bad_addresses_are_refused_with_a_message_and_start_nothing() {
    let app = cloud().await;
    for (input, message) in [
        ("", "Enter your site"),
        ("ftp://example.com", "Only http and https"),
        ("localhost", "private or internal"),
        ("http://10.0.0.1/", "private or internal"),
        ("http://169.254.169.254/", "private or internal"),
    ] {
        let res = submit(&app, input).await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{input:?}");
        assert!(res.body.contains(message), "{input:?}: {}", res.body);
        assert!(
            res.body.contains(r#"action="/audit""#),
            "the form is still there"
        );
    }
    assert_eq!(count(&app, "SELECT count(*) FROM crawls").await, 0);
    assert_eq!(count(&app, "SELECT count(*) FROM sites").await, 0);
}

#[tokio::test]
async fn the_same_site_in_any_spelling_shares_one_audit() {
    let app = cloud().await;
    let first = submit(&app, "example.com").await;
    let crawl = crawl_id(&first);
    assert!(first.cookie(CLAIM_COOKIE).is_some());

    for variant in [
        "https://EXAMPLE.com/",
        "http://example.com/about#team",
        " example.com ",
    ] {
        let again = submit(&app, variant).await;
        assert_eq!(crawl_id(&again), crawl, "{variant}");
        assert!(
            again.cookie(CLAIM_COOKIE).is_none(),
            "a visitor who joins an audit doesn't get its claim cookie"
        );
    }
    assert_eq!(count(&app, "SELECT count(*) FROM crawls").await, 1);

    // Once it is done, the cached report is shown to the next visitor too.
    finish(&app, crawl, vec![page("example.com", "/")]).await;
    assert_eq!(crawl_id(&submit(&app, "example.com").await), crawl);
    assert_eq!(count(&app, "SELECT count(*) FROM crawls").await, 1);
}

#[tokio::test]
async fn the_report_page_walks_through_waiting_running_and_done() {
    let app = cloud().await;
    let crawl = crawl_id(&submit(&app, "example.com").await);
    let path = format!("/audit/{crawl}");

    // Waiting for a crawler: polls itself, no score.
    let res = app.get(&path, None).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(
        res.body.contains(&format!(r#"hx-get="{path}/live""#)),
        "{}",
        res.body
    );
    assert!(res.body.contains("every 2s"));
    assert!(!res.body.contains(r#"class="ring""#));
    assert!(
        res.body.contains("noindex"),
        "the report is not for search engines"
    );
    assert_eq!(res.header("cache-control"), Some("no-store"));

    // Running: the live counter.
    sqlx::query(
        "UPDATE crawls SET status = 'running', started_at = now(), \
         progress = '{\"pages_done\":37,\"queued\":12,\"failures\":0,\"depth\":2,\"elapsed_ms\":9000}' \
         WHERE id = $1",
    )
    .bind(crawl)
    .execute(app.pool())
    .await
    .unwrap();
    let res = app.get_hx(&format!("{path}/live"), None).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("37"), "{}", res.body);
    assert!(res.body.contains("every 2s"));

    // Done: the score and the top issues; the polling stops.
    app.finalize_crawl(
        crawl,
        messy_pages("example.com"),
        Vec::new(),
        StopReason::Completed,
    )
    .await;
    let res = app.get_hx(&format!("{path}/live"), None).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains(r#"class="ring""#), "{}", res.body);
    assert!(res.body.contains("checks passed"));
    assert!(
        !res.body.contains("every 2s"),
        "a finished audit stops polling"
    );
    assert!(res.body.contains(&format!(r#"action="{path}/unlock""#)));
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM events WHERE kind = 'audit_finished'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn the_preview_shows_five_issues_and_keeps_the_rest_and_every_url_locked() {
    let app = cloud().await;
    let crawl = crawl_id(&submit(&app, "example.com").await);
    finish(&app, crawl, messy_pages("example.com")).await;

    let failing: i32 = sqlx::query_scalar(
        "SELECT jsonb_array_length(summary->'counts') FROM crawls WHERE id = $1",
    )
    .bind(crawl)
    .fetch_one(app.pool())
    .await
    .unwrap();
    assert!(
        failing > 5,
        "the fixture should fail more than five checks, got {failing}"
    );

    let body = app.get(&format!("/audit/{crawl}"), None).await.body;
    assert_eq!(body.matches(r#"class="issue-row"#).count(), 5, "{body}");
    let locked = failing - 5;
    assert!(
        body.contains(&format!("{locked} more issue")),
        "the locked remainder is counted ({locked}): {body}"
    );
    // The lead magnet: no page URLs before the email.
    assert!(!body.contains("hidden-secret-path"));
    assert!(!body.contains("/messy-0"));
}

#[tokio::test]
async fn a_site_that_cannot_be_audited_says_why_instead_of_scoring() {
    let app = cloud().await;
    let cases = [
        (
            "blocked.example",
            "site blocked our crawler: 403 on every request",
            "blocked",
        ),
        ("down.example", "site unreachable: dns error", "reach"),
        ("bug.example", "internal error", "went wrong"),
    ];
    for (domain, reason, expect) in cases {
        let crawl = crawl_id(&submit(&app, domain).await);
        sqlx::query(
            "UPDATE crawls SET status = 'failed', failure_reason = $2, finished_at = now() WHERE id = $1",
        )
        .bind(crawl)
        .bind(reason)
        .execute(app.pool())
        .await
        .unwrap();
        let res = app.get(&format!("/audit/{crawl}"), None).await;
        assert_eq!(res.status, StatusCode::OK, "{domain}");
        assert!(res.body.contains(expect), "{domain}: {}", res.body);
        assert!(
            !res.body.contains(r#"class="ring""#),
            "{domain}: no score for a failed audit"
        );
        assert!(
            !res.body.contains("every 2s"),
            "{domain}: no endless spinner"
        );
        assert!(
            res.body.contains(r#"href="/""#),
            "{domain}: a way to try another site"
        );
    }

    // robots.txt forbids crawling: the crawl finishes with no pages.
    let crawl = crawl_id(&submit(&app, "private.example").await);
    app.finalize_crawl(crawl, Vec::new(), Vec::new(), StopReason::RobotsBlocked)
        .await;
    let res = app.get(&format!("/audit/{crawl}"), None).await;
    assert!(res.body.contains("robots.txt"), "{}", res.body);
    assert!(!res.body.contains(r#"class="ring""#));
}

#[tokio::test]
async fn a_crawl_stopped_at_the_page_limit_says_so() {
    let app = cloud().await;
    let crawl = crawl_id(&submit(&app, "big.example").await);
    app.finalize_crawl(
        crawl,
        vec![page("big.example", "/")],
        Vec::new(),
        StopReason::PageLimit,
    )
    .await;
    let body = app.get(&format!("/audit/{crawl}"), None).await.body;
    assert!(body.contains("100 pages"), "{body}");
}

#[tokio::test]
async fn report_urls_that_should_not_resolve_are_all_the_same_404() {
    let app = cloud().await;
    let crawl = crawl_id(&submit(&app, "example.com").await);

    // A signed-in user's ordinary crawl id.
    let (ana, _) = app.login("ana@example.com").await;
    let site = app.site(&ana, "mine.example").await;
    let mine = app
        .finished_crawl(&site, vec![page("mine.example", "/")], Vec::new())
        .await;

    // A report past its seven days (the retention job deletes the unclaimed site).
    sqlx::query("DELETE FROM sites WHERE id = (SELECT site_id FROM crawls WHERE id = $1)")
        .bind(crawl)
        .execute(app.pool())
        .await
        .unwrap();

    for path in [
        format!("/audit/{crawl}"),
        format!("/audit/{mine}"),
        format!("/audit/{}", Uuid::new_v4()),
        "/audit/not-a-uuid".to_owned(),
        format!("/audit/{mine}/live"),
    ] {
        let res = app.get(&path, None).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{path}");
    }
    let res = app
        .post(
            &format!("/audit/{mine}/unlock"),
            "email=ana%40example.com",
            None,
        )
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}

/// Pulls the sign-in token out of the last captured email.
fn emailed_token(app: &TestApp) -> String {
    let mail = app.mail.lock().unwrap();
    let text = &mail.last().expect("an email was sent").text;
    let start = text.find("/auth/magic/").expect("a magic link") + "/auth/magic/".len();
    text[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
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
async fn unlocking_emails_a_link_that_attaches_the_audit_and_queues_the_first_crawl() {
    let app = cloud().await;
    let started = submit(&app, "example.com").await;
    let crawl = crawl_id(&started);
    let token = claim_token(&started);
    finish(&app, crawl, messy_pages("example.com")).await;

    let res = unlock(&app, crawl, "Ana@Example.com").await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("Check your email"), "{}", res.body);
    {
        let mail = app.mail.lock().unwrap();
        assert_eq!(mail.len(), 1);
        assert_eq!(mail[0].to, "Ana@Example.com");
    }
    assert_eq!(
        count(&app, "SELECT count(*) FROM accounts").await,
        0,
        "no account until the click"
    );

    // The link opens a confirm page, then the POST signs in and attaches the site.
    let link = emailed_token(&app);
    assert_eq!(
        app.get(&format!("/auth/magic/{link}"), None).await.status,
        StatusCode::OK
    );
    let res = app
        .post(
            &format!("/auth/magic/{link}"),
            "",
            Some(&format!("{CLAIM_COOKIE}={token}")),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert!(res.cookie("codoseo_session").is_some());
    let cleared = res.headers.get_all("set-cookie").iter().any(|v| {
        v.to_str()
            .unwrap()
            .starts_with(&format!("{CLAIM_COOKIE}=;"))
            && v.to_str().unwrap().contains("Max-Age=0")
    });
    assert!(cleared, "the spent claim cookie is cleared");

    let (account, plan): (Uuid, String) = sqlx::query_as("SELECT id, plan::text FROM accounts")
        .fetch_one(app.pool())
        .await
        .unwrap();
    assert_eq!(plan, "free");
    let (site, owner, claim): (Uuid, Option<Uuid>, Option<Vec<u8>>) = sqlx::query_as(
        "SELECT s.id, s.account_id, s.claim_token_hash FROM crawls c JOIN sites s ON s.id = c.site_id WHERE c.id = $1",
    )
    .bind(crawl)
    .fetch_one(app.pool())
    .await
    .unwrap();
    assert_eq!(
        (owner, claim),
        (Some(account), None),
        "the audited site now belongs to the account"
    );
    assert_eq!(res.location(), Some(format!("/s/{site}/audit").as_str()));
    assert_eq!(
        count(
            &app,
            &format!("SELECT count(*) FROM alert_rules WHERE site_id = '{site}'")
        )
        .await,
        5,
        "the default instant rules are on for the attached site"
    );

    let (priority, status): (i16, String) = sqlx::query_as(
        "SELECT priority, status::text FROM crawls WHERE site_id = $1 AND trigger = 'first'",
    )
    .bind(site)
    .fetch_one(app.pool())
    .await
    .unwrap();
    assert_eq!((priority, status.as_str()), (1, "queued"));
    for kind in ["email_given", "link_clicked"] {
        assert_eq!(
            count(
                &app,
                &format!("SELECT count(*) FROM events WHERE kind = '{kind}'")
            )
            .await,
            1,
            "{kind}"
        );
    }

    // The link works once.
    let again = app.post(&format!("/auth/magic/{link}"), "", None).await;
    assert_eq!(again.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn opening_the_link_in_another_browser_still_gives_the_account_the_site() {
    let app = cloud().await;
    let crawl = crawl_id(&submit(&app, "example.com").await);
    finish(&app, crawl, vec![page("example.com", "/")]).await;
    unlock(&app, crawl, "ana@example.com").await;
    let link = emailed_token(&app);

    // No claim cookie on this device.
    let res = app.post(&format!("/auth/magic/{link}"), "", None).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);

    let (sites, original_owner): (i64, Option<Uuid>) = (
        count(
            &app,
            "SELECT count(*) FROM sites WHERE account_id IS NOT NULL",
        )
        .await,
        sqlx::query_scalar(
            "SELECT s.account_id FROM crawls c JOIN sites s ON s.id = c.site_id WHERE c.id = $1",
        )
        .bind(crawl)
        .fetch_one(app.pool())
        .await
        .unwrap(),
    );
    assert_eq!(sites, 1, "a fresh site was created for the account");
    assert_eq!(
        original_owner, None,
        "the visitor's audit stays unclaimed until it expires"
    );
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM crawls WHERE trigger = 'first' AND priority = 1"
        )
        .await,
        1
    );
    assert_eq!(location_site_domain(&app, &res).await, "example.com");
}

async fn location_site_domain(app: &TestApp, res: &TestResponse) -> String {
    let id = res
        .location()
        .and_then(|l| l.strip_prefix("/s/"))
        .and_then(|l| l.strip_suffix("/audit"))
        .expect("redirect to a site audit");
    sqlx::query_scalar("SELECT domain FROM sites WHERE id = $1::uuid")
        .bind(id)
        .fetch_one(app.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn an_email_variant_of_an_existing_account_signs_in_instead_of_creating_another() {
    let app = cloud().await;
    let (existing, _) = app.login("ana@gmail.com").await;
    let crawl = crawl_id(&submit(&app, "example.com").await);
    unlock(&app, crawl, "a.na+seo@gmail.com").await;
    let link = emailed_token(&app);
    let res = app.post(&format!("/auth/magic/{link}"), "", None).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(count(&app, "SELECT count(*) FROM accounts").await, 1);
    let owner: Uuid = sqlx::query_scalar(
        "SELECT account_id FROM sites WHERE domain = 'example.com' AND account_id IS NOT NULL",
    )
    .fetch_one(app.pool())
    .await
    .unwrap();
    assert_eq!(owner, existing.id);
}

#[tokio::test]
async fn an_expired_unlock_link_explains_itself() {
    let app = cloud().await;
    let crawl = crawl_id(&submit(&app, "example.com").await);
    unlock(&app, crawl, "ana@example.com").await;
    let link = emailed_token(&app);
    sqlx::query("UPDATE login_tokens SET expires_at = now() - interval '1 minute'")
        .execute(app.pool())
        .await
        .unwrap();
    let res = app.post(&format!("/auth/magic/{link}"), "", None).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert!(res.body.contains("expired"), "{}", res.body);
    assert_eq!(count(&app, "SELECT count(*) FROM accounts").await, 0);
}

#[tokio::test]
async fn a_free_account_that_already_has_its_site_keeps_it() {
    let app = cloud().await;
    let (ana, _) = app
        .login_with_plan("ana@example.com", Some(Plan::Free))
        .await;
    let other = app.site(&ana, "other.example").await;
    let crawl = crawl_id(&submit(&app, "example.com").await);
    unlock(&app, crawl, "ana@example.com").await;
    let link = emailed_token(&app);

    let res = app.post(&format!("/auth/magic/{link}"), "", None).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(
        res.location(),
        Some(format!("/s/{}/audit", other.id).as_str())
    );
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM sites WHERE account_id IS NOT NULL"
        )
        .await,
        1
    );
    assert_eq!(
        count(&app, "SELECT count(*) FROM crawls WHERE trigger = 'first'").await,
        0
    );
}

#[tokio::test]
async fn unlocking_rejects_a_bad_email_and_an_ordinary_login_link_has_no_audit() {
    let app = cloud().await;
    let crawl = crawl_id(&submit(&app, "example.com").await);
    let res = unlock(&app, crawl, "not-an-email").await;
    assert!(res.body.contains("email address"), "{}", res.body);
    assert!(app.mail.lock().unwrap().is_empty());

    // The normal login flow is untouched: no audit payload, no site created.
    app.post_hx("/login", "email=bo%40example.com&next=%2F", None)
        .await;
    let link = emailed_token(&app);
    let res = app.post(&format!("/auth/magic/{link}"), "", None).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM sites WHERE account_id IS NOT NULL"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn the_bot_page_robots_and_llms_txt_are_served_in_the_cloud() {
    let app = TestApp::with_config(cloud_config_with(&[("CODOSEO_BOT_IP", "203.0.113.7")])).await;
    let res = app.get("/bot", None).await;
    assert_eq!(res.status, StatusCode::OK);
    for needle in [
        "CodoSEObot/0.1",
        "robots.txt",
        "203.0.113.7",
        "5 requests per second",
        "User-agent: CodoSEObot",
        r#"<link rel="canonical" href="https://codoseo.com/bot">"#,
        r#"<meta name="robots" content="index, follow, max-image-preview:large">"#,
        r#"<meta property="og:url" content="https://codoseo.com/bot">"#,
        r#"<meta name="twitter:card" content="summary_large_image">"#,
    ] {
        assert!(res.body.contains(needle), "{needle}: {}", res.body);
    }

    let res = app.get("/robots.txt", None).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(
        res.header("content-type")
            .unwrap()
            .starts_with("text/plain")
    );
    for rule in [
        "Disallow: /audit/",
        "Disallow: /s/",
        "Disallow: /login",
        "Disallow: /auth/",
        "Disallow: /admin",
        "Disallow: /go/",
    ] {
        assert!(res.body.contains(rule), "{rule}: {}", res.body);
    }
    assert!(!res.body.contains("Disallow: /bot"));
    // A bot with its own group ignores `*`, so the private paths are in every group.
    for agent in ["GPTBot", "ClaudeBot", "Googlebot"] {
        let group = res
            .body
            .split("\n\n")
            .find(|g| g.starts_with(&format!("User-agent: {agent}")))
            .unwrap_or_else(|| panic!("{agent}"));
        assert!(
            group.contains("Disallow: /audit/") && group.contains("Disallow: /s/"),
            "{agent}: {group}"
        );
    }
    assert!(
        res.body
            .contains("Sitemap: https://codoseo.com/sitemap.xml")
    );

    let res = app.get("/llms.txt", None).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("CodoSEO"));
}

#[tokio::test]
async fn one_visitor_leaves_every_funnel_step_in_order() {
    let app = TestApp::with_config(cloud_config_with(&[(
        "RANKORG_URL",
        "https://rankorg.example/",
    )]))
    .await;
    let started = submit(&app, "example.com").await;
    let crawl = crawl_id(&started);
    finish(&app, crawl, messy_pages("example.com")).await;
    unlock(&app, crawl, "ana@example.com").await;
    let link = emailed_token(&app);
    let res = app
        .post(
            &format!("/auth/magic/{link}"),
            "",
            Some(&format!("{CLAIM_COOKIE}={}", claim_token(&started))),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);

    // The full 500-page crawl finishes, and the visitor follows the RankOrg link.
    let first: Uuid = sqlx::query_scalar("SELECT id FROM crawls WHERE trigger = 'first'")
        .fetch_one(app.pool())
        .await
        .unwrap();
    app.finalize_crawl(
        first,
        messy_pages("example.com"),
        Vec::new(),
        StopReason::Completed,
    )
    .await;
    let go = app
        .get(&format!("/go/rankorg?src=audit&audit={crawl}"), None)
        .await;
    assert_eq!(go.status, StatusCode::SEE_OTHER);

    let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM events ORDER BY id")
        .fetch_all(app.pool())
        .await
        .unwrap();
    assert_eq!(
        kinds,
        [
            "audit_started",
            "audit_finished",
            "email_given",
            "link_clicked",
            "first_full_crawl",
            "rankorg_click"
        ]
    );
}

#[tokio::test]
async fn auditing_a_second_site_does_not_lose_the_first_ones_claim() {
    let app = cloud().await;
    let a = submit(&app, "a.com").await;
    let crawl_a = crawl_id(&a);
    let token_a = claim_token(&a);
    // The same browser audits another site: it sends its cookie and gets one holding both.
    let b = app
        .post(
            "/audit",
            "url=b.com",
            Some(&format!("{CLAIM_COOKIE}={token_a}")),
        )
        .await;
    let both = claim_token(&b);
    assert!(both.contains(&token_a) && both.contains('.'), "{both}");

    finish(&app, crawl_a, vec![page("a.com", "/")]).await;
    unlock(&app, crawl_a, "ana@example.com").await;
    let link = emailed_token(&app);
    let res = app
        .post(
            &format!("/auth/magic/{link}"),
            "",
            Some(&format!("{CLAIM_COOKIE}={both}")),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    let owner: Option<Uuid> = sqlx::query_scalar(
        "SELECT s.account_id FROM crawls c JOIN sites s ON s.id = c.site_id WHERE c.id = $1",
    )
    .bind(crawl_a)
    .fetch_one(app.pool())
    .await
    .unwrap();
    assert!(
        owner.is_some(),
        "the audited site itself was attached, not a duplicate"
    );
    assert_eq!(
        count(&app, "SELECT count(*) FROM sites WHERE domain = 'a.com'").await,
        1
    );
}

#[tokio::test]
async fn the_cookie_keeps_only_the_last_five_tokens() {
    let app = cloud().await;
    let mut cookie = String::new();
    for i in 0..7 {
        let res = app
            .post(
                "/audit",
                &format!("url=site{i}.com"),
                if cookie.is_empty() {
                    None
                } else {
                    Some(cookie.as_str())
                },
            )
            .await;
        cookie = format!("{CLAIM_COOKIE}={}", claim_token(&res));
    }
    let tokens = cookie.split_once('=').unwrap().1.split('.').count();
    assert_eq!(tokens, 5);
}

#[tokio::test]
async fn the_report_names_the_start_page_when_it_is_not_the_homepage() {
    let app = cloud().await;
    let shop = crawl_id(&submit(&app, "example.com/shop/").await);
    let body = app.get(&format!("/audit/{shop}"), None).await.body;
    assert!(body.contains("Started from"), "{body}");
    assert!(body.contains("https://example.com/shop/"));

    let root = crawl_id(&submit(&app, "other.com").await);
    let body = app.get(&format!("/audit/{root}"), None).await.body;
    assert!(!body.contains("Started from"), "{body}");
}

#[tokio::test]
async fn the_cloud_serves_the_sitemap_robots_txt_points_to() {
    let app = cloud().await;
    let res = app.get("/sitemap.xml", None).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(
        res.header("content-type")
            .unwrap()
            .starts_with("application/xml")
    );
    assert!(
        res.body.contains("<loc>https://codoseo.com/</loc>"),
        "{}",
        res.body
    );
    assert!(res.body.contains("<loc>https://codoseo.com/bot</loc>"));
    assert!(!res.body.contains("/audit/"), "private pages stay out");
    assert_eq!(
        TestApp::new().await.get("/sitemap.xml", None).await.status,
        StatusCode::NOT_FOUND
    );
}
