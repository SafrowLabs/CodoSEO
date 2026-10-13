//! The legal pages (`/terms`, `/privacy`, `/refunds`): served on the cloud only, carrying the
//! clauses that matter, and linked wherever someone signs up, pays or reaches a page footer.

mod support;

use axum::http::StatusCode;
use support::{TestApp, cloud_config, cloud_config_with};

async fn cloud() -> TestApp {
    TestApp::with_config(cloud_config()).await
}

const LINKS: [&str; 3] = [
    r#"href="/terms""#,
    r#"href="/privacy""#,
    r#"href="/refunds""#,
];

fn assert_links(body: &str, what: &str) {
    for link in LINKS {
        assert!(body.contains(link), "{what} links {link}: {body}");
    }
}

#[tokio::test]
async fn each_legal_page_is_public_indexable_and_dated() {
    let app = cloud().await;
    for (path, title) in [
        ("/terms", "Terms of Service"),
        ("/privacy", "Privacy Policy"),
        ("/refunds", "Refund Policy"),
    ] {
        let res = app.get(path, None).await;
        assert_eq!(res.status, StatusCode::OK, "{path}");
        for needle in [
            format!("<h1 class=\"q-title\">{title}</h1>"),
            format!(r#"<link rel="canonical" href="https://codoseo.com{path}">"#),
            r#"<meta name="robots" content="index, follow">"#.to_owned(),
            r#"<time datetime="2026-10-13">13 October 2026</time>"#.to_owned(),
            "Plain-language summary".to_owned(),
            "mailto:contact@codoseo.com".to_owned(),
            r#"aria-current="page""#.to_owned(),
        ] {
            assert!(res.body.contains(&needle), "{path} {needle}: {}", res.body);
        }
        assert_links(&res.body, path);
    }
}

#[tokio::test]
async fn the_terms_carry_the_key_clauses() {
    let res = cloud().await.get("/terms", None).await;
    for needle in [
        "RankOrg",
        "You may only add, audit and crawl websites you own or are authorised to monitor.",
        "You are solely responsible for every site you add and for any crawling we do on your behalf.",
        "We honour robots.txt",
        "suspend or close any account",
        "share them with law enforcement or other authorities when the law requires us to",
        "Dodo Payments is our merchant of record.",
        "the price shown on the pricing page at the time of purchase",
        "checks and estimates, not guarantees",
        "\"as is\"",
        "<strong>3 months</strong>",
        "governed by the laws of <strong>India</strong>",
        "AGPL-3.0",
    ] {
        assert!(res.body.contains(needle), "{needle}: {}", res.body);
    }
}

#[tokio::test]
async fn the_privacy_policy_matches_what_the_code_keeps() {
    let res = cloud().await.get("/privacy", None).await;
    for needle in [
        // The cookies the app sets, and nothing else.
        "codoseo_session",
        "codoseo_audit",
        "codoseo_oauth",
        // How secrets are stored.
        "<strong>stored encrypted</strong>",
        "<strong>only as a SHA-256 hash</strong>",
        "<strong>We never receive or store card numbers</strong>",
        // Retention, from the daily cleanup and the plan limits.
        "<strong>30 days on the Free plan</strong>",
        "<strong>one year on paid plans</strong>",
        "<strong>7 days</strong> if no one claims them",
        // Processors, named only where the code names them.
        "<strong>Dodo Payments</strong>",
        "<strong>Our email delivery provider</strong>",
        "<strong>Cloudflare</strong>",
        "Umami",
        "Digital Personal Data Protection Act, 2023",
        "<strong>We do not sell your personal data</strong>",
    ] {
        assert!(res.body.contains(needle), "{needle}: {}", res.body);
    }
}

#[tokio::test]
async fn the_refund_policy_states_the_seven_day_window() {
    let res = cloud().await.get("/refunds", None).await;
    for needle in [
        "<strong>within 7 days of its first payment</strong>",
        "<strong>renewals are not refunded</strong>",
        "Cancelling stops the next renewal",
        "<strong>original payment method</strong>",
        "<strong>from the email address on your CodoSEO account</strong>",
    ] {
        assert!(res.body.contains(needle), "{needle}: {}", res.body);
    }
}

#[tokio::test]
async fn self_hosted_instances_have_no_legal_pages_or_links() {
    let app = TestApp::new().await;
    for path in ["/terms", "/privacy", "/refunds"] {
        assert_eq!(
            app.get(path, None).await.status,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    let login = app.get("/login", None).await;
    assert!(!login.body.contains(r#"href="/terms""#), "{}", login.body);
    let (_, cookie) = app.login("me@example.com").await;
    let sites = app.get("/sites", Some(&cookie)).await;
    assert!(!sites.body.contains(r#"href="/terms""#), "{}", sites.body);
}

#[tokio::test]
async fn footers_and_sign_up_screens_link_the_legal_pages() {
    let app = cloud().await;
    for path in ["/", "/bot", "/ai-bots"] {
        assert_links(&app.get(path, None).await.body, path);
    }

    let login = app.get("/login", None).await;
    assert!(
        login
            .body
            .contains("By continuing you agree to the <a href=\"/terms\">Terms</a> and <a href=\"/privacy\">Privacy Policy</a>."),
        "{}",
        login.body
    );
    assert_links(&login.body, "login");

    // The audit report (and its email unlock form) links them too.
    let res = app.post("/audit", "url=example.com", None).await;
    let report = res.location().expect("redirect to the report").to_owned();
    let body = app.get(&report, None).await.body;
    assert_links(&body, "audit report");

    // The app shell's account menu.
    let (_, cookie) = app.login("me@example.com").await;
    assert_links(&app.get("/sites", Some(&cookie)).await.body, "app shell");
}

#[tokio::test]
async fn checkout_names_the_merchant_of_record_and_the_terms() {
    let app = TestApp::with_config(cloud_config_with(&[
        ("DODO_API_KEY", "dodo_key_123"),
        ("DODO_WEBHOOK_SECRET", "whsec_dGVzdC13ZWJob29rLXNlY3JldA=="),
        ("DODO_PRODUCT_LITE", "pdt_lite"),
        ("DODO_PRODUCT_PRO", "pdt_pro"),
    ]))
    .await;
    let (_, cookie) = app.login("me@example.com").await;
    let res = app.get("/billing", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("Upgrade to"), "{}", res.body);
    assert!(
        res.body
            .contains("Payments are processed by Dodo Payments, our merchant of record"),
        "{}",
        res.body
    );
    assert!(
        res.body.contains("By upgrading you agree to the"),
        "{}",
        res.body
    );
    assert_links(&res.body, "billing");
}

#[tokio::test]
async fn the_sitemap_and_llms_txt_list_the_legal_pages() {
    let app = cloud().await;
    let sitemap = app.get("/sitemap.xml", None).await.body;
    let llms = app.get("/llms.txt", None).await.body;
    for path in ["terms", "privacy", "refunds"] {
        let url = format!("https://codoseo.com/{path}");
        assert!(sitemap.contains(&format!("<loc>{url}</loc>")), "{sitemap}");
        assert!(llms.contains(&url), "{llms}");
    }
}
