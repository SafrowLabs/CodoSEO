//! GA.6: the AI access screen, the intent form, "Mark intended", the sidebar count, the public
//! AI bot registry and the changes screen's AI kinds, against reports and incidents written
//! through the real `finalize`.

mod support;

use axum::http::StatusCode;
use codoseo_core::change::{Change, ChangeKind};
use codoseo_core::check::Severity;
use codoseo_core::page::PageRecord;
use codoseo_geo::{Purpose, Stance, registry};
use codoseo_store::geo;
use codoseo_store::sites::Site;
use support::{TestApp, cloud_config, page};

const DOMAIN: &str = "example.com";

/// Every bot may crawl; training is declared off limits (a Cloudflare-style signal).
const OPEN: &str = "User-agent: *\nAllow: /\nContent-Signal: search=yes, ai-train=no\n";
/// The same, plus a group that keeps OAI-SearchBot out (its `Disallow` is on line 6).
const BLOCK_OAI: &str = "User-agent: *\nAllow: /\nContent-Signal: search=yes, ai-train=no\n\nUser-agent: OAI-SearchBot\nDisallow: /\n";

fn pages() -> Vec<PageRecord> {
    vec![
        page(DOMAIN, "/"),
        page(DOMAIN, "/pricing"),
        page(DOMAIN, "/docs/start"),
    ]
}

async fn setup() -> (TestApp, Site, String) {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("ana@example.com").await;
    let site = app.site(&account, DOMAIN).await;
    (app, site, cookie)
}

async fn count(app: &TestApp, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(app.pool())
        .await
        .expect("count")
}

