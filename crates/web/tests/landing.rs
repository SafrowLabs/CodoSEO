//! The cloud landing's hosted vs self-hosted comparison: it renders from the price table in
//! `routes::landing`, every price with its source and date, and only on the cloud.

mod support;

use axum::http::StatusCode;
use codoseo_web::routes::landing::{OTHER_COSTS, PRICES_AS_OF, VPS_OFFERS};
use support::{TestApp, cloud_config};

#[tokio::test]
async fn the_cloud_landing_compares_hosted_and_self_hosted_costs() {
    let app = TestApp::with_config(cloud_config()).await;
    let res = app.get("/", None).await;
    assert_eq!(res.status, StatusCode::OK);
    let body = res.body;

    assert!(
        body.contains(r#"<div class="sh mt" id="self-host">"#),
        "{body}"
    );
    assert!(body.contains("Lite $9 · Pro $19"), "{body}");
    assert!(
        body.contains("A server for 5 sites × 5,000 pages costs $4.54 to $12 a month"),
        "{body}"
    );
    assert!(body.contains("for 10 sites × 10,000 pages $4.54 to $24"));
    assert!(body.contains(&format!(
        "Cheapest VPS that fits, per month before tax, as of {PRICES_AS_OF}"
    )));

    // Every provider, both sizes, and every source link with its date.
    for o in &VPS_OFFERS {
        assert!(body.contains(o.provider), "{}", o.provider);
        assert!(body.contains(&format!("{}: {}", o.lite.plan, o.lite.spec)));
        assert!(body.contains(&format!("{}: {}", o.pro.plan, o.pro.spec)));
        for s in o.sources {
            assert!(
                body.contains(&format!(r#"<a href="{}""#, s.url)),
                "{}",
                s.url
            );
        }
    }
    for c in &OTHER_COSTS {
        assert!(body.contains(&format!("<dt>{}</dt>", c.item)), "{}", c.item);
        for s in c.sources {
            assert!(
                body.contains(&format!(r#"<a href="{}""#, s.url)),
                "{}",
                s.url
            );
        }
    }
    assert!(body.contains(r#"<li id="sh-src-1">"#));
    assert!(body.contains(&format!("</a>, as of {PRICES_AS_OF}</li>")));
    assert!(body.contains(">Self-host guide</a>"));
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
