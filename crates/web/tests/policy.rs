//! The site policy screen: both tabs, the htmx preview, saving (history and the quiet
//! re-evaluation), import and export of `codoseo.toml`, the history page, the sidebar item and
//! the old intent address.

mod support;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use codoseo_core::page::PageRecord;
use codoseo_geo::{Purpose, Stance};
use codoseo_policy::SitePolicy;
use codoseo_store::sites::Site;
use codoseo_store::{geo, policy};
use support::{TestApp, TestResponse, page};

const DOMAIN: &str = "example.com";
const OPEN: &str = "User-agent: *\nAllow: /\n";
const BLOCK_OAI: &str = "User-agent: *\nAllow: /\n\nUser-agent: OAI-SearchBot\nDisallow: /\n";

fn pages() -> Vec<PageRecord> {
    vec![page(DOMAIN, "/"), page(DOMAIN, "/pricing")]
}

async fn setup() -> (TestApp, Site, String) {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("ana@example.com").await;
    let site = app.site(&account, DOMAIN).await;
    (app, site, cookie)
}

fn toast_message(res: &TestResponse) -> String {
    let raw = res.header("hx-trigger").expect("a toast");
    let v: serde_json::Value = serde_json::from_str(raw).expect("json trigger");
    v["toast"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

async fn upload(app: &TestApp, site: &Site, cookie: &str, file: &str) -> TestResponse {
    let boundary = "XpolicyX";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"codoseo.toml\"\r\nContent-Type: application/toml\r\n\r\n{file}\r\n--{boundary}--\r\n"
    );
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("/s/{}/policy/import", site.id))
        .header(header::COOKIE, cookie)
        .header("hx-request", "true")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();
    app.send(req).await
}