/// The decoded toast message of an `HX-Trigger` header.
fn toast_message(res: &support::TestResponse) -> String {
    let raw = res.header("hx-trigger").expect("a toast");
    let v: serde_json::Value = serde_json::from_str(raw).expect("json trigger");
    v["toast"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn the_screen_shows_the_report_and_the_open_incidents() {
    let (app, site, cookie) = setup().await;
    // The first report is the baseline; the second one blocks OAI-SearchBot.
    app.finished_crawl_with_robots(&site, pages(), (200, OPEN), Vec::new())
        .await;
    app.finished_crawl_with_robots(&site, pages(), (200, BLOCK_OAI), Vec::new())
        .await;

    let overview = tab_body(&app, &site, &cookie, "").await;
    let body = &ai_access_body(&app, &site, &cookie).await;
    for needle in [
        "<h1>AI access</h1>",
        "Can AI assistants and search engines reach, read and quote your site?",
        "crawl #2",
        "Site policy",
        // The tab bar.
        r#"<nav class="aia-tabbar" aria-label="AI access sections">"#,
        // Tiles.
        "GEO score",
        "AI products that can find you",
        "user bots reachable",
        "Training bots blocked",
        "Readable without JS",
        // A card per AI product: ChatGPT's search crawler is kept out, its user fetcher isn't.
        "Can AI products find and quote you?",
        "From robots.txt, fetch checks and page markup · not live traffic",
        "OAI-SearchBot · ChatGPT-User",
        "ChatGPT-User can still open a page someone asks about.",
        // The policy, what was resolved and the links to the other tabs.
        "Your policy vs what bots get",
        "Look closer",
        "Needs attention",
        // The incident, its evidence and its action.
        "OAI-SearchBot is blocked by robots.txt",
        "sev-critical",
        "robots.txt line 6 · Disallow: / · User-agent: OAI-SearchBot",
        "Blocked on all 3 important pages",
        "Mark intended",
        "Evidence grade A: documented by the operator",
        "developers.openai.com",
        &format!("/s/{}/explorer?sel=", site.id),
        // The tables and the declared preferences.
        "Search",
        "User-triggered fetchers",
        "Google Search, AI Overviews",
        "No page-level controls documented — robots.txt only",
        "Declared preference — not enforced by major engines",
        "ai-train=no",
        "Important pages",
        "Recently resolved",
    ] {
        assert!(body.contains(needle), "missing {needle:?}");
    }
    // Found by the second crawl, so it was alerted: no baseline chip.
    assert!(!body.contains(">Baseline<"));
    // The Overview holds the incident and the verdicts; the tables are on their own tabs.
    assert!(overview.contains("OAI-SearchBot is blocked by robots.txt"));
    assert!(overview.contains(&format!(r#"href="/s/{}/ai-access/bots""#, site.id)));
    assert!(!overview.contains("aia-bots-table"));
    assert!(!overview.contains(r#"id="aia-content""#));
}

#[tokio::test]
async fn each_tab_has_its_own_address_and_headings() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_robots(&site, pages(), (200, BLOCK_OAI), Vec::new())
        .await;
    for (tab, headings) in [
        (
            "",
            &[
                "Can AI products find and quote you?",
                "Needs attention",
                "Your policy vs what bots get",
                "Recently resolved",
                "Look closer",
            ][..],
        ),
        ("/bots", &[">AI bots<", ">Fetch check<"][..]),
        (
            "/pages",
            &[">Content in raw HTML<", ">Important pages<", ">AI answers<"][..],
        ),
        ("/robots", &[">robots.txt</h2>", "Content-Signal lines"][..]),
        (
            "/files",
            &[
                ">llms.txt<",
                ">Content Signals and Markdown<",
                ">Declared preferences<",
            ][..],
        ),
        (
            "/visits",
            &[
                "Coming soon",
                "Real bot visits, from your server logs",
                "Planned sources",
            ][..],
        ),
    ] {
        let body = tab_body(&app, &site, &cookie, tab).await;
        for h in headings {
            assert!(body.contains(h), "{tab}: missing {h:?}");
        }
        // One tab is current, and every tab is linked.
        let bar = &body[body.find(r#"<nav class="aia-tabbar""#).unwrap()..];
        let bar = &bar[..bar.find("</nav>").unwrap()];
        assert_eq!(bar.matches(r#"aria-current="page""#).count(), 1, "{tab}");
        for t in TABS {
            assert!(
                body.contains(&format!(r#"href="/s/{}/ai-access{t}""#, site.id)),
                "{tab} links {t}"
            );
        }
    }
    // The tab bar counts the bots and the key pages; the visits tab is not live yet.
    let body = tab_body(&app, &site, &cookie, "/robots").await;
    let total = registry().bots.len();
    assert!(body.contains(&format!(r#"Bots<span class="n">{total}</span>"#)));
    assert!(body.contains(r#"Key pages<span class="n">3</span>"#));
    assert!(body.contains(r#"Bot visits<span class="n">soon</span>"#));
    // The robots.txt tab shows the file, tested for the bot its own group names.
    assert!(body.contains("Fetched · HTTP 200"));
    assert!(body.contains("OAI-SearchBot can&#39;t fetch /"), "{body}");
    assert!(body.contains(
        r#"<div class="aia-rl is-hit"><span class="ln">6</span><span class="tx">Disallow: /</span><span class="nt">decides /</span></div>"#
    ));
    // The visits tab shows no data, made up or real.
    let visits = tab_body(&app, &site, &cookie, "/visits").await;
    assert!(visits.contains("Nothing here is data yet."));

    // Another account's site is not found on any tab.
    let (_, other) = app.login("bo@example.com").await;
    for t in TABS {
        let path = format!("/s/{}/ai-access{t}", site.id);
        assert_eq!(
            app.get(&path, Some(&other)).await.status,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
}

#[tokio::test]
async fn the_baseline_report_marks_its_incidents_as_the_baseline() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_robots(&site, pages(), (200, BLOCK_OAI), Vec::new())
        .await;
    let body = app
        .get(&format!("/s/{}/ai-access", site.id), Some(&cookie))
        .await
        .body;
    assert!(
        body.contains(
            r#"<span class="chip" title="Found by the first crawl that could check this, so no alert was sent">Baseline</span>"#
        ),
        "{body}"
    );
}

#[tokio::test]
async fn a_failing_robots_txt_replaces_the_bots_table_with_a_callout() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_robots(&site, pages(), (503, ""), Vec::new())
        .await;
    let overview = tab_body(&app, &site, &cookie, "").await;
    assert!(
        overview.contains(r#"<div class="aia-alert" role="alert">"#),
        "the Overview leads with the failing robots.txt"
    );
    let bots = tab_body(&app, &site, &cookie, "/bots").await;
    assert!(bots.contains(r#"<div class="aia-callout" role="note">"#));
    let body = ai_access_body(&app, &site, &cookie).await;
    assert!(body.contains("robots.txt answered HTTP 503"), "{body}");
    assert!(body.contains("GET /robots.txt → HTTP 503"));
    assert!(body.contains("Failing · HTTP 503"));
    assert!(!body.contains("aia-bots-table"));
    // A failing robots.txt is not a choice: there is nothing to mark as intended.
    assert!(!body.contains("Mark intended"));
    let id: i64 = sqlx::query_scalar("SELECT id FROM ai_incidents WHERE site_id = $1")
        .bind(site.id)
        .fetch_one(app.pool())
        .await
        .unwrap();
    let res = app
        .post_hx(
            &format!("/s/{}/ai-access/incidents/{id}/intended", site.id),
            "",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn before_the_first_report_the_screen_says_when_it_appears() {
    let (app, site, cookie) = setup().await;
    let res = app
        .get(&format!("/s/{}/ai-access", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(
        res.body.contains("AI access appears after the next crawl"),
        "{}",
        res.body
    );
    assert!(res.body.contains("Run crawl"));
    assert!(res.body.contains("Set your intent"));
}

#[tokio::test]
async fn another_accounts_site_is_not_found() {
    let (app, site, _) = setup().await;
    app.finished_crawl_with_robots(&site, pages(), (200, BLOCK_OAI), Vec::new())
        .await;
    let id: i64 = sqlx::query_scalar("SELECT id FROM ai_incidents WHERE site_id = $1")
        .bind(site.id)
        .fetch_one(app.pool())
        .await
        .unwrap();
    let (_, other) = app.login("bo@example.com").await;
    let base = format!("/s/{}/ai-access", site.id);
    for path in [base.clone(), format!("{base}/intent")] {
        assert_eq!(
            app.get(&path, Some(&other)).await.status,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    for path in [
        format!("{base}/intent"),
        format!("{base}/intent/reset"),
        format!("{base}/incidents/{id}/intended"),
    ] {
        assert_eq!(
            app.post(&path, "p.training=block", Some(&other))
                .await
                .status,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    // Nothing moved.
    assert_eq!(
        geo::get_intent(app.pool(), site.id).await.unwrap(),
        Default::default()
    );
    assert_eq!(
        geo::open_incidents(app.pool(), site.id)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn saving_the_intent_re_evaluates_the_latest_report_quietly() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_robots(&site, pages(), (200, OPEN), Vec::new())
        .await;
    app.finished_crawl_with_robots(&site, pages(), (200, OPEN), Vec::new())
        .await;
    assert!(
        geo::open_incidents(app.pool(), site.id)
            .await
            .unwrap()
            .is_empty()
    );

    // The form: a segmented control per purpose and a select per bot.
    let form = app
        .get(&format!("/s/{}/ai-access/intent", site.id), Some(&cookie))
        .await;
    assert_eq!(form.status, StatusCode::OK);
    for needle in [
        r#"name="p.search" value="allow" checked"#,
        r#"name="p.training" value="any" checked"#,
        r#"name="b.GPTBot""#,
        "Per-bot overrides",
        "Save intent",
        "Reset to defaults",
    ] {
        assert!(form.body.contains(needle), "missing {needle:?}");
    }

    // Training set to Block: every training bot that honours robots.txt can still crawl.
    // (No `d` box ticked: no page directive accepted.)
    let res = app
        .post_hx(
            &format!("/s/{}/ai-access/intent", site.id),
            "p.search=allow&p.user_fetch=allow&p.agent=any&p.training=block&p.ads=any&b.GPTBot=inherit&b.Googlebot=inherit",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(toast_message(&res), "Intent saved · 1 issue opened");
    assert_eq!(
        res.header("hx-push-url"),
        Some(format!("/s/{}/ai-access", site.id).as_str())
    );
    assert!(res.body.contains("can still"), "{}", res.body);
    assert!(res.body.contains("After an intent change"));
    assert!(res.body.contains("Add to robots.txt"));
    // The remedy's snippet: one `User-agent` line per bot, then `Disallow: /`.
    assert!(
        res.body
            .contains("<pre class=\"aia-snippet\">User-agent: GPTBot\nUser-agent: "),
        "{}",
        res.body
    );
    assert!(res.body.contains("\nDisallow: /</pre>"), "{}", res.body);

    let intent = geo::get_intent(app.pool(), site.id).await.unwrap();
    assert_eq!(
        intent.purposes.get(&Purpose::Training),
        Some(&Stance::Block)
    );
    // Defaults are not pinned.
    assert!(!intent.purposes.contains_key(&Purpose::Search));
    let open = geo::open_incidents(app.pool(), site.id).await.unwrap();
    assert_eq!(open.len(), 1);
    assert!(open[0].quiet, "an intent change never alerts");
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM changes WHERE kind::text LIKE 'ai_%'"
        )
        .await,
        0
    );

    // A plain form post is redirected back.
    let res = app
        .post(
            &format!("/s/{}/ai-access/intent", site.id),
            "p.training=block",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(
        res.location(),
        Some(format!("/s/{}/ai-access", site.id).as_str())
    );

    // A bad value is refused and changes nothing.
    let res = app
        .post_hx(
            &format!("/s/{}/ai-access/intent", site.id),
            "p.training=sometimes",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);

    // Reset: back to the defaults, and the issue resolves as a choice.
    let res = app
        .post_hx(
            &format!("/s/{}/ai-access/intent/reset", site.id),
            "",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(
        toast_message(&res),
        "Intent reset to the defaults · 1 issue resolved"
    );
    assert_eq!(
        geo::get_intent(app.pool(), site.id).await.unwrap(),
        Default::default()
    );
    let resolved = geo::recent_resolved(app.pool(), site.id, 10).await.unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].resolution.as_deref(), Some("intent"));
}

#[tokio::test]
async fn an_intent_saved_before_any_report_applies_from_the_next_crawl() {
    let (app, site, cookie) = setup().await;
    let res = app
        .post_hx(
            &format!("/s/{}/ai-access/intent", site.id),
            "p.training=block",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(
        toast_message(&res),
        "Intent saved · it applies from the next crawl"
    );
}

#[tokio::test]
async fn mark_intended_turns_the_finding_into_a_choice() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_robots(&site, pages(), (200, OPEN), Vec::new())
        .await;
    app.finished_crawl_with_robots(&site, pages(), (200, BLOCK_OAI), Vec::new())
        .await;
    let open = geo::open_incidents(app.pool(), site.id).await.unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].severity, Severity::Critical);
    let id = open[0].id;

    let res = app
        .post_hx(
            &format!("/s/{}/ai-access/incidents/{id}/intended", site.id),
            "",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(toast_message(&res), "Marked as intended · 1 issue resolved");
    assert_eq!(
        res.header("hx-replace-url"),
        Some(format!("/s/{}/ai-access", site.id).as_str())
    );
    // Blocked bots become blocked on purpose.
    let intent = geo::get_intent(app.pool(), site.id).await.unwrap();
    assert_eq!(intent.bots.get("OAI-SearchBot"), Some(&Stance::Block));
    assert!(
        geo::open_incidents(app.pool(), site.id)
            .await
            .unwrap()
            .is_empty()
    );
    let incident = geo::incident(app.pool(), site.id, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(incident.resolution.as_deref(), Some("intent"));
    // The Overview now lists it as resolved, ChatGPT is blocked as the policy says, and the
    // bot row on the Bots tab shows the override.
    assert!(res.body.contains("No AI access issues"), "{}", res.body);
    assert!(res.body.contains(">intended<"));
    assert!(res.body.contains("Blocked · per your policy"));
    let bots = tab_body(&app, &site, &cookie, "/bots").await;
    assert!(bots.contains(">override<"));

    // Twice is a conflict, not a second change.
    let res = app
        .post_hx(
            &format!("/s/{}/ai-access/incidents/{id}/intended", site.id),
            "",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    // An unknown incident is not found.
    let res = app
        .post(
            &format!("/s/{}/ai-access/incidents/999999/intended", site.id),
            "",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn mark_intended_on_a_bot_that_is_not_blocked_sets_no_preference() {
    let (app, site, cookie) = setup().await;
    let mut intent = codoseo_geo::Intent::default();
    intent.purposes.insert(Purpose::Training, Stance::Block);
    geo::set_intent(app.pool(), site.id, &intent).await.unwrap();
    app.finished_crawl_with_robots(&site, pages(), (200, OPEN), Vec::new())
        .await;
    let open = geo::open_incidents(app.pool(), site.id).await.unwrap();
    assert_eq!(open.len(), 1);
    let res = app
        .post(
            &format!("/s/{}/ai-access/incidents/{}/intended", site.id, open[0].id),
            "",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    let intent = geo::get_intent(app.pool(), site.id).await.unwrap();
    assert_eq!(intent.bots.get("GPTBot"), Some(&Stance::Any));
    assert!(
        geo::open_incidents(app.pool(), site.id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn the_sidebar_counts_open_issues_and_offers_the_shortcut() {
    let (app, site, cookie) = setup().await;
    let body = app
        .get(&format!("/s/{}/audit", site.id), Some(&cookie))
        .await
        .body;
    let href = format!(r#"href="/s/{}/ai-access""#, site.id);
    let item = |body: &str| {
        let start = body.find(&href).expect("the AI access nav item");
        body[start..start + body[start..].find("</a>").unwrap()].to_owned()
    };
    assert!(item(&body).contains("G I"));
    assert!(item(&body).contains(r#"<span class="count"></span>"#));

    app.finished_crawl_with_robots(&site, pages(), (200, BLOCK_OAI), Vec::new())
        .await;
    let body = app
        .get(&format!("/s/{}/audit", site.id), Some(&cookie))
        .await
        .body;
    assert!(
        item(&body).contains(r#"<span class="count alert">1</span>"#),
        "{}",
        item(&body)
    );
    assert!(body.contains("<div>AI access<span><kbd>G</kbd><kbd>I</kbd></span></div>"));
}

#[tokio::test]
async fn the_public_registry_needs_no_login_on_either_mode() {
    for app in [
        TestApp::new().await,
        TestApp::with_config(cloud_config()).await,
    ] {
        let res = app.get("/ai-bots", None).await;
        assert_eq!(res.status, StatusCode::OK);
        for needle in [
            "<h1 class=\"q-title\">AI bot registry</h1>",
            "OAI-SearchBot",
            "Common Crawl",
            "control token",
            "CC0-1.0",
            r#"href="/ai-bots.json""#,
            "data-filter=\"#reg-table\"",
        ] {
            assert!(res.body.contains(needle), "missing {needle:?}");
        }

        let res = app.get("/ai-bots.json", None).await;
        assert_eq!(res.status, StatusCode::OK);
        assert_eq!(res.header("content-type"), Some("application/json"));
        assert_eq!(res.header("access-control-allow-origin"), Some("*"));
        assert_eq!(res.header("cache-control"), Some("public, max-age=3600"));
        let json: serde_json::Value = serde_json::from_str(&res.body).expect("json");
        assert_eq!(json["license"], "CC0-1.0");
        assert_eq!(
            json["bots"].as_array().map(Vec::len),
            Some(registry().bots.len())
        );
    }
    // Only the cloud asks to be indexed.
    let cloud = TestApp::with_config(cloud_config()).await;
    assert!(
        cloud
            .get("/ai-bots", None)
            .await
            .body
            .contains("index, follow")
    );
    assert!(
        cloud
            .get("/sitemap.xml", None)
            .await
            .body
            .contains("<loc>https://codoseo.com/ai-bots</loc>")
    );
    let selfhost = TestApp::new().await;
    assert!(
        selfhost
            .get("/ai-bots", None)
            .await
            .body
            .contains("noindex, nofollow")
    );
}

/// A recovery row of a kind that can fail (llms.txt served again) does not colour the AI access
/// tile as a problem; a failure of the same kind does.
#[tokio::test]
async fn an_llms_txt_recovery_does_not_colour_the_ai_access_tile() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl(&site, pages(), Vec::new()).await;
    let llms = |severity, after: &str| Change {
        kind: ChangeKind::AiLlmsTxtUnavailable,
        severity,
        url: None,
        before: "llms.txt served".to_owned(),
        after: after.to_owned(),
    };
    let tile = |body: &str| {
        let start = body
            .find("<div class=\"tile-label\">AI access</div>")
            .expect("the AI access tile");
        let rest = &body[start..];
        rest[..rest.find("</div></div>").unwrap()].to_owned()
    };
    app.finished_crawl(&site, pages(), vec![llms(Severity::Notice, "resolved")])
        .await;
    let body = app
        .get(&format!("/s/{}/changes", site.id), Some(&cookie))
        .await
        .body;
    let t = tile(&body);
    assert!(t.ends_with(">1"), "the recovery is still counted: {t}");
    assert!(!t.contains("c-err"), "a recovery is not a problem: {t}");

    app.finished_crawl(
        &site,
        pages(),
        vec![llms(Severity::Warning, "llms.txt could not be fetched")],
    )
    .await;
    let body = app
        .get(&format!("/s/{}/changes", site.id), Some(&cookie))
        .await
        .body;
    let t = tile(&body);
    assert!(t.contains("c-err"), "a failure is: {t}");
}

#[tokio::test]
async fn the_changes_screen_renders_every_ai_kind() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl(&site, pages(), Vec::new()).await;
    let change = |kind, severity, before: &str, after: &str| Change {
        kind,
        severity,
        url: None,
        before: before.to_owned(),
        after: after.to_owned(),
    };
    app.finished_crawl(
        &site,
        pages(),
        vec![
            change(
                ChangeKind::AiBotBlocked,
                Severity::Critical,
                "allowed",
                "OAI-SearchBot is blocked by robots.txt",
            ),
            change(
                ChangeKind::AiAnswersRestricted,
                Severity::Critical,
                "eligible",
                "nosnippet on all 3 important pages, including the home page",
            ),
            change(
                ChangeKind::AiBlockNotApplied,
                Severity::Warning,
                "intent: block",
                "GPTBot can still crawl for AI training",
            ),
            change(
                ChangeKind::AiIssueResolved,
                Severity::Notice,
                "PerplexityBot is partly blocked by robots.txt",
                "resolved",
            ),
            change(
                ChangeKind::AiPreferencesChanged,
                Severity::Notice,
                "none",
                "robots.txt Content-Signal for *: search=yes, ai-train=no",
            ),
        ],
    )
    .await;

    let res = app
        .get(&format!("/s/{}/changes", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let body = &res.body;
    for needle in [
        "AI bot blocked",
        "AI answers restricted",
        "AI block not applied",
        "AI issue resolved",
        "AI preferences changed",
        // Before/after in words.
        "AI bots can crawl",
        "Eligible for AI answers",
        "Your intent: Block",
        "none declared",
        "OAI-SearchBot is blocked by robots.txt",
        "after resolved",
        "diff diff-wrap",
        // Site-wide, linked to the AI access screen, and counted in a tile.
        "site-wide",
        &format!(r#"<a class="change-link" href="/s/{}/ai-access">"#, site.id),
        "<div class=\"tile-label\">AI access</div>",
        "AI bots blocked or answers restricted",
        // The default rules card: an AI issue resolving is instant too.
        r#"<span>AI issue resolved</span><span class="badge t-accent">Instant</span>"#,
    ] {
        assert!(body.contains(needle), "missing {needle:?}");
    }
    assert_eq!(body.matches("class=\"change-link\"").count(), 5);

    // The alert grid groups the AI kinds under their own heading.
    let alerts = app.get("/settings/alerts", Some(&cookie)).await.body;
    assert!(alerts.contains(">Site changes</th>"), "{alerts}");
    assert!(alerts.contains(">AI access</th>"));
    let ai = alerts.find(">AI access</th>").unwrap();
    for label in [
        "AI bot blocked",
        "AI answers restricted",
        "AI issue resolved",
        "AI block not applied",
        "AI preferences changed",
    ] {
        assert!(alerts[ai..].contains(label), "{label} sits under AI access");
    }
}

#[tokio::test]
async fn an_incident_id_only_works_under_its_own_site() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_robots(&site, pages(), (200, BLOCK_OAI), Vec::new())
        .await;
    let id = geo::open_incidents(app.pool(), site.id).await.unwrap()[0].id;
    // Bo has a site of his own and aims at Ana's incident through it.
    let (bo, bo_cookie) = app.login("bo@example.com").await;
    let bo_site = app.site(&bo, "bo.example").await;
    let res = app
        .post_hx(
            &format!("/s/{}/ai-access/incidents/{id}/intended", bo_site.id),
            "",
            Some(&bo_cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    // Ana's own second site can't reach it either.
    let (ana, _) = app.login("ana@example.com").await;
    let other = app.site(&ana, "two.example").await;
    let res = app
        .post_hx(
            &format!("/s/{}/ai-access/incidents/{id}/intended", other.id),
            "",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    // A non-numeric id is a bad request, not a server error.
    let res = app
        .post_hx(
            &format!("/s/{}/ai-access/incidents/nope/intended", site.id),
            "",
            Some(&cookie),
        )
        .await;
    assert!(res.status.is_client_error(), "{}", res.status);
    assert_eq!(
        geo::open_incidents(app.pool(), site.id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        geo::get_intent(app.pool(), site.id).await.unwrap(),
        Default::default()
    );
}

#[tokio::test]
async fn posts_from_another_origin_are_refused() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_robots(&site, pages(), (200, BLOCK_OAI), Vec::new())
        .await;
    let id = geo::open_incidents(app.pool(), site.id).await.unwrap()[0].id;
    let base = format!("/s/{}/ai-access", site.id);
    for path in [
        format!("{base}/intent"),
        format!("{base}/intent/reset"),
        format!("{base}/incidents/{id}/intended"),
    ] {
        let res = app
            .post_with_headers(
                &path,
                "p.training=block",
                Some(&cookie),
                &[("origin", "https://evil.example")],
            )
            .await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{path}");
    }
    assert_eq!(
        geo::get_intent(app.pool(), site.id).await.unwrap(),
        Default::default()
    );
    assert_eq!(
        geo::open_incidents(app.pool(), site.id)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn unknown_form_values_are_a_bad_request() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_robots(&site, pages(), (200, OPEN), Vec::new())
        .await;
    let path = format!("/s/{}/ai-access/intent", site.id);
    for form in [
        "p.training=sometimes",
        "p.nothing=allow",
        "b.NoSuchBot=allow",
        "b.GPTBot=maybe",
        "p.search=%3Cscript%3E",
    ] {
        let res = app.post_hx(&path, form, Some(&cookie)).await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{form}");
        assert!(!res.body.contains("<script>"), "{form}: {}", res.body);
    }
    assert_eq!(
        geo::get_intent(app.pool(), site.id).await.unwrap(),
        Default::default()
    );
}

#[tokio::test]
async fn robots_txt_content_is_escaped_everywhere_it_shows() {
    let (app, site, cookie) = setup().await;
    let robots = "User-agent: *\nAllow: /\nContent-Signal: search=yes, x=<script>alert(1)</script>\nContent-Usage: /\"><svg/onload=alert(2)> train-ai=n\n\nUser-agent: OAI-SearchBot\nDisallow: /\n";
    app.finished_crawl_with_robots(&site, pages(), (200, OPEN), Vec::new())
        .await;
    app.finished_crawl_with_robots(&site, pages(), (200, robots), Vec::new())
        .await;
    let body = ai_access_body(&app, &site, &cookie).await;
    assert!(!body.contains("<script>alert(1)"), "{body}");
    assert!(!body.contains("<svg/onload"), "{body}");
    assert!(
        body.contains("x=&#60;script&#62;alert(1)&#60;/script&#62;"),
        "{body}"
    );
    assert!(
        body.contains("/&#34;&#62;&#60;svg/onload=alert(2)&#62;"),
        "{body}"
    );
    // The changes screen shows the declared preferences change as text too.
    let changes = app
        .get(&format!("/s/{}/changes", site.id), Some(&cookie))
        .await
        .body;
    assert!(!changes.contains("<script>alert(1)"), "{changes}");
}

#[tokio::test]
async fn the_bots_list_opens_on_what_needs_attention() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_robots(&site, pages(), (200, OPEN), Vec::new())
        .await;
    let body = tab_body(&app, &site, &cookie, "/bots").await;
    // Nothing conflicts: every bot shows, "All bots" is the current filter.
    let total = registry().bots.len();
    let training = registry()
        .bots
        .iter()
        .filter(|b| b.purpose == Purpose::Training)
        .count();
    let link = |show: &str| {
        format!(
            "/s/{}/ai-access/bots?show={show}&#38;group=purpose",
            site.id
        )
    };
    assert!(
        body.contains(&format!(
            r#"<a href="{}" aria-current="true">All bots<span class="n">{total}</span></a>"#,
            link("all")
        )),
        "{body}"
    );
    assert!(body.contains(&format!(
        r#"<a href="{}">Needs attention<span class="n">0</span></a>"#,
        link("attention")
    )));
    assert!(body.contains(&format!(
        r#"Training only<span class="n">{training}</span>"#
    )));
    assert_eq!(
        body.matches(r#"<tr class="aia-row aia-brow"#).count(),
        total
    );
    assert!(!body.contains("is-attn"));
    assert!(body.contains("no conflicts"));
    // The panel opens on the first bot.
    let first = &registry().bots[0].token;
    assert!(body.contains(&format!(
        r#"id="aia-panel" aria-label="Selected bot: {first}""#
    )));

    app.finished_crawl_with_robots(&site, pages(), (200, BLOCK_OAI), Vec::new())
        .await;
    let body = tab_body(&app, &site, &cookie, "/bots").await;
    assert!(body.contains(&format!(
        r#"<a href="{}" aria-current="true">Needs attention<span class="n">1</span></a>"#,
        link("attention")
    )));
    // Only the bot that needs attention shows, with its accent, under its group's count.
    assert_eq!(body.matches(r#"<tr class="aia-row aia-brow"#).count(), 1);
    assert_eq!(
        body.matches(r#"<tr class="aia-row aia-brow is-attn is-sel" data-bot="OAI-SearchBot""#)
            .count(),
        1
    );
    assert!(body.contains(r#"<span class="aia-group-flag c-err">1 needs attention</span>"#));
    assert!(body.contains(r#"<span class="aia-cell-head c-err">Disallowed</span><span class="aia-sub mono">line 6 · own group</span>"#));
    assert!(body.contains(
        r#"<span class="badge t-err" title="You allow it, but robots.txt blocks it">Blocked</span>"#
    ));
    // The panel opens on it: robots.txt says no, with the group's lines and the deciding one.
    for needle in [
        r#"id="aia-panel" aria-label="Selected bot: OAI-SearchBot""#,
        "1 · Allowed by robots.txt?",
        "No — a group names OAI-SearchBot",
        "Line 6 “Disallow: /” decides the home page, so 3 of 3 key pages are off limits.",
        r#"<div class="aia-rl"><span class="ln">5</span><span class="tx">User-agent: OAI-SearchBot</span><span class="nt"></span></div>"#,
        r#"<div class="aia-rl is-hit"><span class="ln">6</span><span class="tx">Disallow: /</span><span class="nt">decides /</span></div>"#,
        "0 of 3 key pages allowed; blocked: /, /docs/start, /pricing",
        "Open the issue",
        &format!("/s/{}/ai-access/intent#bot-OAI-SearchBot", site.id),
    ] {
        assert!(body.contains(needle), "missing {needle:?} in {body}");
    }
    let incident = geo::open_incidents(app.pool(), site.id)
        .await
        .unwrap()
        .into_iter()
        .find(|i| i.kind == codoseo_geo::findings::FindingKind::BotsBlocked)
        .unwrap();
    assert!(body.contains(&format!(
        r#"href="/s/{}/ai-access#issue-{}">Open the issue</a>"#,
        site.id, incident.id
    )));

    // Every bot, by company; the training filter; a search, without JavaScript.
    let all = tab_body(&app, &site, &cookie, "/bots?show=all&group=company").await;
    assert_eq!(all.matches(r#"<tr class="aia-row aia-brow"#).count(), total);
    assert!(all.contains(r#"<span class="aia-group-name">OpenAI</span>"#));
    assert!(all.contains(r#"By company</a>"#));
    let t = tab_body(&app, &site, &cookie, "/bots?show=training").await;
    assert_eq!(
        t.matches(r#"<tr class="aia-row aia-brow"#).count(),
        training
    );
    let q = tab_body(&app, &site, &cookie, "/bots?show=all&q=anthropic").await;
    let anthropic = registry()
        .bots
        .iter()
        .filter(|b| b.operator == "Anthropic")
        .count();
    assert_eq!(
        q.matches(r#"<tr class="aia-row aia-brow"#).count(),
        anthropic
    );
    assert!(q.contains(r#"value="anthropic""#));
    let none = tab_body(&app, &site, &cookie, "/bots?show=all&q=nobody-at-all").await;
    assert!(none.contains("No bot matches the search."));

    // ?bot= picks the panel's bot, whatever the filter shows.
    let gpt = tab_body(&app, &site, &cookie, "/bots?bot=gptbot").await;
    assert!(gpt.contains(r#"id="aia-panel" aria-label="Selected bot: GPTBot""#));
    assert!(gpt.contains(r#"class="aia-bots-layout has-pick""#));
    assert!(gpt.contains("Yes — no group names it, so it uses User-agent: *"));
    // htmx asking for the panel gets the panel alone.
    let path = format!("/s/{}/ai-access/bots?show=all&bot=ClaudeBot", site.id);
    let res = app
        .get_with_headers(
            &path,
            Some(&cookie),
            &[("hx-request", "true"), ("hx-target", "aia-panel")],
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(
        res.body.trim_start().starts_with(
            r#"<aside class="card aia-panel" id="aia-panel" aria-label="Selected bot: ClaudeBot""#
        ),
        "{}",
        res.body
    );
    assert!(!res.body.contains("<html"));
    // An unknown bot or filter falls back to the defaults.
    let odd = tab_body(
        &app,
        &site,
        &cookie,
        "/bots?show=nope&group=nope&bot=Nobody",
    )
    .await;
    assert!(odd.contains(r#"id="aia-panel" aria-label="Selected bot: OAI-SearchBot""#));
}

/// OAI-SearchBot kept out everywhere, PerplexityBot only from /docs/ (its home page is allowed).
const BLOCK_OAI_AND_DOCS: &str = "User-agent: *\nAllow: /\n\nUser-agent: OAI-SearchBot\nDisallow: /\n\nUser-agent: PerplexityBot\nDisallow: /docs/\n";

#[tokio::test]
async fn mark_intended_on_a_partly_blocked_bot_sets_no_preference() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_robots(&site, pages(), (200, OPEN), Vec::new())
        .await;
    app.finished_crawl_with_robots(&site, pages(), (200, BLOCK_OAI_AND_DOCS), Vec::new())
        .await;
    let open: Vec<_> = geo::open_incidents(app.pool(), site.id)
        .await
        .unwrap()
        .into_iter()
        .filter(|i| !i.kind.is_advisory())
        .collect();
    assert_eq!(open.len(), 1, "one issue for the search bots");
    let body = app
        .get(&format!("/s/{}/ai-access", site.id), Some(&cookie))
        .await
        .body;
    assert!(
        body.contains(
            "Set OAI-SearchBot to Block, and PerplexityBot (blocked on some pages only) to No preference, in your intent?"
        ),
        "{body}"
    );

    let res = app
        .post_hx(
            &format!("/s/{}/ai-access/incidents/{}/intended", site.id, open[0].id),
            "",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    // Block would have opened "can still crawl for AI search" for PerplexityBot at once.
    assert_eq!(toast_message(&res), "Marked as intended · 1 issue resolved");
    let intent = geo::get_intent(app.pool(), site.id).await.unwrap();
    assert_eq!(intent.bot_override("OAI-SearchBot"), Some(Stance::Block));
    assert_eq!(intent.bot_override("PerplexityBot"), Some(Stance::Any));
    assert!(
        geo::open_incidents(app.pool(), site.id)
            .await
            .unwrap()
            .iter()
            .all(|i| i.kind.is_advisory())
    );
}

#[tokio::test]
async fn mark_intended_says_when_the_change_opens_another_issue() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_robots(&site, pages(), (200, OPEN), Vec::new())
        .await;
    app.finished_crawl_with_robots(&site, pages(), (200, BLOCK_OAI), Vec::new())
        .await;
    let open = geo::open_incidents(app.pool(), site.id).await.unwrap();
    assert_eq!(open.len(), 1);
    // Training set to Block without a re-evaluation (as a save in another tab could leave it
    // for a moment): marking the search issue re-evaluates, and the training bots that can
    // still crawl open an issue of their own.
    let mut intent = codoseo_geo::Intent::default();
    intent.purposes.insert(Purpose::Training, Stance::Block);
    geo::set_intent(app.pool(), site.id, &intent).await.unwrap();
    let res = app
        .post_hx(
            &format!("/s/{}/ai-access/incidents/{}/intended", site.id, open[0].id),
            "",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(
        toast_message(&res),
        "Marked as intended · 1 issue resolved, 1 opened"
    );
}

fn nosnippet(mut p: PageRecord) -> PageRecord {
    p.fields.meta_robots = Some("nosnippet".to_owned());
    p
}

#[tokio::test]
async fn mark_intended_on_restricted_answers_accepts_the_directive() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_robots(&site, pages(), (200, OPEN), Vec::new())
        .await;
    let marked: Vec<PageRecord> = pages().into_iter().map(nosnippet).collect();
    app.finished_crawl_with_robots(&site, marked, (200, OPEN), Vec::new())
        .await;
    let open = geo::open_incidents(app.pool(), site.id).await.unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].subject, "nosnippet");
    let body = app
        .get(&format!("/s/{}/ai-access", site.id), Some(&cookie))
        .await
        .body;
    assert!(
        body.contains(
            "Accept nosnippet as intended? CodoSEO stops raising issues for it but still shows it in AI answers."
        ),
        "{body}"
    );

    let res = app
        .post_hx(
            &format!("/s/{}/ai-access/incidents/{}/intended", site.id, open[0].id),
            "",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(toast_message(&res), "Marked as intended · 1 issue resolved");
    let intent = geo::get_intent(app.pool(), site.id).await.unwrap();
    assert_eq!(
        intent.accepted_directives,
        [codoseo_geo::eligibility::DirectiveSlug::Nosnippet].into()
    );
    // The engines' crawlers keep their stance: robots.txt is still watched for Googlebot.
    assert!(intent.bots.is_empty(), "{:?}", intent.bots);
    assert!(
        geo::open_incidents(app.pool(), site.id)
            .await
            .unwrap()
            .is_empty()
    );
    // The matrix on the Key pages tab still shows it.
    let pages_tab = tab_body(&app, &site, &cookie, "/pages").await;
    assert!(pages_tab.contains("nosnippet ×3"), "{pages_tab}");

    // The intent form shows it ticked, and unticking it brings the issue back.
    let form = app
        .get(&format!("/s/{}/ai-access/intent", site.id), Some(&cookie))
        .await
        .body;
    assert!(
        form.contains("Page directives you use on purpose"),
        "{form}"
    );
    assert!(
        form.contains(r#"name="d" value="nosnippet" checked>"#),
        "{form}"
    );
    assert!(form.contains(r#"name="d" value="max_snippet">"#));
    assert!(!form.contains(r#"value="noindex""#));
    let res = app
        .post_hx(
            &format!("/s/{}/ai-access/intent", site.id),
            "p.search=allow&d=noarchive",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(toast_message(&res), "Intent saved · 1 issue opened");
    let intent = geo::get_intent(app.pool(), site.id).await.unwrap();
    assert_eq!(
        intent.accepted_directives,
        [codoseo_geo::eligibility::DirectiveSlug::Noarchive].into()
    );
    // noindex can't be accepted from the form.
    let res = app
        .post_hx(
            &format!("/s/{}/ai-access/intent", site.id),
            "d=noindex",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_bot_that_follows_another_bots_group_says_so() {
    let (app, site, cookie) = setup().await;
    let robots = "User-agent: Googlebot\nAllow: /\nDisallow: /docs/\n";
    app.finished_crawl_with_robots(&site, pages(), (200, OPEN), Vec::new())
        .await;
    app.finished_crawl_with_robots(&site, pages(), (200, robots), Vec::new())
        .await;
    let body = ai_access_body(&app, &site, &cookie).await;
    // The bots list: Applebot reads Googlebot's group.
    assert!(
        body.contains(
            r#"<span class="aia-cell-head c-ok">Allowed</span><span class="aia-sub mono">line 2 · Googlebot&#39;s group · 1 of 3 key pages blocked</span>"#
        ) || body.contains("line 2 · Googlebot&#39;s group"),
        "{body}"
    );
    // The incident: both are kept out of /docs/ by the same line.
    assert!(
        body.contains(
            "robots.txt line 3 · Disallow: /docs/ · User-agent: Googlebot (Applebot follows its rules)"
        ),
        "{body}"
    );
}

// ---- synthetic fetch checks (GB.10) ----------------------------------------------------------

use codoseo_geo::probe::{
    Challenge, InconclusiveReason, ProbeFetch, ProbeReport, ProbeResult, ProbedUrl, Vendor,
    Verdict, plan_probes, robots_skips,
};
use codoseo_geo::report::AccessReport;

/// What CodoSEObot got on every probed page.
fn baseline() -> ProbeFetch {
    ProbeFetch {
        status: Some(200),
        bytes: 48_000,
        html: true,
        title: Some("Example".to_owned()),
        ..ProbeFetch::default()
    }
}

/// What a bot got on a probed page (bot token, page index), `None` for the same page.
type Outcome = dyn Fn(&str, usize) -> Option<(Verdict, ProbeFetch)>;

/// The probe report for `report` on up to three pages: `outcome(token, page index)` says what
/// each bot got, `None` for the same page as CodoSEObot. Pairs robots.txt disallows are skipped,
/// as the executor does.
fn probes_with(report: &AccessReport, outcome: &Outcome) -> ProbeReport {
    let plan = plan_probes(report, None, 3).expect("a probe plan");
    let mut out = ProbeReport::new("hello@codoseo.com", &plan);
    for (i, &url) in plan.urls.iter().enumerate() {
        let results = plan
            .bots
            .iter()
            .map(|b| {
                if robots_skips(report, url, &b.token) {
                    return ProbeResult {
                        token: b.token.clone(),
                        fetch: None,
                        verdict: Verdict::SkippedRobots,
                    };
                }
                let (verdict, fetch) = outcome(&b.token, i).unwrap_or((Verdict::Same, baseline()));
                ProbeResult {
                    token: b.token.clone(),
                    fetch: Some(fetch),
                    verdict,
                }
            })
            .collect();
        out.urls.push(ProbedUrl {
            url,
            baseline: baseline(),
            results,
        });
        out.requests += 1 + plan.bots.len() as u16;
    }
    out
}

/// A Cloudflare challenge page.
fn cloudflare() -> (Verdict, ProbeFetch) {
    (
        Verdict::Challenged {
            vendor: Vendor::Cloudflare,
        },
        ProbeFetch {
            status: Some(403),
            bytes: 5_200,
            html: true,
            title: Some("Just a moment...".to_owned()),
            challenge: Some(Challenge {
                vendor: Vendor::Cloudflare,
                signal: "cf-mitigated: challenge".to_owned(),
            }),
            ..ProbeFetch::default()
        },
    )
}

/// Cloudflare challenges OAI-SearchBot and PerplexityBot everywhere; ChatGPT-User times out on
/// the second page.
fn waf(token: &str, page: usize) -> Option<(Verdict, ProbeFetch)> {
    match token {
        "OAI-SearchBot" | "PerplexityBot" => Some(cloudflare()),
        "ChatGPT-User" if page == 1 => Some((
            Verdict::Inconclusive {
                reason: InconclusiveReason::ProbeError,
            },
            ProbeFetch {
                error: Some(codoseo_geo::probe::FetchErrorKind::Timeout),
                ..ProbeFetch::default()
            },
        )),
        _ => None,
    }
}

/// Every tab of the AI access screen, one after the other: the Overview first, then the Bots
/// (every bot shown, not only those that need attention), Key pages, robots.txt, Agent files &
/// signals and Bot visits tabs.
async fn ai_access_body(app: &TestApp, site: &Site, cookie: &str) -> String {
    let mut out = String::new();
    for tab in TABS {
        let tab = if tab == "/bots" {
            "/bots?show=all"
        } else {
            tab
        };
        out.push_str(&tab_body(app, site, cookie, tab).await);
    }
    out
}

/// The section of `body` whose heading has the id `id`, up to its end.
fn section_of<'a>(body: &'a str, id: &str) -> &'a str {
    let from = body
        .find(&format!(r#"id="{id}""#))
        .unwrap_or_else(|| panic!("no {id} in {body}"));
    let to = from + body[from..].find("</section>").expect("the section's end");
    &body[from..to]
}

/// The tabs' paths under `/s/{site}/ai-access`, the Overview first.
const TABS: [&str; 6] = ["", "/bots", "/pages", "/robots", "/files", "/visits"];

/// One tab of the AI access screen (`""` for the Overview, else `/bots` and so on, with a query
/// if need be).
async fn tab_body(app: &TestApp, site: &Site, cookie: &str, tab: &str) -> String {
    let path = format!("/s/{}/ai-access{tab}", site.id);
    let res = app.get(&path, Some(cookie)).await;
    assert_eq!(res.status, StatusCode::OK, "{path}");
    let current = path.split('?').next().unwrap();
    assert!(
        res.body
            .contains(&format!(r#"href="{current}" aria-current="page""#)),
        "{path} is the current tab"
    );
    res.body
}

#[tokio::test]
async fn the_fetch_check_shows_in_each_bots_row_and_panel() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_probes(&site, pages(), (200, OPEN), &|r| probes_with(r, &waf))
        .await;
    let body = ai_access_body(&app, &site, &cookie).await;

    // On the Bots tab, after the bots list, badged as synthetic.
    let tab = tab_body(&app, &site, &cookie, "/bots").await;
    assert!(
        tab.find(r#"id="aia-bots""#).unwrap() < tab.find(r#"id="aia-fetch""#).unwrap(),
        "section order"
    );
    let section = section_of(&tab, "aia-fetch");
    for needle in [
        ">Fetch check</h2>",
        r#"<span class="badge aia-synthetic">Synthetic</span>"#,
        "Sent each bot&#39;s documented user agent from CodoSEO&#39;s address on 3 pages, compared with CodoSEObot. Real bots come from their own addresses and may be treated differently.",
    ] {
        assert!(section.contains(needle), "missing {needle:?} in {section}");
    }
    // The two challenged bots need attention; the list opens on them.
    assert!(
        tab.contains(r#"Needs attention<span class="n">2</span>"#),
        "{tab}"
    );
    for needle in [
        r#"<span class="aia-cell-head c-err">Blocked · 403</span><span class="aia-sub">Cloudflare challenge on 3 of 3 pages</span>"#,
        r#">Firewall blocks</span>"#,
        // The panel: OAI-SearchBot as fetched, against CodoSEObot on the home page.
        "2 · Reachable when fetched as OAI-SearchBot?",
        "No — challenged (Cloudflare) on 3 of 3 pages",
        r#"<span role="cell" class="mono">HTTP 200</span><span role="cell" class="mono c-err">HTTP 403</span>"#,
        r#"<span role="cell" class="mono">46.9 KB</span><span role="cell" class="mono c-err">5.1 KB</span>"#,
        r#"<span role="cell" class="mono">Example</span><span role="cell" class="mono c-err">Just a moment...</span>"#,
        r#"<span role="cell" class="mono c-err">cf-mitigated: challenge</span>"#,
        "Synthetic: both requests came from CodoSEO's servers",
        "Open the issue",
    ] {
        assert!(tab.contains(needle), "missing {needle:?} in {tab}");
    }
    // Every bot: what each got, or why it wasn't fetched.
    for needle in [
        // ChatGPT-User was compared on two pages and timed out on one: still the same page.
        r#"<span class="aia-cell-head c-ok">Same page as CodoSEObot</span><span class="aia-sub">2 of 2 pages</span>"#,
        r#"<span class="aia-cell-head c-muted">Not checked</span><span class="aia-sub">no full user-agent published</span>"#,
        r#"<span class="aia-cell-head c-muted">Not checked</span><span class="aia-sub">verified search engine</span>"#,
        r#"<span class="aia-cell-head c-muted">—</span><span class="aia-sub">only search and user bots are fetched</span>"#,
        r#"<span class="aia-cell-head c-muted">—</span><span class="aia-sub">control token</span>"#,
    ] {
        assert!(body.contains(needle), "missing {needle:?}");
    }
    let same = tab_body(&app, &site, &cookie, "/bots?bot=ChatGPT-User").await;
    assert!(same.contains("Yes — it gets the same page as CodoSEObot"));
    assert!(!same.contains("Open the issue"));

    // The incident it opened: synthetic, grade B, a line per bot from the probe evidence.
    let incident = section_of(&body, "aia-issues");
    for needle in [
        "Cloudflare challenge served to OAI-SearchBot and PerplexityBot on 3 pages (synthetic check)",
        r#"<span class="badge aia-synthetic">Synthetic</span>"#,
        "Evidence grade B: observed by CodoSEO&#39;s synthetic check",
        ">B</span>",
        "Challenged on all 3 probed pages",
        "Probe: HTTP 403 · 5.1 KB · “Just a moment...” · cf-mitigated: challenge · CodoSEObot: HTTP 200 · 46.9 KB · “Example”",
        "developers.cloudflare.com",
        "developers.openai.com",
    ] {
        assert!(
            incident.contains(needle),
            "missing {needle:?} in {incident}"
        );
    }
    assert!(!incident.contains("Evidence grade A"), "{incident}");
}

#[tokio::test]
async fn bots_robots_txt_keeps_out_are_not_probed_and_unwanted_ones_are_expected() {
    let (app, site, cookie) = setup().await;
    // OAI-SearchBot is disallowed; PerplexityBot is challenged, but the owner blocks it.
    geo::set_intent(
        app.pool(),
        site.id,
        &codoseo_geo::Intent {
            bots: [("PerplexityBot".to_owned(), Stance::Block)].into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    app.finished_crawl_with_probes(&site, pages(), (200, BLOCK_OAI), &|r| probes_with(r, &waf))
        .await;
    let body = ai_access_body(&app, &site, &cookie).await;
    assert!(
        body.contains(r#"<span class="aia-cell-head c-muted">Not fetched</span><span class="aia-sub">robots.txt keeps it out, so we don&#39;t</span>"#),
        "{body}"
    );
    // PerplexityBot's challenge is expected: muted, and not a status that asks for attention.
    assert!(body.contains(r#"<span class="aia-cell-head c-muted">Blocked · 403</span>"#));
    let panel = tab_body(&app, &site, &cookie, "/bots?bot=PerplexityBot").await;
    assert!(
        panel.contains("You don&#39;t ask for this bot, so it isn&#39;t reported."),
        "{panel}"
    );
    // No fetch_blocked incident: the only challenged bot is one the owner blocks.
    assert!(!body.contains("(synthetic check)"), "{body}");
}

#[tokio::test]
async fn the_fetch_check_says_why_there_are_no_results() {
    let (app, site, cookie) = setup().await;
    // An older report, written before probes existed.
    app.finished_crawl_with_robots(&site, pages(), (200, OPEN), Vec::new())
        .await;
    let body = ai_access_body(&app, &site, &cookie).await;
    assert!(body.contains(">Fetch check</h2>"), "{body}");
    assert!(
        body.contains("Fetch checks run after the next crawl"),
        "{body}"
    );

    // Switched off: says so and links to the intent.
    geo::set_ai_probes(app.pool(), site.id, false)
        .await
        .unwrap();
    let body = ai_access_body(&app, &site, &cookie).await;
    assert!(
        body.contains("Fetch checks are off for this site"),
        "{body}"
    );
    assert!(body.contains(&format!(r#"href="/s/{}/ai-access/intent#probes""#, site.id)));
    assert!(body.contains(STAY_OPEN), "{body}");
    // Even with a stored probe report.
    app.finished_crawl_with_probes(&site, pages(), (200, OPEN), &|r| probes_with(r, &waf))
        .await;
    let body = ai_access_body(&app, &site, &cookie).await;
    assert!(
        body.contains("Fetch checks are off for this site"),
        "{body}"
    );
    assert!(!body.contains("Cloudflare challenge on"), "{body}");

    // A robots.txt that can't be read: no per-bot verdicts, so no probes.
    geo::set_ai_probes(app.pool(), site.id, true).await.unwrap();
    app.finished_crawl_with_robots(&site, pages(), (503, ""), Vec::new())
        .await;
    let body = ai_access_body(&app, &site, &cookie).await;
    assert!(
        body.contains("Fetch checks need a readable robots.txt"),
        "{body}"
    );
}

#[tokio::test]
async fn the_probe_switch_is_saved_with_the_intent() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_robots(&site, pages(), (200, OPEN), Vec::new())
        .await;
    let path = format!("/s/{}/ai-access/intent", site.id);
    let form = app.get(&path, Some(&cookie)).await.body;
    for needle in [
        r#"id="probes""#,
        "Run synthetic fetch checks",
        r#"type="hidden" name="probes" value="off""#,
        r#"name="probes" value="on" checked"#,
    ] {
        assert!(form.contains(needle), "missing {needle:?} in {form}");
    }

    // Unticked: only the hidden field is sent.
    let res = app.post_hx(&path, "probes=off", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(!geo::get_ai_probes(app.pool(), site.id).await.unwrap());
    let bots = tab_body(&app, &site, &cookie, "/bots").await;
    assert!(bots.contains("Fetch checks are off for this site"));
    let form = app.get(&path, Some(&cookie)).await.body;
    assert!(
        !form.contains(r#"name="probes" value="on" checked"#),
        "{form}"
    );

    // A post without the field (an older form) leaves it as it is.
    app.post_hx(&path, "p.training=block", Some(&cookie)).await;
    assert!(!geo::get_ai_probes(app.pool(), site.id).await.unwrap());

    // Ticked again.
    app.post_hx(&path, "probes=off&probes=on", Some(&cookie))
        .await;
    assert!(geo::get_ai_probes(app.pool(), site.id).await.unwrap());

    let res = app.post_hx(&path, "probes=maybe", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn probed_page_content_is_escaped_everywhere_it_shows() {
    let (app, site, cookie) = setup().await;
    let hostile = |token: &str, _page: usize| -> Option<(Verdict, ProbeFetch)> {
        match token {
            "OAI-SearchBot" => Some((
                Verdict::Challenged {
                    vendor: Vendor::Generic,
                },
                ProbeFetch {
                    status: Some(200),
                    bytes: 900,
                    html: true,
                    title: Some("<script>alert(1)</script>".to_owned()),
                    challenge: Some(Challenge {
                        vendor: Vendor::Generic,
                        signal: "<img src=x onerror=alert(2)>".to_owned(),
                    }),
                    ..ProbeFetch::default()
                },
            )),
            "PerplexityBot" => {
                let to = "https://evil.example/\"><svg/onload=alert(3)>".to_owned();
                Some((
                    Verdict::Diverted { to: to.clone() },
                    ProbeFetch {
                        status: Some(200),
                        bytes: 900,
                        html: true,
                        final_url: Some(to),
                        redirects: 1,
                        ..ProbeFetch::default()
                    },
                ))
            }
            _ => None,
        }
    };
    app.finished_crawl_with_probes(&site, pages(), (200, OPEN), &|r| probes_with(r, &hostile))
        .await;
    let body = ai_access_body(&app, &site, &cookie).await;
    assert!(body.contains("(synthetic check)"), "{body}");
    for raw in ["<script>alert(1)", "<img src=x", "<svg/onload"] {
        assert!(!body.contains(raw), "unescaped {raw:?}: {body}");
    }
    for escaped in [
        "&#60;script&#62;alert(1)&#60;/script&#62;",
        "&#60;img src=x onerror=alert(2)&#62;",
        "https://evil.example/&#34;&#62;&#60;svg/onload=alert(3)&#62;",
    ] {
        assert!(body.contains(escaped), "missing {escaped:?}: {body}");
    }
}

#[tokio::test]
async fn the_fetch_blocked_kind_reads_well_on_changes_and_alerts() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl(&site, pages(), Vec::new()).await;
    app.finished_crawl(
        &site,
        pages(),
        vec![Change {
            kind: ChangeKind::AiFetchBlocked,
            severity: Severity::Warning,
            url: None,
            before: "reachable".to_owned(),
            after:
                "Cloudflare challenge served to OAI-SearchBot on the home page (synthetic check)"
                    .to_owned(),
        }],
    )
    .await;
    let changes = app
        .get(&format!("/s/{}/changes", site.id), Some(&cookie))
        .await
        .body;
    for needle in [
        "AI fetch blocked (synthetic)",
        "AI bots got through",
        "on the home page (synthetic check)",
    ] {
        assert!(changes.contains(needle), "missing {needle:?}");
    }
    let alerts = app.get("/settings/alerts", Some(&cookie)).await.body;
    let ai = alerts.find(">AI access</th>").expect("AI access section");
    assert!(
        alerts[ai..].contains("AI fetch blocked (synthetic)"),
        "{alerts}"
    );
    assert!(
        alerts[ai..].contains(
            "your firewall or CDN blocks or challenges AI bots you want (synthetic check)"
        ),
        "{alerts}"
    );
}

#[tokio::test]
async fn the_registry_page_shows_each_documented_user_agent() {
    let app = TestApp::new().await;
    let body = app.get("/ai-bots", None).await.body;
    let escape = |s: &str| {
        s.replace('&', "&#38;")
            .replace('<', "&#60;")
            .replace('>', "&#62;")
            .replace('"', "&#34;")
            .replace('\'', "&#39;")
    };
    let mut documented = 0;
    for bot in &registry().bots {
        if let Some(ua) = &bot.user_agent {
            documented += 1;
            assert!(
                body.contains(&escape(ua)),
                "missing the user agent of {}",
                bot.token
            );
        }
    }
    assert!(documented > 0);
    assert!(body.contains("User agent"), "{body}");
}

#[tokio::test]
async fn probe_urls_zero_turns_the_checks_off_for_the_whole_install() {
    let config =
        codoseo_web::Config::from_lookup(|k| (k == "CODOSEO_PROBE_URLS").then(|| "0".to_owned()))
            .expect("config");
    assert_eq!(config.probe_urls, Some(0));
    let app = TestApp::with_config(config).await;
    let (account, cookie) = app.login("ana@example.com").await;
    let site = app.site(&account, DOMAIN).await;
    app.finished_crawl_with_robots(&site, pages(), (200, OPEN), Vec::new())
        .await;

    let body = ai_access_body(&app, &site, &cookie).await;
    assert!(
        body.contains("Fetch checks are turned off on this install"),
        "{body}"
    );
    assert!(body.contains(STAY_OPEN), "{body}");
    assert!(
        !body.contains("Fetch checks run after the next crawl"),
        "{body}"
    );

    // The intent form can't switch them on, and saving it leaves the site's switch alone.
    let path = format!("/s/{}/ai-access/intent", site.id);
    let form = app.get(&path, Some(&cookie)).await.body;
    assert!(form.contains("turned off on this install"), "{form}");
    assert!(!form.contains(r#"type="hidden" name="probes""#), "{form}");
    assert!(form.contains(r#"value="on" checked disabled"#), "{form}");
    app.post_hx(&path, "p.training=block", Some(&cookie)).await;
    assert!(geo::get_ai_probes(app.pool(), site.id).await.unwrap());
}

/// What the AI access screen says about open synthetic incidents while the checks are off.
const STAY_OPEN: &str = "Open synthetic incidents stay open until fetch checks are turned back on or the issue is resolved.";

// ---- B2.12: content in raw HTML and llms.txt -----------------------------------------------

/// An app shell: no text, an empty mount element and a noscript notice.
fn shell_page(domain: &str, path: &str) -> PageRecord {
    let mut p = page(domain, path);
    p.fields.word_count = 0;
    p.fields.h1.clear();
    p.fields.raw.mount_empty = true;
    p.fields.raw.noscript_js = true;
    p
}

fn llms_fetch(status: u16, body: &str) -> codoseo_core::output::LlmsTxtFetch {
    codoseo_core::output::LlmsTxtFetch {
        status,
        content_type: Some("text/plain".to_owned()),
        bytes: body.len() as u64,
        body: body.to_owned(),
        ..Default::default()
    }
}

#[tokio::test]
async fn the_content_card_lists_each_important_page_with_its_verdict() {
    let (app, site, cookie) = setup().await;
    let pages = vec![
        page(DOMAIN, "/"),
        shell_page(DOMAIN, "/app"),
        page(DOMAIN, "/pricing"),
    ];
    app.finished_crawl_with_robots(&site, pages, (200, OPEN), Vec::new())
        .await;
    let body = ai_access_body(&app, &site, &cookie).await;
    // First on the Key pages tab.
    let tab = tab_body(&app, &site, &cookie, "/pages").await;
    assert!(
        tab.find(r#"id="aia-content""#).unwrap() < tab.find(r#"id="aia-answers""#).unwrap(),
        "section order"
    );
    let card = section_of(&body, "aia-content");
    for needle in [
        "Content in raw HTML",
        "as AI crawlers that don't run JavaScript see it",
        ">A-</span>",
        "before JavaScript runs",
        "vercel.com/blog/the-rise-of-the-ai-crawler",
        "/app",
        "/pricing",
        ">Shell</span>",
        ">Present</span>",
        "640",
        "3 pages · 1 shell",
        "Signals: the app&#39;s mount element is empty, a noscript notice asks for JavaScript",
    ] {
        assert!(card.contains(needle), "missing {needle:?} in {card}");
    }
    assert_eq!(
        card.matches("Signals:").count(),
        1,
        "only shell rows: {card}"
    );
    // The shell page is an incident, graded A-, with its page as evidence.
    let issues = section_of(&body, "aia-issues");
    for needle in [
        "empty app shell",
        "Evidence grade A-",
        "0 words in the HTML as served",
        "the app&#39;s mount element is empty",
        "vercel.com",
    ] {
        assert!(issues.contains(needle), "missing {needle:?} in {issues}");
    }
}

#[tokio::test]
async fn the_content_card_says_why_it_has_no_rows() {
    let (app, site, cookie) = setup().await;
    // A page that is not HTML: nothing to read.
    let mut api = page(DOMAIN, "/");
    api.content_type = Some("application/json".to_owned());
    app.finished_crawl_with_robots(&site, vec![api], (200, OPEN), Vec::new())
        .await;
    let body = ai_access_body(&app, &site, &cookie).await;
    assert!(body.contains("No HTML pages to read"), "{body}");

    // A report from before content was read.
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_robots(&site, pages(), (200, OPEN), Vec::new())
        .await;
    sqlx::query("UPDATE ai_reports SET report = report - 'content' - 'llms_txt'")
        .execute(app.pool())
        .await
        .expect("strip");
    let body = ai_access_body(&app, &site, &cookie).await;
    assert!(
        body.contains("Content checks run after the next crawl"),
        "{body}"
    );
    assert!(body.contains("Not checked in this report"), "{body}");
    assert!(!body.contains(">Present</span>"));
}

#[tokio::test]
async fn an_llms_txt_robots_txt_disallows_says_so() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_robots(&site, pages(), (200, OPEN), Vec::new())
        .await;
    sqlx::query(
        "UPDATE ai_reports SET report = (report - 'llms_txt') || '{\"llms_txt_skipped\":\"robots_disallowed\"}'",
    )
    .execute(app.pool())
    .await
    .expect("skip");
    let body = ai_access_body(&app, &site, &cookie).await;
    let card = section_of(&body, "aia-llms");
    assert!(
        card.contains("robots.txt disallows /llms.txt (or where it redirects) for CodoSEObot"),
        "{card}"
    );
    assert!(
        !card.contains("Reports from before llms.txt was read"),
        "{card}"
    );
}

#[tokio::test]
async fn a_missing_llms_txt_is_a_neutral_line_without_warning_styles() {
    for fetch in [
        llms_fetch(404, ""),
        llms_fetch(200, "<!doctype html><html><body>Welcome</body></html>"),
    ] {
        let (app, site, cookie) = setup().await;
        app.finished_crawl_with_llms(&site, pages(), (200, OPEN), fetch, false)
            .await;
        let body = ai_access_body(&app, &site, &cookie).await;
        let card = section_of(&body, "aia-llms");
        assert!(card.contains("Informational · not scored"), "{card}");
        assert!(
            card.contains("No llms.txt — optional, nothing to fix"),
            "{card}"
        );
        assert!(card.contains("llmstxt.org"), "{card}");
        for bad in ["t-warn", "t-err", "c-warn", "c-err", "alert"] {
            assert!(!card.contains(bad), "{bad} in {card}");
        }
        assert!(!body.contains("llms.txt returns"), "no incident");
    }
}

#[tokio::test]
async fn an_available_llms_txt_shows_its_checks_and_links() {
    let (app, site, cookie) = setup().await;
    let file = "# Example\n\n> A site about examples.\n\n## Docs\n\n- [Start](https://example.com/docs/start): begin here\n- [Elsewhere](https://other.example.org/x)\n";
    app.finished_crawl_with_llms(&site, pages(), (200, OPEN), llms_fetch(200, file), false)
        .await;
    let body = ai_access_body(&app, &site, &cookie).await;
    let card = section_of(&body, "aia-llms");
    for needle in [
        "Informational · not scored",
        ">Available</span>",
        "Starts with an H1",
        "Sections of links",
        "Links",
        "not checked",
    ] {
        assert!(card.contains(needle), "missing {needle:?} in {card}");
    }
}

#[tokio::test]
async fn an_adopted_llms_txt_that_fails_is_an_incident_graded_a() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_llms(&site, pages(), (200, OPEN), llms_fetch(503, ""), true)
        .await;
    let body = ai_access_body(&app, &site, &cookie).await;
    let issues = section_of(&body, "aia-issues");
    for needle in [
        "llms.txt returns HTTP 503",
        "GET /llms.txt → HTTP 503",
        ">A</span>",
        "llmstxt.org",
        "developer.chrome.com",
    ] {
        assert!(issues.contains(needle), "missing {needle:?} in {issues}");
    }
    let card = section_of(&body, "aia-llms");
    assert!(
        card.contains(">Unavailable</span>") && card.contains("HTTP 503"),
        "{card}"
    );
}

#[tokio::test]
async fn a_huge_llms_txt_h1_is_clipped_on_the_card() {
    let (app, site, cookie) = setup().await;
    let body = format!(
        "# {}\n\n> A site.\n\n## Docs\n\n- [Start](/start)\n",
        "A".repeat(100 * 1024)
    );
    app.finished_crawl_with_llms(&site, pages(), (200, OPEN), llms_fetch(200, &body), false)
        .await;
    // A report stored before details were capped is clipped on the way out too.
    sqlx::query(
        "UPDATE ai_reports SET report = jsonb_set(report, '{llms_txt,checks,0,detail}', to_jsonb(repeat('B', 50000)))",
    )
    .execute(app.pool())
    .await
    .expect("old detail");
    let page = ai_access_body(&app, &site, &cookie).await;
    let card = section_of(&page, "aia-llms");
    assert!(card.contains("Starts with an H1"), "{card}");
    // Backtick spans in details are code, as in incident summaries.
    assert!(
        card.contains("A <code>&#62;</code> summary follows the H1."),
        "{card}"
    );
    // The tick says what it means in text, not in an aria-label on a plain span.
    assert!(
        card.contains(r#"<span class="sr-only">Passes:</span>"#),
        "{card}"
    );
    assert!(!card.contains("aria-label=\"passes\""), "{card}");
    assert!(!page.contains(&"A".repeat(201)), "H1 clipped");
    assert!(!card.contains(&"B".repeat(401)), "detail clipped");
    assert!(card.contains(&format!("{}…", "B".repeat(399))), "{card}");
}

#[test]
fn long_llms_txt_urls_and_details_wrap_on_a_phone() {
    let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/app.css"))
        .expect("app.css");
    for rule in [
        ".aia-llms-failing .aia-path { min-width: 0; max-width: 100%; overflow-wrap: anywhere; }",
        ".aia-llms-check .aia-ev-detail, .aia-llms-facts dd { min-width: 0; overflow-wrap: anywhere; }",
    ] {
        assert!(css.contains(rule), "missing {rule}");
    }
}

#[tokio::test]
async fn llms_txt_text_is_escaped_everywhere_it_shows() {
    let (app, site, cookie) = setup().await;
    let body = "# <script>alert(\"h1\")</script> \"Quoted\"\n\n> A site.\n\n## Docs\n\n- [Start](/start)\n\n## <img src=x onerror=\"alert(1)\">\n\nprose\n";
    let mut fetch = llms_fetch(200, body);
    fetch.links = vec![codoseo_core::output::LinkStatus {
        url: "https://example.com/\"><script>alert(\"url\")</script>".to_owned(),
        status: Some(404),
        from_crawl: false,
    }];
    app.finished_crawl_with_llms(&site, pages(), (200, OPEN), fetch, false)
        .await;
    let page = ai_access_body(&app, &site, &cookie).await;
    let card = section_of(&page, "aia-llms");
    for raw in ["<script>alert(", "<img src=x", "\"Quoted\"", "\"><script>"] {
        assert!(!page.contains(raw), "unescaped {raw:?} in {card}");
    }
    for escaped in [
        "&#60;script&#62;alert(&#34;h1&#34;)&#60;/script&#62; &#34;Quoted&#34;",
        "&#60;img src=x onerror=&#34;alert(1)&#34;&#62;",
        "&#34;&#62;&#60;script&#62;alert(&#34;url&#34;)&#60;/script&#62;",
    ] {
        assert!(card.contains(escaped), "missing {escaped:?} in {card}");
    }
}

#[tokio::test]
async fn an_unavailable_llms_txt_is_warning_styled_only_when_adopted_and_names_the_error() {
    let no_answer = || codoseo_core::output::LlmsTxtFetch {
        error: Some("request timed out".to_owned()),
        ..Default::default()
    };
    // Adopted: a warning badge, the error on the card and in the incident.
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_llms(&site, pages(), (200, OPEN), no_answer(), true)
        .await;
    let page = ai_access_body(&app, &site, &cookie).await;
    let card = section_of(&page, "aia-llms");
    assert!(
        card.contains(r#"<span class="badge t-warn">Unavailable</span>"#),
        "{card}"
    );
    assert!(card.contains("<dt>Adopted</dt><dd>Yes</dd>"), "{card}");
    assert!(card.contains("No answer (request timed out)"), "{card}");
    let issues = section_of(&page, "aia-issues");
    assert!(
        issues.contains("GET /llms.txt → no answer (request timed out)"),
        "{issues}"
    );
    // Not adopted: the same answer is muted and raises nothing.
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_llms(&site, pages(), (200, OPEN), no_answer(), false)
        .await;
    let page = ai_access_body(&app, &site, &cookie).await;
    let card = section_of(&page, "aia-llms");
    assert!(
        card.contains(r#"<span class="badge t-muted">Unavailable</span>"#),
        "{card}"
    );
    assert!(card.contains("<dt>Adopted</dt><dd>No</dd>"), "{card}");
    assert!(
        !page.contains("llms.txt could not be fetched"),
        "no incident"
    );
}

#[tokio::test]
async fn a_content_incident_lists_its_pages_once_with_a_count_of_the_rest() {
    let (app, site, cookie) = setup().await;
    let mut pages = vec![page(DOMAIN, "/")];
    for i in 0..8 {
        pages.push(shell_page(DOMAIN, &format!("/app{i}")));
    }
    app.finished_crawl_with_robots(&site, pages, (200, OPEN), Vec::new())
        .await;
    let body = ai_access_body(&app, &site, &cookie).await;
    let issues = section_of(&body, "aia-issues");
    assert!(
        issues.contains("8 important pages are empty app shells"),
        "{issues}"
    );
    // Five pages in the shells grid, the other three counted; no second "Pages" list.
    assert_eq!(issues.matches(">Shell</span>").count(), 5, "{issues}");
    assert!(issues.contains("+3 more"), "{issues}");
    assert!(
        !issues.contains(r#"<span class="aia-urls-label">Pages</span>"#),
        "{issues}"
    );
}

// ---- B3.10: the incident card's repair actions -----------------------------------------------

/// A site whose second crawl blocks OAI-SearchBot: one open, alerted `bots_blocked` incident.
async fn blocked_incident() -> (TestApp, Site, String, i64) {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_robots(&site, pages(), (200, OPEN), Vec::new())
        .await;
    app.finished_crawl_with_robots(&site, pages(), (200, BLOCK_OAI), Vec::new())
        .await;
    let id = geo::open_incidents(app.pool(), site.id).await.unwrap()[0].id;
    (app, site, cookie, id)
}

/// A site whose adopted llms.txt fails: one open `llms_txt_unavailable` incident (acceptable).
async fn llms_incident() -> (TestApp, Site, String, i64) {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_llms(&site, pages(), (200, OPEN), llms_fetch(503, ""), true)
        .await;
    let id = geo::open_incidents(app.pool(), site.id).await.unwrap()[0].id;
    (app, site, cookie, id)
}

fn action(site: &Site, id: i64, what: &str) -> String {
    format!("/s/{}/ai-access/incidents/{id}/{what}", site.id)
}

/// The sidebar's AI access item.
fn nav_item(body: &str, site: &Site) -> String {
    let href = format!(r#"href="/s/{}/ai-access""#, site.id);
    let start = body.find(&href).expect("the AI access nav item");
    body[start..start + body[start..].find("</a>").unwrap()].to_owned()
}

/// The card of incident `id` on the page.
fn card(body: &str, id: i64) -> String {
    let start = body
        .find(&format!(r#"id="issue-{id}""#))
        .unwrap_or_else(|| panic!("no card for {id} in {body}"));
    body[start..start + body[start..].find("</article>").unwrap()].to_owned()
}

#[tokio::test]
async fn a_card_shows_the_remedy_the_copy_buttons_and_the_honest_line() {
    let (app, site, cookie, id) = blocked_incident().await;
    let body = ai_access_body(&app, &site, &cookie).await;
    let card = card(&body, id);
    for needle in [
        "How to fix",
        "Copy fix",
        "Copy task",
        "Copy prompt",
        "data-copy=",
        "Recheck now",
        "Mark intended",
        "Policy restored ≠ back in AI answers",
        // Before and after.
        "OAI-SearchBot allowed on all",
        "OAI-SearchBot blocked on all 3 important pages",
        // The ledger's grade and sources.
        "Evidence grade A: documented by the operator",
        "developers.openai.com",
        "Snooze",
    ] {
        assert!(card.contains(needle), "missing {needle:?} in {card}");
    }
    // Bot and directive kinds go through the intent: no Accept.
    assert!(!card.contains(">Accept<"), "{card}");
}

#[tokio::test]
async fn rechecking_queues_one_and_the_card_says_so() {
    let (app, site, cookie, id) = blocked_incident().await;
    let res = app
        .post_hx(&action(&site, id, "recheck"), "", Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(
        toast_message(&res),
        "Recheck queued · results in a minute or two"
    );
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM ai_rechecks WHERE status = 'queued' AND origin = 'manual'"
        )
        .await,
        1
    );
    assert!(
        card(&res.body, id).contains("Recheck queued"),
        "{}",
        res.body
    );
    // The card's state changed, and the page polls until the result is in.
    let c = card(&res.body, id);
    assert!(
        c.contains(r#"data-state="Recheck queued · results in a minute or two|""#),
        "{c}"
    );
    assert!(
        c.contains(r#"is-pending" role="status" data-recheck-pending>"#),
        "{c}"
    );
    // A plain form post is sent back to the page.
    let res = app
        .post(&action(&site, id, "recheck"), "", Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert!(
        res.body
            .contains("A recheck of this issue is already queued"),
        "{}",
        res.body
    );
}

#[tokio::test]
async fn refused_rechecks_say_why_and_when() {
    let (app, site, cookie, id) = blocked_incident().await;
    let path = action(&site, id, "recheck");

    // Cooldown: one finished a minute ago.
    app.post_hx(&path, "", Some(&cookie)).await;
    sqlx::query("UPDATE ai_rechecks SET status = 'done', outcome = 'still_failing', created_at = now() - interval '1 minute', finished_at = now()")
        .execute(app.pool())
        .await
        .unwrap();
    let res = app.post_hx(&path, "", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert!(
        res.body.contains("Rechecked moments ago. Try again at"),
        "{}",
        res.body
    );

    // The card says what the last one found.
    let body = ai_access_body(&app, &site, &cookie).await;
    assert!(
        card(&body, id).contains("Last recheck: still failing"),
        "{body}"
    );

    // Busy while verifying a fix: a follow-up is queued.
    sqlx::query("UPDATE ai_rechecks SET created_at = now() - interval '1 hour'")
        .execute(app.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE ai_incidents SET recovered_at = now() WHERE id = $1")
        .bind(id)
        .execute(app.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO ai_rechecks (site_id, incident_id, origin, due_at) \
         VALUES ($1, $2, 'followup', now() + interval '24 hours')",
    )
    .bind(site.id)
    .bind(id)
    .execute(app.pool())
    .await
    .unwrap();
    let res = app.post_hx(&path, "", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert!(
        res.body.contains("Already verifying — due at"),
        "{}",
        res.body
    );
    let body = ai_access_body(&app, &site, &cookie).await;
    let c = card(&body, id);
    assert!(
        c.contains("Fix detected · policy restored — verifying until"),
        "{c}"
    );
    assert!(c.contains("Policy restored ≠ back in AI answers"), "{c}");
    assert!(c.contains("openai.com"), "the lag's source: {c}");

    // A crawl is running: it will judge it.
    sqlx::query("UPDATE ai_rechecks SET status = 'superseded' WHERE origin = 'followup'")
        .execute(app.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO crawls (site_id, domain, trigger, priority, status) \
         VALUES ($1, $2, 'manual', 2, 'queued')",
    )
    .bind(site.id)
    .bind(&site.domain)
    .execute(app.pool())
    .await
    .unwrap();
    let res = app.post_hx(&path, "", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert!(
        res.body.contains("A crawl is running; it will judge this"),
        "{}",
        res.body
    );
    // The follow-up gave way to the crawl: the card waits for it.
    let body = ai_access_body(&app, &site, &cookie).await;
    assert!(
        card(&body, id).contains("Waiting for the next crawl"),
        "{body}"
    );
}

#[tokio::test]
async fn the_daily_recheck_cap_is_the_plan_s() {
    let app = TestApp::with_config(cloud_config()).await;
    let (account, cookie) = app
        .login_with_plan("ana@example.com", Some(codoseo_core::plan::Plan::Free))
        .await;
    let site = app.site(&account, DOMAIN).await;
    app.finished_crawl_with_robots(&site, pages(), (200, BLOCK_OAI), Vec::new())
        .await;
    let id = geo::open_incidents(app.pool(), site.id).await.unwrap()[0].id;
    for _ in 0..3 {
        sqlx::query(
            "INSERT INTO ai_rechecks (site_id, incident_id, origin, status, outcome, due_at, created_at) \
             VALUES ($1, $2, 'manual', 'done', 'still_failing', now(), now() - interval '2 hours')",
        )
        .bind(site.id)
        .bind(id)
        .execute(app.pool())
        .await
        .unwrap();
    }
    let res = app
        .post_hx(&action(&site, id, "recheck"), "", Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    assert!(
        res.body
            .contains("3 rechecks per day on your plan — next at"),
        "{}",
        res.body
    );
}

#[tokio::test]
async fn accepting_mutes_the_incident_and_lists_it_under_accepted() {
    let (app, site, cookie, id) = llms_incident().await;
    let body = ai_access_body(&app, &site, &cookie).await;
    assert!(card(&body, id).contains(">Accept<"), "{body}");
    assert!(nav_item(&body, &site).contains(">1<"));

    let res = app
        .post_hx(&action(&site, id, "accept"), "", Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(
        toast_message(&res),
        "Accepted · it no longer counts or alerts"
    );
    let i = geo::incident(app.pool(), site.id, id)
        .await
        .unwrap()
        .unwrap();
    assert!(i.accepted_at.is_some());
    let body = &res.body;
    assert!(body.contains("Accepted (1)"), "{body}");
    assert!(card(body, id).contains("Unaccept"));
    assert!(
        nav_item(body, &site).contains(r#"<span class="count"></span>"#),
        "{}",
        nav_item(body, &site)
    );
    // The open issues list and its count leave it out.
    let open = &body[body.find(r#"id="aia-issues""#).unwrap()..body.find("Accepted (1)").unwrap()];
    assert!(!open.contains(&format!(r#"id="issue-{id}""#)), "{open}");
    // Not the all-clear: something is there, set aside.
    assert!(!body.contains("No AI access issues"), "{body}");
    assert!(
        body.contains("Nothing needs attention · 1 accepted"),
        "{body}"
    );

    let res = app
        .post_hx(&action(&site, id, "unaccept"), "", Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(
        toast_message(&res),
        "No longer accepted · it counts and alerts again"
    );
    assert!(!res.body.contains("Accepted (1)"));
    assert!(
        geo::incident(app.pool(), site.id, id)
            .await
            .unwrap()
            .unwrap()
            .accepted_at
            .is_none()
    );

    // A plain form post redirects back.
    let res = app
        .post(&action(&site, id, "accept"), "", Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(
        res.location(),
        Some(format!("/s/{}/ai-access", site.id).as_str())
    );
}

#[tokio::test]
async fn snoozing_takes_one_seven_or_thirty_days_and_leaves_the_badge() {
    let (app, site, cookie, id) = blocked_incident().await;
    for bad in ["days=5", "days=90", "days=", "days=abc", ""] {
        let res = app
            .post_hx(&action(&site, id, "snooze"), bad, Some(&cookie))
            .await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    assert!(
        geo::incident(app.pool(), site.id, id)
            .await
            .unwrap()
            .unwrap()
            .snoozed_until
            .is_none()
    );

    let res = app
        .post_hx(&action(&site, id, "snooze"), "days=7", Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(
        toast_message(&res).starts_with("Snoozed until "),
        "{}",
        toast_message(&res)
    );
    let until = geo::incident(app.pool(), site.id, id)
        .await
        .unwrap()
        .unwrap()
        .snoozed_until
        .unwrap();
    let left = until - time::OffsetDateTime::now_utc();
    assert!(
        left > time::Duration::days(6) && left <= time::Duration::days(7),
        "{left}"
    );
    let c = card(&res.body, id);
    assert!(c.contains("Snoozed until"), "{c}");
    assert!(c.contains("Unsnooze"), "{c}");
    assert!(nav_item(&res.body, &site).contains(r#"<span class="count"></span>"#));
    // Still listed among the open issues, but not counted.
    let tile = &res.body[res.body.find("Open issues").unwrap()..];
    assert!(
        tile.contains("nothing to fix") || tile.contains(">0<"),
        "{tile}"
    );

    let res = app
        .post_hx(&action(&site, id, "unsnooze"), "", Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(toast_message(&res), "Snooze ended");
    assert!(
        geo::incident(app.pool(), site.id, id)
            .await
            .unwrap()
            .unwrap()
            .snoozed_until
            .is_none()
    );
}

#[tokio::test]
async fn a_failing_robots_txt_can_only_be_snoozed() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_robots(&site, pages(), (503, ""), Vec::new())
        .await;
    let id = geo::open_incidents(app.pool(), site.id).await.unwrap()[0].id;
    let body = ai_access_body(&app, &site, &cookie).await;
    let c = card(&body, id);
    assert!(
        !c.contains(">Accept<") && !c.contains("Mark intended"),
        "{c}"
    );
    assert!(c.contains("Snooze"), "{c}");
    assert!(
        c.contains("A failing robots.txt can&#39;t be accepted")
            || c.contains("A failing robots.txt can&#x27;t be accepted"),
        "{c}"
    );
    let res = app
        .post_hx(&action(&site, id, "accept"), "", Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert!(res.body.contains("can only be snoozed"), "{}", res.body);
}

#[tokio::test]
async fn the_repair_actions_are_not_found_for_another_account() {
    let (app, site, _, id) = blocked_incident().await;
    let (_, other) = app.login("bo@example.com").await;
    for what in ["recheck", "accept", "unaccept", "snooze", "unsnooze"] {
        let res = app
            .post(&action(&site, id, what), "days=7", Some(&other))
            .await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{what}");
    }
    // An unknown incident on one's own site is not found either.
    let (app, site, cookie, _) = blocked_incident().await;
    for what in ["recheck", "accept", "unaccept", "snooze", "unsnooze"] {
        let res = app
            .post(&action(&site, 999_999, what), "days=7", Some(&cookie))
            .await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{what}");
    }
    assert_eq!(count(&app, "SELECT count(*) FROM ai_rechecks").await, 0);
}

#[tokio::test]
async fn incident_refusals_have_their_own_headings() {
    let (app, site, cookie, id) = blocked_incident().await;
    // An unknown issue on one's own site: not a missing page.
    let res = app
        .post_hx(&action(&site, 999_999, "snooze"), "days=7", Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert!(res.body.contains("Issue not found"), "{}", res.body);
    assert!(!res.body.contains("Page not found"), "{}", res.body);
    assert!(!res.body.contains("That page doesn"), "{}", res.body);

    // A refusal that is no crawl or job in progress.
    sqlx::query("UPDATE ai_incidents SET resolved_at = now(), resolution = 'fixed' WHERE id = $1")
        .bind(id)
        .execute(app.pool())
        .await
        .unwrap();
    let res = app
        .post_hx(&action(&site, id, "snooze"), "days=7", Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert!(
        res.body.contains("Can&#39;t do that right now")
            || res.body.contains("Can&#x27;t do that right now"),
        "{}",
        res.body
    );
    assert!(!res.body.contains("Already in progress"), "{}", res.body);
}

#[tokio::test]
async fn copy_payloads_escape_hostile_site_strings() {
    let (app, site, cookie, id) = llms_incident().await;
    // Evidence a hostile site could put words in: its pages' paths.
    sqlx::query(
        "UPDATE ai_incidents SET evidence = jsonb_set(evidence, '{urls}', \
         '{\"sample\": [\"https://example.com/\\\"><img src=x onerror=alert(1)>\"], \"total\": 1}') \
         WHERE id = $1",
    )
    .bind(id)
    .execute(app.pool())
    .await
    .unwrap();
    let body = ai_access_body(&app, &site, &cookie).await;
    let c = card(&body, id);
    assert!(c.contains("Copy prompt"), "{c}");
    assert!(!body.contains("<img src=x"), "{c}");
    assert!(!body.contains(r#""><img src=x"#), "{c}");
    // The copy payloads carry it, escaped in their attribute.
    assert!(
        c.contains("&#34;&#62;&#60;img src=x onerror=alert(1)&#62;"),
        "{c}"
    );
}

#[tokio::test]
async fn a_fetch_blocked_card_names_the_probe_identity() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_probes(&site, pages(), (200, OPEN), &|r| {
        probes_with(r, &|_, _| None)
    })
    .await;
    app.finished_crawl_with_probes(&site, pages(), (200, OPEN), &|r| probes_with(r, &waf))
        .await;
    let i = geo::open_incidents(app.pool(), site.id)
        .await
        .unwrap()
        .into_iter()
        .find(|i| i.kind == codoseo_geo::findings::FindingKind::FetchBlocked)
        .expect("a fetch_blocked incident");
    let body = ai_access_body(&app, &site, &cookie).await;
    let c = card(&body, i.id);
    let ua = registry()
        .bot("OAI-SearchBot")
        .unwrap()
        .user_agent
        .clone()
        .unwrap();
    assert!(
        c.contains(&ua.replace('/', "&#x2f;")) || c.contains(&ua),
        "{c}"
    );
    assert!(c.contains("From: hello@codoseo.com"), "{c}");
    assert!(c.contains("sent from CodoSEO's address"), "{c}");
    assert!(c.contains(">Accept<"), "{c}");
}

// ---- the site audit's fix list and the exports (B3.14) ----------------------------------------

async fn audit_body(app: &TestApp, site: &Site, cookie: &str) -> String {
    app.get(&format!("/s/{}/audit", site.id), Some(cookie))
        .await
        .body
}

/// The fix list's row of incident `id`, if it is listed.
fn fix_row(body: &str, id: i64) -> Option<String> {
    let at = body.find(&format!(r#"id="fx-ai-{id}""#))?;
    let end = body[at..].find("Copy fix prompt").unwrap();
    Some(body[at..at + end].to_owned())
}

/// A score card's markup, by its key.
fn score_card(body: &str, key: &str) -> String {
    let at = body
        .find(&format!("sc sc-{key} "))
        .unwrap_or_else(|| panic!("the {key} card"));
    let rest = &body[at + 1..];
    let end = ["card sc sc-", "id=\"help-overall\""]
        .iter()
        .filter_map(|m| rest.find(m))
        .min()
        .unwrap();
    rest[..end].to_owned()
}

#[tokio::test]
async fn an_ai_issue_is_ranked_in_the_fix_list_with_its_geo_points() {
    let (app, site, cookie, id) = blocked_incident().await;
    let body = audit_body(&app, &site, &cookie).await;
    let row = fix_row(&body, id).expect("the AI issue in the fix list");
    assert!(row.contains(r#"data-kind="ai""#), "{row}");
    assert!(row.contains(r#"data-sev="critical""#), "{row}");
    // Not there on the crawl before: new this crawl. It costs GEO points.
    assert!(row.contains("New this crawl"), "{row}");
    assert!(row.contains(" GEO</span>"), "{row}");
    // Mark intended returns to the audit; the fix and its prompt sit behind the "?".
    assert!(
        row.contains(&format!(
            r#"action="/s/{}/ai-access/incidents/{id}/intended?back=audit""#,
            site.id
        )),
        "{row}"
    );
    assert!(row.contains(r#"popovertarget="fx-ai-"#), "{row}");
    assert!(
        body.contains("User-agent: OAI-SearchBot"),
        "the fix: {body}"
    );
    // The cards read the crawl's own scores.
    let (seo, geo, overall): (i16, i16, i16) = sqlx::query_as(
        "SELECT health_score, geo_score, overall_score FROM crawls WHERE site_id = $1 \
         ORDER BY finished_at DESC LIMIT 1",
    )
    .bind(site.id)
    .fetch_one(app.pool())
    .await
    .unwrap();
    assert!(score_card(&body, "seo").contains(&format!(r#"data-count="{seo}""#)));
    assert!(score_card(&body, "geo").contains(&format!(r#"data-count="{geo}""#)));
    assert!(score_card(&body, "overall").contains(&format!(r#"data-count="{overall}""#)));
    assert!(geo < 100, "the blocked search bot costs GEO points");
    // The SEO score is what it is without the incident: both crawls hold the same pages.
    let scores: Vec<i16> = sqlx::query_scalar(
        "SELECT health_score FROM crawls WHERE site_id = $1 ORDER BY created_at",
    )
    .bind(site.id)
    .fetch_all(app.pool())
    .await
    .unwrap();
    assert_eq!(scores.len(), 2);
    assert_eq!(scores[0], scores[1]);
    // The trend has two crawls to draw, the GEO line among them.
    assert!(body.contains("tr-line tr-geo"), "{body}");
}

#[tokio::test]
async fn accepted_and_snoozed_issues_leave_the_fix_list() {
    let (app, site, cookie, id) = llms_incident().await;
    let body = audit_body(&app, &site, &cookie).await;
    let row = fix_row(&body, id).expect("listed");
    assert!(row.contains(r#"data-sev="warning""#), "{row}");
    // An llms.txt that is unavailable is this incident, not a second "missing" row.
    assert!(!body.contains(r#"id="fx-ai-llms""#), "{body}");

    let res = app
        .post_hx(&action(&site, id, "accept"), "", Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(fix_row(&audit_body(&app, &site, &cookie).await, id).is_none());

    app.post_hx(&action(&site, id, "unaccept"), "", Some(&cookie))
        .await;
    app.post_hx(&action(&site, id, "snooze"), "days=7", Some(&cookie))
        .await;
    assert!(fix_row(&audit_body(&app, &site, &cookie).await, id).is_none());

    app.post_hx(&action(&site, id, "unsnooze"), "", Some(&cookie))
        .await;
    assert!(fix_row(&audit_body(&app, &site, &cookie).await, id).is_some());
}

#[tokio::test]
async fn a_missing_llms_txt_is_a_notice_in_the_fix_list() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_llms(&site, pages(), (200, OPEN), llms_fetch(404, ""), false)
        .await;
    let body = audit_body(&app, &site, &cookie).await;
    let at = body.find(r#"id="fx-ai-llms""#).expect("the llms.txt row");
    let row = &body[at..at + body[at..].find("Copy fix prompt").unwrap()];
    assert!(row.contains(r#"data-sev="notice""#), "{row}");
    assert!(row.contains("llms.txt is missing"), "{row}");
    assert!(row.contains(" GEO</span>"), "{row}");
    // The first crawl: nothing is new yet.
    assert!(!body.contains("New this crawl"), "{body}");
}

fn csv_rows(body: &str) -> Vec<Vec<String>> {
    let mut r = csv::Reader::from_reader(body.as_bytes());
    let mut rows = vec![r.headers().unwrap().iter().map(str::to_owned).collect()];
    rows.extend(
        r.records()
            .map(|rec| rec.unwrap().iter().map(str::to_owned).collect::<Vec<_>>()),
    );
    rows
}

#[tokio::test]
async fn the_incident_csv_lists_open_and_recently_resolved_in_column_order_and_neutralises_formulas()
 {
    let (app, site, cookie, id) = blocked_incident().await;
    // Strings from the site can start like a formula; the file must not run them.
    sqlx::query("UPDATE ai_incidents SET title = '=HYPERLINK(\"http://evil.example\",\"x\")', subject = '@cmd' WHERE id = $1")
        .bind(id)
        .execute(app.pool())
        .await
        .unwrap();
    let ins = |kind: &'static str, title: &'static str, days: i32| {
        let (pool, site) = (app.pool().clone(), site.id);
        async move {
            sqlx::query(
                "INSERT INTO ai_incidents (site_id, kind, subject, severity, title, summary, evidence, opened_at, last_seen_at, resolved_at, resolution, resolved_by) \
                 SELECT site_id, $2, subject, severity, $3, summary, evidence, now() - interval '200 days', now() - ($4 || ' days')::interval, now() - ($4 || ' days')::interval, 'fixed', 'crawl' \
                 FROM ai_incidents WHERE site_id = $1 LIMIT 1",
            )
            .bind(site)
            .bind(kind)
            .bind(title)
            .bind(days.to_string())
            .execute(&pool)
            .await
            .unwrap();
        }
    };
    ins("llms_txt_unavailable", "Resolved ten days ago", 10).await;
    ins("llms_txt_unavailable", "Resolved a hundred days ago", 100).await;

    let res = app
        .get(
            &format!("/s/{}/ai-access/incidents.csv", site.id),
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.header("content-type"), Some("text/csv; charset=utf-8"));
    assert!(
        res.header("content-disposition").is_some_and(
            |d| d.starts_with("attachment; filename=\"example.com-ai-access-incidents-")
        ),
        "{:?}",
        res.header("content-disposition")
    );
    let rows = csv_rows(&res.body);
    assert_eq!(
        rows[0],
        [
            "kind",
            "subject",
            "severity",
            "grade",
            "title",
            "state",
            "first_seen",
            "last_seen",
            "resolved_at",
            "resolved_by",
            "verified_at",
            "affected_urls",
            "sources"
        ]
    );
    assert_eq!(
        rows.len(),
        3,
        "open + resolved in 90 days, not older: {rows:?}"
    );
    // Open first.
    let open = &rows[1];
    assert_eq!(open[0], "bots_blocked");
    assert_eq!(open[1], "'@cmd");
    assert_eq!(open[2], "critical");
    assert_eq!(open[3], "A");
    assert_eq!(open[4], "'=HYPERLINK(\"http://evil.example\",\"x\")");
    assert_eq!(open[5], "open");
    assert!(
        open[6].starts_with("20") && open[6].contains('T'),
        "{open:?}"
    );
    assert_eq!(open[8], "");
    assert!(open[11].contains(&format!("https://{DOMAIN}/")), "{open:?}");
    assert!(open[12].contains("developers.openai.com"), "{open:?}");
    let done = &rows[2];
    assert_eq!(done[4], "Resolved ten days ago");
    assert_eq!(done[5], "resolved");
    assert_eq!(done[9], "crawl");
    assert!(done[8].starts_with("20"), "{done:?}");
    // No cell starts like a formula.
    for row in &rows[1..] {
        for cell in row {
            assert!(!cell.starts_with(['=', '+', '-', '@']), "{cell:?}");
        }
    }
}

#[tokio::test]
async fn the_incident_json_is_the_same_rows_with_lists_as_arrays() {
    let (app, site, cookie, id) = blocked_incident().await;
    let res = app
        .get(
            &format!("/s/{}/ai-access/incidents.json", site.id),
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.header("content-type"), Some("application/json"));
    let v: serde_json::Value = serde_json::from_str(&res.body).unwrap();
    let rows = v.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    let keys: Vec<&str> = rows[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    let mut want = codoseo_web::ai_incident::EXPORT_COLUMNS.to_vec();
    want.sort();
    assert_eq!(sorted, want);
    assert_eq!(rows[0]["kind"], "bots_blocked");
    assert_eq!(rows[0]["state"], "open");
    assert_eq!(rows[0]["resolved_at"], serde_json::Value::Null);
    assert!(rows[0]["affected_urls"].is_array());
    assert!(rows[0]["sources"].is_array());
    let _ = id;
}

#[tokio::test]
async fn the_incident_exports_are_the_signed_in_owners_only() {
    let (app, site, cookie, _) = blocked_incident().await;
    let (_, other) = app.login("other@example.com").await;
    for ext in ["csv", "json"] {
        let path = format!("/s/{}/ai-access/incidents.{ext}", site.id);
        assert_eq!(
            app.get(&path, Some(&other)).await.status,
            StatusCode::NOT_FOUND,
            "{ext}"
        );
        assert_eq!(
            app.get(&path, None).await.status,
            StatusCode::SEE_OTHER,
            "{ext}"
        );
        assert_eq!(app.get(&path, Some(&cookie)).await.status, StatusCode::OK);
    }
    // The AI access page links to both.
    let body = app
        .get(&format!("/s/{}/ai-access", site.id), Some(&cookie))
        .await
        .body;
    assert!(body.contains(&format!("/s/{}/ai-access/incidents.csv", site.id)));
    assert!(body.contains(&format!("/s/{}/ai-access/incidents.json", site.id)));
}

// ---- the site audit's AI bots table -----------------------------------------------------------

/// The AI bots table's row of one product, up to its closing `</tr>`.
fn product_row(body: &str, name: &str) -> String {
    let at = body
        .find(&format!("<b>{name}</b>"))
        .unwrap_or_else(|| panic!("the {name} row"));
    body[at..at + body[at..].find("</tr>").unwrap()].to_owned()
}

#[tokio::test]
async fn the_ai_bots_table_lists_every_product_and_the_fetch_problems() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_probes(&site, pages(), (200, OPEN), &|r| probes_with(r, &waf))
        .await;
    let body = audit_body(&app, &site, &cookie).await;
    let start = body
        .find(r#"aria-labelledby="bt-h""#)
        .expect("the AI bots card");
    let table = &body[start..];
    for e in codoseo_geo::eligibility::engines() {
        assert!(
            table.contains(&format!(">{}</td>", e.crawler)),
            "{}: {table}",
            e.crawler
        );
    }
    assert!(table.contains("Training crawlers"), "{table}");
    // OpenAI's search bot is challenged by the WAF: ChatGPT reaches the pages only partly,
    // and the reason opens on tap or focus.
    let chatgpt = product_row(table, "ChatGPT");
    assert!(chatgpt.contains("Partly"), "{chatgpt}");
    assert!(chatgpt.contains("Cloudflare challenge on"), "{chatgpt}");
    assert!(
        chatgpt.contains(r#"popovertarget="bt-chatgpt""#),
        "{chatgpt}"
    );
    assert!(chatgpt.contains("treated worse"), "{chatgpt}");
    let claude = product_row(table, "Claude");
    assert!(claude.contains("Allowed"), "{claude}");
    assert!(claude.contains("All 3 key pages"), "{claude}");
    // Logos beside the names, and the head counts who can reach the pages.
    assert!(table.contains("/assets/logo-openai."), "{table}");
    assert!(table.contains("can reach your pages"), "{table}");
    assert!(
        table.contains(&format!(r#"href="/s/{}/ai-access""#, site.id)),
        "{table}"
    );
    // The open fetch incident is in the fix list.
    let id = geo::open_incidents(app.pool(), site.id).await.unwrap()[0].id;
    assert!(fix_row(&body, id).is_some(), "{body}");
    // The AI products card agrees with the table.
    let card = score_card(&body, "ai");
    assert!(card.contains("partly"), "{card}");
}

#[tokio::test]
async fn a_blocked_search_bot_reads_blocked_and_mark_intended_returns_to_the_audit() {
    let (app, site, cookie, id) = blocked_incident().await;
    let body = audit_body(&app, &site, &cookie).await;
    let row = product_row(&body, "ChatGPT");
    assert!(row.contains(r#"<span class="badge t-err">"#), "{row}");
    assert!(row.contains("Blocked on all 3 key pages"), "{row}");

    // Marked intended from the fix list: the audit comes back, the issue leaves the list and
    // the block is the owner's choice, named in the footer.
    let res = app
        .post_hx(
            &format!("{}?back=audit", action(&site, id, "intended")),
            "",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(
        res.header("hx-replace-url"),
        Some(format!("/s/{}/audit", site.id).as_str())
    );
    assert!(res.body.contains("Fix these, in order"), "{}", res.body);
    assert!(fix_row(&res.body, id).is_none(), "{}", res.body);
    let row = product_row(&res.body, "ChatGPT");
    assert!(row.contains("Blocked by you"), "{row}");
    assert!(
        res.body
            .contains("marked intended, not counted:</span> <b>OAI-SearchBot</b>"),
        "{}",
        res.body
    );
    // A plain form post is redirected there.
    let (app, site, cookie, id) = blocked_incident().await;
    let res = app
        .post(
            &format!("{}?back=audit", action(&site, id, "intended")),
            "",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(
        res.location(),
        Some(format!("/s/{}/audit", site.id).as_str())
    );
}

#[tokio::test]
async fn operator_logos_sit_beside_their_bots() {
    let (app, site, cookie, id) = blocked_incident().await;
    let body = ai_access_body(&app, &site, &cookie).await;
    let openai = codoseo_web::operator_logo::logo("OpenAI").unwrap();
    let tile = |size: &str, src: &str| {
        format!(
            r#"<span class="op-logo op-logo-{size}" aria-hidden="true"><img src="{src}" alt="" width="14" height="14" loading="lazy" decoding="async"></span>"#
        )
    };
    // The bots table: a tile, then the token and the operator's name as text.
    assert!(
        body.contains(&format!(
            r#"<span class="aia-op">{}<span class="aia-op-text"><span class="aia-token">GPTBot</span>"#,
            tile("sm", openai)
        )),
        "{body}"
    );
    // Every operator with a logo shows it; Common Crawl, without one, shows its initials.
    for b in &registry().bots {
        if let Some(src) = codoseo_web::operator_logo::logo(&b.operator) {
            assert!(body.contains(&tile("sm", src)), "{}", b.operator);
        }
    }
    assert!(body.contains(
        r#"<span class="op-logo op-logo-sm" aria-hidden="true"><b>CC</b></span><span class="aia-op-text"><span class="aia-token">CCBot</span>"#
    ));
    // The issue names its bot with the operator's tile.
    let c = card(&body, id);
    assert!(
        c.contains(&format!(
            r#"<span class="aia-bot">{}OAI-SearchBot"#,
            tile("xs", openai)
        )),
        "{c}"
    );
    // Rows fade in one after another; each bot's status is keyed for the swap pulse.
    assert!(body.contains(r#"<tr class="aia-row aia-brow" data-bot="#));
    assert!(body.contains(r#"style="--i:0">"#));
    assert!(body.contains(r#"data-state-key="bot-GPTBot" data-state="#));
    // The answer engines take their crawler's operator.
    let google = codoseo_web::operator_logo::logo("Google").unwrap();
    assert!(body.contains(&format!(
        r#"{}<span class="aia-op-text"><span class="aia-engine">Google Search"#,
        tile("sm", google)
    )));

    // The intent form shows the same tiles.
    let intent = app
        .get(&format!("/s/{}/ai-access/intent", site.id), Some(&cookie))
        .await
        .body;
    assert!(intent.contains(&tile("sm", openai)), "{intent}");
    assert!(intent.contains(r#"<b>CC</b>"#), "{intent}");
}

// ---- Content Signals and Markdown: advisory checks ---------------------------------------------

/// robots.txt with a Content-Signal line whose only value can't be read.
const BAD_SIGNALS: &str = "User-agent: *\nContent-Signal: ai-train=maybe\nAllow: /\n";

fn markdown_probe(
    status: u16,
    content_type: &str,
    body_bytes: u64,
) -> codoseo_core::output::MarkdownProbe {
    codoseo_core::output::MarkdownProbe {
        negotiated: codoseo_core::output::MarkdownFetch {
            url: format!("https://{DOMAIN}/"),
            status,
            error: None,
            content_type: Some(content_type.to_owned()),
            body_bytes,
        },
        ..Default::default()
    }
}

#[tokio::test]
async fn content_signals_and_markdown_are_advisory_issues_with_fixes() {
    use codoseo_geo::findings::FindingKind;

    let (app, site, cookie) = setup().await;
    // Both pass on the first crawl.
    app.finished_crawl_with_markdown(
        &site,
        pages(),
        (200, OPEN),
        markdown_probe(200, "text/markdown", 120),
    )
    .await;
    assert!(
        geo::open_incidents(app.pool(), site.id)
            .await
            .unwrap()
            .is_empty()
    );
    // Then robots.txt loses its valid signal and the home page answers HTML only.
    app.finished_crawl_with_markdown(
        &site,
        pages(),
        (200, BAD_SIGNALS),
        markdown_probe(200, "text/html; charset=utf-8", 5000),
    )
    .await;
    let open = geo::open_incidents(app.pool(), site.id).await.unwrap();
    let of = |k: FindingKind| open.iter().find(|i| i.kind == k).expect("open").clone();
    let (signals, markdown) = (
        of(FindingKind::ContentSignalsMissing),
        of(FindingKind::MarkdownUnavailable),
    );
    // Advisory: Notices, opened quietly although an earlier crawl judged them, never alerted.
    for i in [&signals, &markdown] {
        assert_eq!(i.severity, Severity::Notice);
        assert!(i.quiet, "{:?}", i.kind);
    }
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM changes WHERE kind::text LIKE 'ai_%' \
             AND kind::text <> 'ai_preferences_changed'"
        )
        .await,
        0
    );

    // The AI access screen: the card, the unreadable value and what the Markdown request got.
    let body = ai_access_body(&app, &site, &cookie).await;
    assert!(body.contains("Content Signals and Markdown"), "{body}");
    assert!(
        body.contains("ai-train=maybe (the value must be yes or no)"),
        "{body}"
    );
    assert!(
        body.contains("HTTP 200 text&#x2f;html") || body.contains("HTTP 200 text/html"),
        "{body}"
    );
    assert!(body.contains("Advisory"), "{body}");

    // The fix list: both Notices, new this crawl, with their fix and prompt.
    let audit = audit_body(&app, &site, &cookie).await;
    for i in [&signals, &markdown] {
        let row = fix_row(&audit, i.id).expect("in the fix list");
        assert!(row.contains(r#"data-sev="notice""#), "{row}");
        assert!(row.contains("New this crawl"), "{row}");
        assert!(row.contains(" GEO</span>"), "{row}");
    }
    assert!(
        audit.contains("Content-Signal: search=yes, ai-input=yes, ai-train=no"),
        "{audit}"
    );
    assert!(audit.contains("Markdown for Agents"), "the prompt: {audit}");

    // Fixed: both resolve, quietly, and the footer says so.
    app.finished_crawl_with_markdown(
        &site,
        pages(),
        (200, OPEN),
        markdown_probe(200, "text/markdown", 120),
    )
    .await;
    assert!(
        geo::open_incidents(app.pool(), site.id)
            .await
            .unwrap()
            .is_empty()
    );
    let audit = audit_body(&app, &site, &cookie).await;
    assert!(audit.contains("fixed since last crawl"), "{audit}");
    assert!(
        audit.contains("robots.txt declares Content Signals"),
        "{audit}"
    );
    assert!(audit.contains("Markdown for AI agents"), "{audit}");
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM changes WHERE kind::text = 'ai_issue_resolved'"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn the_robots_tab_shows_the_file_and_tests_a_bot_against_it() {
    let (app, site, cookie) = setup().await;
    let robots = "# robots.txt\nUser-agent: *\nDisallow: /docs/\nAllow: /\n\nUser-agent: GPTBot\nUser-agent: CCBot\nDisallow: /\n\nSitemap: https://example.com/sitemap.xml\n";
    app.finished_crawl_with_robots(&site, pages(), (200, robots), Vec::new())
        .await;
    let body = tab_body(&app, &site, &cookie, "/robots").await;
    let total = registry().bots.len();
    for needle in [
        r#"<span class="badge t-ok">Fetched · HTTP 200</span>"#,
        "https://example.com/robots.txt",
        &format!("{} bytes", robots.len()),
        "Unchanged since crawl #1",
        r#"<div class="aia-rl is-comment"><span class="ln">1</span><span class="tx"># robots.txt</span><span class="nt"></span></div>"#,
        &format!(
            r#"<span class="ln">2</span><span class="tx">User-agent: *</span><span class="nt">{} bots</span>"#,
            total - 2
        ),
        r#"<span class="ln">6</span><span class="tx">User-agent: GPTBot</span><span class="nt">2 bots</span>"#,
        ">Test a bot against this file</h2>",
        ">Groups in this file</h2>",
        "lines 2–4 · 2 rules",
        "GPTBot, CCBot</span> — lines 6–8 · Disallow: / · 2 registry bots",
        "No Crawl-delay · no Content-Signal line · 1 sitemap",
    ] {
        assert!(body.contains(needle), "missing {needle:?} in {body}");
    }
    // A chip per likely bot; GPTBot's own group comes first and is tested on /.
    assert!(body.contains(&format!(
        r#"<a class="chip aia-tchip is-on" href="/s/{}/ai-access/robots?bot=GPTBot&#38;path=%2F" aria-current="true">GPTBot</a>"#,
        site.id
    )), "{body}");
    assert!(body.contains("GPTBot can&#39;t fetch /"));

    // Allowed: no group names it, the * group decides.
    let path = |q: &str| format!("/s/{}/ai-access/robots/test?{q}", site.id);
    let res = app
        .get_hx(&path("bot=OAI-SearchBot&path=/pricing"), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(
        res.body
            .trim_start()
            .starts_with(r#"<div class="aia-robots-tab" id="aia-robots-tab">"#),
        "{}",
        res.body
    );
    assert!(!res.body.contains("<html"));
    assert!(res.body.contains("OAI-SearchBot can fetch /pricing"));
    assert!(
        res.body.contains(
            "No group names it, so User-agent: * (line 2) applies; line 4 “Allow: /” matches."
        ),
        "{}",
        res.body
    );
    assert!(
        res.body
            .contains(r#"<div class="aia-rl is-hit"><span class="ln">4</span>"#)
    );
    assert!(
        res.body
            .contains(r#"<div class="aia-rl is-in"><span class="ln">3</span>"#)
    );
    // Disallowed: the longest match wins.
    let res = app
        .get_hx(&path("bot=OAI-SearchBot&path=/docs/start"), Some(&cookie))
        .await;
    assert!(
        res.body
            .contains("OAI-SearchBot can&#39;t fetch /docs/start")
    );
    assert!(res.body.contains("line 3 “Disallow: /docs/” matches."));
    assert!(
        res.body
            .contains(r#"<span class="nt">decides /docs/start</span>"#)
    );
    // A bot the registry doesn't list is tested as written.
    let res = app.get_hx(&path("bot=FooBot&path=/a"), Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("FooBot can fetch /a"));
    assert!(
        res.body
            .contains("It isn&#39;t in the registry: tested as the token written.")
    );
    assert!(
        res.body
            .contains(r#"<option value="FooBot" selected>FooBot</option>"#)
    );
    // Refused input is a 400 that says why, and nothing is tested.
    for (q, why) in [
        ("bot=GPTBot&path=docs", "The path must start with /."),
        (
            "bot=GPTBot&path=/a%20b",
            "The path can&#39;t contain spaces or control characters.",
        ),
        (
            "bot=%3Cscript%3E&path=/",
            "A bot is a product token: letters, digits, - and _ only.",
        ),
    ] {
        let res = app.get_hx(&path(q), Some(&cookie)).await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{q}");
        assert!(res.body.contains(why), "{q}: {}", res.body);
        assert!(!res.body.contains("<script>"), "{q}");
        assert!(!res.body.contains("can fetch"), "{q}");
    }
    let long = format!("bot=GPTBot&path=/{}", "a".repeat(1100));
    assert_eq!(
        app.get_hx(&path(&long), Some(&cookie)).await.status,
        StatusCode::BAD_REQUEST
    );
    // Without JavaScript the tester is the whole tab.
    let res = app
        .get(&path("bot=GPTBot&path=/pricing"), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("<html"));
    assert!(res.body.contains("GPTBot can&#39;t fetch /pricing"));
    // Another account can't test this site's file.
    let (_, other) = app.login("bo@example.com").await;
    assert_eq!(
        app.get(&path("bot=GPTBot&path=/"), Some(&other))
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // A new file: changed in that crawl.
    app.finished_crawl_with_robots(&site, pages(), (200, OPEN), Vec::new())
        .await;
    let body = tab_body(&app, &site, &cookie, "/robots").await;
    assert!(body.contains("Last changed: crawl #2"), "{body}");
}

#[tokio::test]
async fn a_report_without_the_robots_body_says_it_appears_after_the_next_crawl() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_robots(&site, pages(), (200, BLOCK_OAI), Vec::new())
        .await;
    // A report stored before the body was kept.
    sqlx::query("UPDATE ai_reports SET report = report #- '{robots,file}' WHERE site_id = $1")
        .bind(site.id)
        .execute(app.pool())
        .await
        .unwrap();
    let body = tab_body(&app, &site, &cookie, "/robots").await;
    assert!(body.contains("Fetched · HTTP 200"));
    assert!(body.contains("The file&#39;s text wasn&#39;t stored for this check; it appears here after the next crawl."), "{body}");
    assert!(body.contains("so it can&#39;t be tested yet"));
    assert!(!body.contains("aia-rl"));
    assert!(!body.contains("Unchanged since"));
    // The Bots tab still has its verdicts, without the excerpt.
    let bots = tab_body(&app, &site, &cookie, "/bots").await;
    assert!(bots.contains("No — a group names OAI-SearchBot"));
    assert!(bots.contains(
        "The file&#39;s text wasn&#39;t stored for this check. It appears after the next crawl."
    ));
    // A missing robots.txt: nothing to show, everything allowed.
    app.finished_crawl_with_robots(&site, pages(), (404, ""), Vec::new())
        .await;
    let body = tab_body(&app, &site, &cookie, "/robots").await;
    assert!(body.contains("None · HTTP 404"), "{body}");
    assert!(body.contains("There is no robots.txt, so every path is allowed."));
}
