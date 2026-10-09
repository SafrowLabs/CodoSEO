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

    let res = app
        .get(&format!("/s/{}/ai-access", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let body = &res.body;
    for needle in [
        "<h1>AI access</h1>",
        "Can AI assistants and search engines reach, read and quote your site?",
        "crawl #2",
        "Edit intent",
        // Tiles.
        "AI search bots allowed",
        "Training bots blocked",
        "Quotable in Google AI",
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
    let body = app
        .get(&format!("/s/{}/ai-access", site.id), Some(&cookie))
        .await
        .body;
    assert!(body.contains("robots.txt answered HTTP 503"), "{body}");
    assert!(body.contains("GET /robots.txt → HTTP 503"));
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
    // The page now lists it as resolved, and the bot row shows the override.
    assert!(res.body.contains("No AI access issues"), "{}", res.body);
    assert!(res.body.contains(">intended<"));
    assert!(res.body.contains(">override<"));

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
    let body = app
        .get(&format!("/s/{}/ai-access", site.id), Some(&cookie))
        .await
        .body;
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
async fn the_bots_table_opens_on_what_needs_attention() {
    let (app, site, cookie) = setup().await;
    app.finished_crawl_with_robots(&site, pages(), (200, OPEN), Vec::new())
        .await;
    let body = app
        .get(&format!("/s/{}/ai-access", site.id), Some(&cookie))
        .await
        .body;
    // Nothing conflicts: every bot shows, and "Needs attention" can't be picked.
    let total = registry().bots.len();
    assert!(
        body.contains(
            r#"id="aia-f-attn" value="attention" aria-controls="aia-bots-table" disabled>"#
        ),
        "{body}"
    );
    assert!(body.contains(r#"id="aia-f-all" value="all" aria-controls="aia-bots-table" checked>"#));
    assert!(body.contains(&format!(r#"All bots<span class="n">{total}</span>"#)));
    assert!(!body.contains(r#"class="is-attn"#));

    app.finished_crawl_with_robots(&site, pages(), (200, BLOCK_OAI), Vec::new())
        .await;
    let body = app
        .get(&format!("/s/{}/ai-access", site.id), Some(&cookie))
        .await
        .body;
    assert!(
        body.contains(
            r#"id="aia-f-attn" value="attention" aria-controls="aia-bots-table" checked>"#
        )
    );
    assert!(body.contains(r#"Needs attention<span class="n">1</span>"#));
    assert!(body.contains(r#"<tr class="is-attn is-attn-last">"#));
    assert_eq!(body.matches(r#"<tr class="is-attn"#).count(), 1);
    assert_eq!(body.matches(r#"<tr class="is-ok">"#).count(), total - 1);
    // The group says how many conflict; the others say none do (shown while filtered).
    assert!(body.contains(r#"<span class="aia-group-flag">1 conflict</span>"#));
    assert!(body.contains(r#"<tbody class="is-clear">"#));
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
    let open = geo::open_incidents(app.pool(), site.id).await.unwrap();
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
            .is_empty()
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
    // The matrix still shows it.
    assert!(res.body.contains("nosnippet ×3"), "{}", res.body);

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
    let body = app
        .get(&format!("/s/{}/ai-access", site.id), Some(&cookie))
        .await
        .body;
    // The bots table: Applebot reads Googlebot's group.
    assert!(
        body.contains("Allowed · follows Googlebot&#39;s rules (line 2)"),
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