#[tokio::test]
async fn the_ai_tab_is_the_default_and_compares_with_the_latest_crawl() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_robots(&site, pages(), (200, BLOCK_OAI), Vec::new())
        .await;
    let res = app
        .get(&format!("/s/{}/policy", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let body = &res.body;
    for needle in [
        "<h1>Site policy</h1>",
        r#"aria-current="page">AI rules"#,
        "Search crawlers",
        "Training crawlers",
        r#"name="p.search" value="allow" checked"#,
        r#"name="p.training" value="any" checked"#,
        r#"name="b.GPTBot""#,
        r#"id="bot-OAI-SearchBot""#,
        "Same as group",
        "Blocked now",
        "OAI-SearchBot is blocked by robots.txt",
        "Answer limits you use on purpose",
        "Run fetch checks",
        "Your rules vs crawl #1",
        "robots.txt that matches",
        "Copy as prompt",
        "Copy file",
        "Content-Signal: search=yes, ai-input=yes, ai-train=yes",
        "Save policy",
        "Reset to defaults",
        "Import a codoseo.toml",
        // The htmx preview on every change.
        r#"hx-post="/s/"#,
        r#"/policy/preview?tab=ai" hx-trigger="change""#,
    ] {
        assert!(body.contains(needle), "missing {needle:?}");
    }
    // The blanket block of a search crawler the policy allows goes from the suggested file.
    assert!(
        body.contains(r#"<div class="pol-rl is-del"><span aria-hidden="true">−</span><span>User-agent: OAI-SearchBot</span></div>"#),
        "{body}"
    );
    // The sidebar has the item (after Crawls: tests/sidebar.rs), and it is the current screen.
    assert!(body.contains(&format!(
        "class=\"nav-item is-active\" href=\"/s/{}/policy\"",
        site.id
    )));
}

#[tokio::test]
async fn the_seo_tab_says_what_is_coming() {
    let (app, site, cookie) = setup().await;
    let res = app
        .get(&format!("/s/{}/policy?tab=seo", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    for needle in [
        "SEO rules arrive in the next release",
        "Preferred address",
        "Pages that must always work",
        "Check thresholds",
        "Exceptions",
        r#"aria-current="page">SEO rules"#,
    ] {
        assert!(res.body.contains(needle), "missing {needle:?}");
    }
    assert!(!res.body.contains(r#"id="policy-form""#));
    assert!(!res.body.contains("robots.txt that matches"));
}

#[tokio::test]
async fn the_preview_follows_the_form_without_saving() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_robots(&site, pages(), (200, OPEN), Vec::new())
        .await;
    let res = app
        .post_hx(
            &format!("/s/{}/policy/preview?tab=ai", site.id),
            "p.search=allow&p.user_fetch=allow&p.agent=any&p.training=block&p.ads=any&probes=off&probes=on",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let body = &res.body;
    assert!(
        body.starts_with(r#"<aside class="pol-rail" id="pol-rail""#),
        "{body}"
    );
    assert!(body.contains(r#"id="cmp-training" aria-live="polite" hx-swap-oob="true""#));
    assert!(body.contains("gets in, policy says Block"), "{body}");
    assert!(body.contains("ai-train=no"));
    assert!(
        body.contains("+User-agent: GPTBot") || body.contains("<span>User-agent: GPTBot</span>")
    );
    assert!(body.contains(r#"id="tab-ai-gaps" hx-swap-oob="true""#));
    // Nothing saved.
    assert_eq!(
        policy::get(app.pool(), site.id).await.unwrap(),
        SitePolicy::default()
    );
    // A bad value is refused.
    let res = app
        .post_hx(
            &format!("/s/{}/policy/preview", site.id),
            "p.training=sometimes",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn saving_writes_the_policy_and_its_history() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_robots(&site, pages(), (200, OPEN), Vec::new())
        .await;
    let res = app
        .post_hx(
            &format!("/s/{}/policy", site.id),
            "p.search=allow&p.user_fetch=allow&p.agent=any&p.training=block&p.ads=any&b.GPTBot=inherit&d=max_snippet&probes=off",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(toast_message(&res), "Policy saved · 1 issue opened");
    assert_eq!(
        res.header("hx-replace-url"),
        Some(format!("/s/{}/policy?tab=ai", site.id).as_str())
    );
    assert!(
        res.body
            .contains(r#"name="p.training" value="block" checked"#)
    );

    let p = policy::get(app.pool(), site.id).await.unwrap();
    assert_eq!(p.ai.intent.purpose_stance(Purpose::Training), Stance::Block);
    assert!(!p.ai.probes);
    // The AI access readers see it.
    assert_eq!(
        geo::get_intent(app.pool(), site.id).await.unwrap(),
        p.ai.intent
    );
    assert!(!geo::get_ai_probes(app.pool(), site.id).await.unwrap());
    let open = geo::open_incidents(app.pool(), site.id).await.unwrap();
    assert!(open.iter().any(|i| i.quiet && !i.kind.is_advisory()));

    // History lists it, with who and what.
    let h = app
        .get(&format!("/s/{}/policy/history", site.id), Some(&cookie))
        .await;
    assert_eq!(h.status, StatusCode::OK);
    for needle in [
        "Policy history",
        "ana@example.com",
        "Policy screen",
        "ai.purposes.training: default → block",
        "ai.probes: true → false",
        "crawl #1",
    ] {
        assert!(h.body.contains(needle), "missing {needle:?}");
    }

    // A plain form post is redirected back.
    let res = app
        .post(
            &format!("/s/{}/policy", site.id),
            "p.training=block",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(
        res.location(),
        Some(format!("/s/{}/policy?tab=ai", site.id).as_str())
    );

    // Reset the AI rules: back to the defaults, the issue resolves as a choice.
    let res = app
        .post_hx(
            &format!("/s/{}/policy/reset?tab=ai", site.id),
            "",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(
        toast_message(&res),
        "AI rules reset to the defaults · 1 issue resolved"
    );
    assert_eq!(
        policy::get(app.pool(), site.id).await.unwrap(),
        SitePolicy::default()
    );
}

#[tokio::test]
async fn export_and_import_round_trip_and_bad_files_say_why() {
    let (app, site, cookie) = setup().await;
    let file = "[policy]\nversion = 1\n[policy.seo]\nmust_work = [\"/pricing\"]\n[policy.ai]\npurposes = { training = \"block\" }\nbots = { \"GPTBot\" = \"allow\" }\nfuture_key = 1\n";
    let res = upload(&app, &site, &cookie, file).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(
        toast_message(&res),
        "Policy imported · it applies from the next crawl · 1 unknown key ignored"
    );
    let p = policy::get(app.pool(), site.id).await.unwrap();
    assert_eq!(p.seo.must_work, ["/pricing"]);
    assert_eq!(p.ai.intent.bot_override("GPTBot"), Some(Stance::Allow));
    let h = policy::history(app.pool(), site.id, 5).await.unwrap();
    assert_eq!(h[0].via, "import");

    // The SEO tab says it keeps them.
    let seo = app
        .get(&format!("/s/{}/policy?tab=seo", site.id), Some(&cookie))
        .await;
    assert!(
        seo.body.contains("holds 1 SEO rule from an import"),
        "{}",
        seo.body
    );

    // Export: the same policy back, as a download.
    let res = app
        .get(&format!("/s/{}/policy/export", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(
        res.header("content-type"),
        Some("application/toml; charset=utf-8")
    );
    assert_eq!(
        res.header("content-disposition"),
        Some("attachment; filename=\"codoseo.toml\"")
    );
    let back = codoseo_policy::parse_file(&res.body).unwrap();
    assert_eq!(back.policy, p);

    // A broken file is shown inline and changes nothing.
    for (bad, why) in [
        ("[policy\n", "line 1"),
        (
            "[policy]\n[policy.ai]\naccepted_directives = [\"noindex\"]\n",
            "noindex can",
        ),
        ("[other]\nx = 1\n", "no [policy] table"),
        ("", "Choose a codoseo.toml file"),
    ] {
        let res = upload(&app, &site, &cookie, bad).await;
        assert_eq!(res.status, StatusCode::OK);
        assert!(res.body.contains("The file was not imported"), "{bad:?}");
        assert!(res.body.contains(why), "{bad:?}: {why}");
    }
    assert_eq!(policy::get(app.pool(), site.id).await.unwrap(), p);
}

#[tokio::test]
async fn the_old_intent_address_moves_and_its_form_still_saves() {
    let (app, site, cookie) = setup().await;
    let res = app
        .get(&format!("/s/{}/ai-access/intent", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(
        res.location(),
        Some(format!("/s/{}/policy?tab=ai", site.id).as_str())
    );
    let res = app
        .post_hx(
            &format!("/s/{}/ai-access/intent", site.id),
            "p.ads=block",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(
        toast_message(&res),
        "Intent saved · it applies from the next crawl"
    );
    let p = policy::get(app.pool(), site.id).await.unwrap();
    assert_eq!(p.ai.intent.purpose_stance(Purpose::Ads), Stance::Block);
    assert_eq!(
        policy::history(app.pool(), site.id, 5).await.unwrap()[0].via,
        "web"
    );
}

#[tokio::test]
async fn another_account_gets_nothing() {
    let (app, site, _) = setup().await;
    let (_, other) = app.login("bo@example.com").await;
    let base = format!("/s/{}/policy", site.id);
    for path in [
        base.clone(),
        format!("{base}/history"),
        format!("{base}/export"),
    ] {
        assert_eq!(
            app.get(&path, Some(&other)).await.status,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    for path in [
        base.clone(),
        format!("{base}/preview"),
        format!("{base}/reset"),
    ] {
        assert_eq!(
            app.post(&path, "p.training=block", Some(&other))
                .await
                .status,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    assert_eq!(
        policy::get(app.pool(), site.id).await.unwrap(),
        SitePolicy::default()
    );
}
