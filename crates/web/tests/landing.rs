//! The cloud landing page: written for the person responsible for rankings, with every number
//! (plan lines, prices, the size of the free audit) rendered from the code, and only on the cloud.

mod support;

use axum::http::StatusCode;
use codoseo_core::plan::{Plan, PlanLimits};
use codoseo_web::routes::landing::{monthly_price, plan_features};
use support::{TestApp, cloud_config, cloud_config_with};

#[tokio::test]
async fn the_cloud_landing_renders_the_plans_from_the_code() {
    let app = TestApp::with_config(cloud_config()).await;
    let res = app.get("/", None).await;
    assert_eq!(res.status, StatusCode::OK);
    let body = res.body;

    // The hero, the free audit box and its size.
    assert!(body.contains("Find out your site broke"), "{body}");
    assert!(
        body.contains(r#"<form class="audit-form" method="post" action="/audit">"#),
        "{body}"
    );
    let quick = PlanLimits::quick_audit().max_pages.unwrap_or(0);
    assert!(
        body.contains(&format!("Free · {quick} pages · takes about a minute.")),
        "{body}"
    );

    // Prices and every plan line come from the plan limits, so the page can't drift.
    let lite = monthly_price(Plan::Lite).unwrap();
    let pro = monthly_price(Plan::Pro).unwrap();
    assert!(
        body.contains(&format!("<b>${lite}</b><span>a month</span>")),
        "{body}"
    );
    assert!(
        body.contains(&format!("<b>${pro}</b><span>a month</span>")),
        "{body}"
    );
    assert!(body.contains(&format!("Watch every site you manage from ${lite}.")));
    for plan in [Plan::Free, Plan::Lite, Plan::Pro] {
        for line in plan_features(plan) {
            assert!(body.contains(&format!("<span>{line}</span>")), "{line}");
        }
    }
    assert!(body.contains(">Self-host guide</a>"));
    // The old hosted-vs-self-hosted price table is gone from the page.
    assert!(!body.contains(r#"id="self-host""#), "{body}");
    // The FAQ JSON-LD carries the visible prices.
    assert!(body.contains(&format!(
        "Lite at ${lite} a month and Pro at ${pro} a month"
    )));
    // Every check category and its highlights render from the route's table.
    assert!(
        body.contains(r#"<div class="cat"><b>Response</b>"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<li class="r">4xx and 5xx errors</li>"#),
        "{body}"
    );
    // The comparison and the developer block are present.
    assert!(body.contains("Screaming Frog audits. CodoSEO watches."));
    assert!(body.contains("For developers and AI agents"));
}

#[tokio::test]
async fn self_hosted_installs_have_no_landing_comparison() {
    let app = TestApp::new().await;
    // Signed out, `/` is the sign-in page there, never the landing.
    let res = app.get("/", None).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    let login = app.get("/login", None).await.body;
    assert!(!login.contains("self-host"), "{login}");
}

#[tokio::test]
async fn a_signed_in_visitor_sees_the_landing_with_a_way_into_the_app() {
    let app = TestApp::with_config(cloud_config()).await;
    let (_, cookie) = app.login("ana@example.com").await;
    let res = app.get("/", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(
        res.body
            .contains(r#"<a class="ghost" href="/app">Open app</a>"#)
    );
    assert!(!res.body.contains(r#"href="/login""#), "{}", res.body);

    // The app's home moved to `/app`: no site yet, so onboarding.
    let res = app.get("/app", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.location(), Some("/sites"));
}

#[tokio::test]
async fn the_turnstile_widget_sits_inside_each_audit_form() {
    let app = TestApp::with_config(cloud_config_with(&[
        ("TURNSTILE_SITE_KEY", "site-key-123"),
        ("TURNSTILE_SECRET", "shh-secret"),
    ]))
    .await;
    let body = app.get("/", None).await.body;
    // Cloudflare injects the hidden token input inside the widget's div, so the widget has
    // to be a descendant of the form or the token is never submitted.
    let mut rest = body.as_str();
    let mut forms = 0;
    while let Some(start) = rest.find(r#"<form class="audit-form"#) {
        let end = rest[start..].find("</form>").expect("form closes") + start;
        assert!(
            rest[start..end].contains(r#"class="cf-turnstile ts" data-sitekey="site-key-123""#),
            "{}",
            &rest[start..end]
        );
        forms += 1;
        rest = &rest[end..];
    }
    assert_eq!(forms, 2, "{body}");
}
