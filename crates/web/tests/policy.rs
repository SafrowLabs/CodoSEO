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

/// On the Free plan, which allows 5 must-work pages.
async fn setup_free() -> (TestApp, Site, String) {
    let app = TestApp::new().await;
    let (account, cookie) = app
        .login_with_plan("ana@example.com", Some(codoseo_core::plan::Plan::Free))
        .await;
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
async fn the_ai_tab_compares_with_the_latest_crawl() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_robots(&site, pages(), (200, BLOCK_OAI), Vec::new())
        .await;
    let res = app
        .get(&format!("/s/{}/policy?tab=ai", site.id), Some(&cookie))
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

/// The mockup's shop: a keep-out rule met, one partly met (robots.txt only), one with a gap,
/// an ignored parameter on an indexable page and a must-work page answering 500.
fn shop_pages() -> Vec<PageRecord> {
    let mut cart = page(DOMAIN, "/checkout/cart");
    cart.indexability = codoseo_core::page::Indexability::Noindex;
    cart.fields.meta_robots = Some("noindex".to_owned());
    cart.in_sitemap = false;
    let mut account = page(DOMAIN, "/account/orders");
    account.status = 0;
    account.content_type = None;
    account.indexability = codoseo_core::page::Indexability::BlockedByRobots;
    let mut down = page(DOMAIN, "/down");
    down.status = 500;
    down.indexability = codoseo_core::page::Indexability::ServerError;
    vec![
        page(DOMAIN, "/"),
        page(DOMAIN, "/pricing"),
        cart,
        account,
        page(DOMAIN, "/search?q=shirt"),
        page(DOMAIN, "/collections/winter?sort=price"),
        down,
    ]
}

const SHOP_POLICY: &str = r#"
[policy]
version = 1
[policy.seo.address]
host = "apex"
trailing_slash = "without"
[[policy.seo.index]]
pattern = "/checkout/*"
stance = "keep_out"
[[policy.seo.index]]
pattern = "/account/*"
stance = "keep_out"
[[policy.seo.index]]
pattern = "/search*"
stance = "keep_out"
[policy.seo]
index_default = "index"
ignore_params = ["utm_*", "sort"]
pagination_param = "page"
must_work = ["/pricing", "/down"]
"#;

#[tokio::test]
async fn the_seo_tab_is_the_default_and_compares_each_rule_with_the_latest_crawl() {
    let (app, site, cookie) = setup_free().await;
    app.finished_crawl(&site, shop_pages(), Vec::new()).await;
    let p = codoseo_policy::parse_file(SHOP_POLICY).unwrap().policy;
    let account = site.account_id.unwrap();
    policy::save(app.pool(), site.id, &p, &policy::Actor::web(account))
        .await
        .unwrap();
    let res = app
        .get(&format!("/s/{}/policy", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let body = &res.body;
    for needle in [
        r#"aria-current="page">SEO rules"#,
        // Tab labels count the gaps: /search* and ?sort= and /down, plus /account/* partly.
        r#"id="tab-seo-gaps">4 gaps</span>"#,
        "Preferred address",
        r#"name="a.host" value="apex" checked"#,
        "https://www.example.com",
        r#"name="a.slash" value="without" checked"#,
        r#"id="cmp-address" aria-live="polite"><b>Met</b>"#,
        "What search engines should index",
        r#"name="ix.pattern" value="/checkout/*""#,
        r#"id="cmp-ix-0" aria-live="polite"><b>Met</b>"#,
        r#"id="cmp-ix-1" aria-live="polite"><b>Partly</b>"#,
        "Google can still index the URLs from links",
        r#"id="note-ix-1">1 URL linked</span>"#,
        r#"id="cmp-ix-2" aria-live="polite"><b>1 gap</b>"#,
        "Everything else",
        r#"id="cmp-ix-else" aria-live="polite"><b>Met</b>"#,
        "URL parameters that don’t change the page",
        r#"name="params" value="utm_*, sort""#,
        r#"id="cmp-params" aria-live="polite"><b>1 gap</b>"#,
        "1 ?sort= page canonicalises to itself",
        "Pages that must always work",
        "/pricing\n/down",
        "Answers 500.",
        "200, indexable.",
        "Your plan checks up to",
        "Check thresholds",
        "Exceptions",
        // The rail lists the SEO gaps with the AI ones.
        "/down answers 500",
        r#"<span class="pol-tag pol-tag-seo">SEO</span>"#,
        r#"/policy/preview?tab=seo" hx-trigger="change""#,
        r#"action="/s/"#,
    ] {
        assert!(
            body.contains(&needle.replace("\\n", "\n")),
            "missing {needle:?}"
        );
    }
    assert!(!body.contains("robots.txt that matches"));
}

#[tokio::test]
async fn the_seo_preview_follows_the_form_lists_errors_and_adds_rules() {
    let (app, site, cookie) = setup_free().await;
    app.finished_crawl(&site, shop_pages(), Vec::new()).await;
    let url = format!("/s/{}/policy/preview?tab=seo", site.id);
    let res = app
        .post_hx(
            &url,
            "a.host=www&a.slash=without&ix.pattern=%2Fsearch*&ix.stance=index&ix.default=none&params=&pagination=&must=",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let body = &res.body;
    assert!(
        body.starts_with(r#"<aside class="pol-rail" id="pol-rail""#),
        "{body}"
    );
    assert!(body.contains(r#"id="cmp-address" aria-live="polite" hx-swap-oob="true"><b>"#));
    assert!(body.contains(r#"id="cmp-ix-0" aria-live="polite" hx-swap-oob="true"><b>Met</b>"#));
    assert!(body.contains(r#"id="tab-seo-gaps" hx-swap-oob="true""#));
    assert!(body.contains(r#"id="seo-must-rows""#));
    assert_eq!(
        policy::get(app.pool(), site.id).await.unwrap(),
        SitePolicy::default(),
        "nothing saved"
    );
    // What would stop a save shows as the owner types.
    let res = app
        .post_hx(
            &url,
            "a.host=none&ix.pattern=checkout&ix.stance=keep_out&ix.default=none&must=%2Fa%0A%2Fb%0A%2Fc%0A%2Fd%0A%2Fe%0A%2Ff",
            Some(&cookie),
        )
        .await;
    assert!(
        res.body
            .contains(r#"<div id="seo-errors" hx-swap-oob="true">"#)
    );
    assert!(res.body.contains("a pattern starts with /"), "{}", res.body);
    assert!(res.body.contains("the Free plan allows 5 must-work pages"));
    // Add rule: the rules come back with a blank one, the rest out of band.
    let res = app
        .post_hx(
            &format!("{url}&op=add"),
            "a.host=none&ix.pattern=%2Fsearch*&ix.stance=keep_out&ix.default=none",
            Some(&cookie),
        )
        .await;
    assert!(
        res.body
            .starts_with(r#"<div class="pol-rules" id="seo-index">"#),
        "{}",
        res.body
    );
    assert_eq!(res.body.matches(r#"name="ix.pattern""#).count(), 2);
    assert!(res.body.contains(
        r#"id="pol-rail" aria-label="Your rules compared with the site" hx-swap-oob="true""#
    ));
    // Remove: one fewer.
    let res = app
        .post_hx(
            &format!("{url}&op=remove:0"),
            "a.host=none&ix.pattern=%2Fsearch*&ix.stance=keep_out&ix.default=none",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.body.matches(r#"name="ix.pattern""#).count(), 0);
}

#[tokio::test]
async fn saving_the_seo_rules_keeps_the_ai_rules_and_refuses_what_the_plan_does_not_allow() {
    let (app, site, cookie) = setup_free().await;
    let mut before = SitePolicy::default();
    before.ai.probes = false;
    let account = site.account_id.unwrap();
    policy::save(app.pool(), site.id, &before, &policy::Actor::web(account))
        .await
        .unwrap();
    let res = app
        .post_hx(
            &format!("/s/{}/policy?tab=seo", site.id),
            "a.host=apex&a.slash=with&ix.pattern=%2Fcheckout%2F*&ix.stance=keep_out&ix.pattern=&ix.stance=keep_out&ix.default=index&params=utm_*%2C+gclid&pagination=page&must=%2Fpricing",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(toast_message(&res).starts_with("Policy saved"));
    assert_eq!(
        res.header("hx-replace-url"),
        Some(format!("/s/{}/policy?tab=seo", site.id).as_str())
    );
    let p = policy::get(app.pool(), site.id).await.unwrap();
    assert_eq!(
        p.seo.address,
        Some(codoseo_policy::Address {
            host: codoseo_policy::HostPref::Apex,
            trailing_slash: codoseo_policy::Slash::With
        })
    );
    assert_eq!(p.seo.index.len(), 1);
    assert_eq!(p.seo.ignore_params, ["utm_*", "gclid"]);
    assert_eq!(p.seo.must_work, ["/pricing"]);
    assert!(!p.ai.probes, "the AI rules are kept");

    // Six must-work pages on Free: shown inline, nothing saved.
    let res = app
        .post_hx(
            &format!("/s/{}/policy?tab=seo", site.id),
            "a.host=none&ix.default=none&must=%2Fa%0A%2Fb%0A%2Fc%0A%2Fd%0A%2Fe%0A%2Ff",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.header("hx-trigger").is_none(), "no toast");
    assert!(res.body.contains("These rules can't be saved yet"));
    assert!(
        res.body
            .contains(r#"<p class="pol-field-error" role="alert">the Free plan allows 5"#)
    );
    assert!(
        res.body.contains("/a\n/b")
            || res.body.contains(
                "/a
/b"
            ),
        "the form as sent"
    );
    assert_eq!(
        policy::get(app.pool(), site.id)
            .await
            .unwrap()
            .seo
            .must_work,
        ["/pricing"]
    );
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

    // The SEO tab shows the imported rule in its form.
    let seo = app
        .get(&format!("/s/{}/policy?tab=seo", site.id), Some(&cookie))
        .await;
    assert!(
        seo.body.contains(r#"name="must" rows="4" placeholder="/pricing&#10;/checkout/cart" spellcheck="false">/pricing</textarea>"#),
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

// ---- thresholds, exceptions and "When you save" ------------------------------------------

fn store_pages() -> Vec<PageRecord> {
    let mut a = page(DOMAIN, "/stores/a");
    a.fields.word_count = 100;
    let mut b = page(DOMAIN, "/stores/b");
    b.fields.word_count = 90;
    vec![page(DOMAIN, "/"), page(DOMAIN, "/pricing"), a, b]
}

/// The SEO form with a thin threshold and an exception for the store pages.
const STORE_FORM: &str = "a.host=none&ix.default=none&params=&pagination=&must=\
    &t.title_max=60&t.title_min=30&t.description_max=160&t.description_min=70\
    &t.thin_words=95&t.slow=1&t.max_redirect_hops=1&t.image_alt=flag\
    &ex.form=1&ex.check=thin_content&ex.scope=%2Fstores%2F*&ex.note=Store+pages+are+short";

#[tokio::test]
async fn thresholds_and_exceptions_count_pages_and_say_what_a_save_does() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl(&site, store_pages(), Vec::new()).await;
    let res = app
        .get(&format!("/s/{}/policy?tab=seo", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    for needle in [
        "Check thresholds",
        r#"name="t.thin_words" value="200""#,
        r#"id="cmp-t-thin_words" aria-live="polite"><b>2 pages</b><span>under 200 words, as the default</span>"#,
        r#"name="t.slow" value="1""#,
        "Exceptions",
        "No exceptions: every check counts on every page.",
        "Add exception",
        "When you save",
        "<b>0</b> issues become intended or excepted",
    ] {
        assert!(res.body.contains(needle), "missing {needle:?}");
    }
    assert!(!res.body.contains("Coming next"));

    let url = format!("/s/{}/policy/preview?tab=seo", site.id);
    let res = app.post_hx(&url, STORE_FORM, Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::OK);
    let body = &res.body;
    for needle in [
        r#"id="cmp-t-thin_words" aria-live="polite" hx-swap-oob="true"><b>1 page</b><span>under 95 words (default flags 2)</span>"#,
        r#"id="cmp-ex-0" aria-live="polite" hx-swap-oob="true"><b>1 page</b><span>excluded from the health score</span>"#,
        // /stores/a is no longer thin and /stores/b is excepted.
        "<b>2</b> issues become intended or excepted",
        "<b>0</b> new issues open now",
        "Health score <b>",
        " → ",
    ] {
        assert!(body.contains(needle), "missing {needle:?} in {body}");
    }
    assert_eq!(
        policy::get(app.pool(), site.id).await.unwrap(),
        SitePolicy::default(),
        "nothing saved"
    );

    // Add exception: the exceptions come back with a blank row.
    let res = app
        .post_hx(&format!("{url}&op=ex-add"), STORE_FORM, Some(&cookie))
        .await;
    assert!(
        res.body
            .starts_with(r#"<div class="pol-rules" id="seo-exceptions">"#),
        "{}",
        res.body
    );
    assert_eq!(res.body.matches(r#"name="ex.scope""#).count(), 2);
    let res = app
        .post_hx(&format!("{url}&op=ex-remove:0"), STORE_FORM, Some(&cookie))
        .await;
    assert_eq!(res.body.matches(r#"name="ex.scope""#).count(), 0);

    // Inline validation as the owner types.
    let res = app
        .post_hx(
            &url,
            "a.host=none&t.title_min=70&t.title_max=60&ex.form=1&ex.check=must_work_failed&ex.scope=%2Fx",
            Some(&cookie),
        )
        .await;
    assert!(
        res.body
            .contains("title_min (70) must be below title_max (60)"),
        "{}",
        res.body
    );
    assert!(res.body.contains("a policy check can"), "{}", res.body);
}

#[tokio::test]
async fn saving_thresholds_rescores_the_latest_crawl_with_a_pin_and_a_change_row() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl(&site, store_pages(), Vec::new()).await;
    let latest = app.finished_crawl(&site, store_pages(), Vec::new()).await;
    let health = |app: &TestApp| {
        let pool = app.pool().clone();
        async move {
            sqlx::query_scalar::<_, Option<i16>>("SELECT health_score FROM crawls WHERE id = $1")
                .bind(latest)
                .fetch_one(&pool)
                .await
                .unwrap()
                .unwrap()
        }
    };
    let before = health(&app).await;
    let res = app
        .post_hx(
            &format!("/s/{}/policy?tab=seo", site.id),
            STORE_FORM,
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let saved = policy::get(app.pool(), site.id).await.unwrap();
    assert_eq!(saved.seo.thresholds.thin_words, Some(95));
    assert_eq!(saved.seo.exceptions.len(), 1);
    let after = health(&app).await;
    assert!(after > before, "{before} → {after}");

    // The score trend pins the change on the crawl it judged again.
    let res = app
        .get(&format!("/s/{}/audit", site.id), Some(&cookie))
        .await;
    assert!(
        res.body.contains("Policy changed by ana@example.com"),
        "{}",
        res.body
    );
    assert!(res.body.contains(&format!("Health {before} → {after}.")));
    // The Changes page lists it with the crawl's changes.
    let res = app
        .get(&format!("/s/{}/changes", site.id), Some(&cookie))
        .await;
    assert!(
        res.body.contains("Policy changed by ana@example.com"),
        "{}",
        res.body
    );
    assert!(res.body.contains(&format!("Health {before} → {after}")));
    // The explorer marks the excepted issue, with the note.
    let b = &store_pages()[3];
    let res = app
        .get_hx(
            &format!(
                "/s/{}/explorer/detail?sel={:016x}&tab=details",
                site.id, b.url_hash
            ),
            Some(&cookie),
        )
        .await;
    assert!(res.body.contains("Page has under 95 words"), "{}", res.body);
    assert!(
        res.body.contains("Excepted · Store pages are short"),
        "{}",
        res.body
    );
}
