//! Agent API metrics: `codoseo_api_requests_total` counts each REST and MCP call once, by
//! surface, tier and result, and the public router has no `/metrics`.

mod support;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use codoseo_store::api_keys::{self, CreateKeyOutcome};
use codoseo_web::agent::keys;
use metrics_exporter_prometheus::PrometheusBuilder;
use serde_json::json;
use support::mcp::{Client, jsonrpc};
use support::{TestApp, cloud_config};

#[tokio::test]
async fn the_public_router_has_no_metrics_endpoint() {
    let app = TestApp::with_config(cloud_config()).await;
    for path in ["/metrics", "/metrics/", "/api/v1/metrics"] {
        let res = app.get(path, None).await;
        assert!(
            res.status == StatusCode::NOT_FOUND || res.status == StatusCode::UNAUTHORIZED,
            "{path}: {}",
            res.status
        );
        assert!(!res.body.contains("codoseo_"), "{path}: {}", res.body);
    }
}

#[tokio::test]
async fn rest_and_mcp_calls_are_counted_by_surface_tier_and_result() {
    let recorder = PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    // A current-thread runtime runs every handler on this thread, so a thread-local recorder
    // sees them all.
    let _guard = metrics::set_default_local_recorder(&recorder);

    let app = TestApp::with_config(cloud_config()).await;
    let (account, _) = app.login("owner@example.com").await;
    let key = keys::generate();
    match api_keys::create(
        app.pool(),
        account.id,
        "test key",
        &key.hash,
        &key.prefix,
        api_keys::MAX_LIVE_KEYS,
    )
    .await
    .unwrap()
    {
        CreateKeyOutcome::Created(_) => {}
        CreateKeyOutcome::LimitReached => panic!("at the cap"),
    }

    let rest = |auth: Option<&str>| {
        let mut b = Request::builder().method(Method::GET).uri("/api/v1/sites");
        if let Some(k) = auth {
            b = b.header(header::AUTHORIZATION, format!("Bearer {k}"));
        }
        b.body(Body::empty()).unwrap()
    };
    assert_eq!(app.send(rest(Some(&key.plaintext))).await.status, 200);
    assert_eq!(app.send(rest(None)).await.status, 401);

    // A keyed MCP tool call.
    let call = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::HOST, "localhost")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::AUTHORIZATION, format!("Bearer {}", key.plaintext))
        .body(Body::from(
            jsonrpc("tools/call", json!({"name": "list_sites", "arguments": {}})).to_string(),
        ))
        .unwrap();
    assert_eq!(app.send(call).await.status, 200);

    // A no-key call that fails (an unknown audit id).
    let anon = Client::new(&app, Duration::ZERO, None);
    anon.call_err("get_audit", json!({"audit_id": "nope"}))
        .await;

    let body = handle.render();
    for series in [
        r#"codoseo_api_requests_total{surface="rest",tier="key",result="ok"} 1"#,
        r#"codoseo_api_requests_total{surface="rest",tier="key",result="unauthorized"} 1"#,
        r#"codoseo_api_requests_total{surface="mcp",tier="key",result="ok"} 1"#,
        r#"codoseo_api_requests_total{surface="mcp",tier="anon",result="error"} 1"#,
    ] {
        assert!(body.contains(series), "{series} missing from:\n{body}");
    }
}
