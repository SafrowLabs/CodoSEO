//! `codoseo healthcheck`: the probe the container HEALTHCHECK runs (the image has no curl).

use std::sync::mpsc;

use assert_cmd::cargo::cargo_bin_cmd;
use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;

/// Serves `/ok` (200), `/slow` (never answers within the probe timeout) and `/down` (503) on
/// a background thread and returns the address.
fn serve() -> std::net::SocketAddr {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("a runtime");
        rt.block_on(async {
            let app = Router::new()
                .route("/ok", get(|| async { "fine" }))
                .route("/down", get(|| async { StatusCode::SERVICE_UNAVAILABLE }))
                .route(
                    "/slow",
                    get(|| async {
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });
    rx.recv().expect("the server started")
}

#[test]
fn healthy_endpoint_exits_zero() {
    let addr = serve();
    cargo_bin_cmd!("codoseo")
        .args(["healthcheck", "--url", &format!("http://{addr}/ok")])
        .assert()
        .success();
}

#[test]
fn non_2xx_exits_one() {
    let addr = serve();
    cargo_bin_cmd!("codoseo")
        .args(["healthcheck", "--url", &format!("http://{addr}/down")])
        .assert()
        .code(1);
}

#[test]
fn nothing_listening_exits_one() {
    // Port 1 on loopback is never open in CI or a container.
    cargo_bin_cmd!("codoseo")
        .args(["healthcheck", "--url", "http://127.0.0.1:1/readyz"])
        .assert()
        .code(1);
}

#[test]
fn a_hung_server_times_out_with_exit_one() {
    let addr = serve();
    cargo_bin_cmd!("codoseo")
        .args([
            "healthcheck",
            "--url",
            &format!("http://{addr}/slow"),
            "--timeout",
            "1",
        ])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .code(1);
}

#[test]
fn the_default_url_follows_codoseo_bind() {
    let addr = serve();
    // The test server has no /readyz, so the default URL answers 404: exit 1, and the message
    // names the URL derived from CODOSEO_BIND.
    cargo_bin_cmd!("codoseo")
        .arg("healthcheck")
        .env("CODOSEO_BIND", format!("0.0.0.0:{}", addr.port()))
        .assert()
        .code(1)
        .stderr(predicates::str::contains(format!(
            "http://127.0.0.1:{}/readyz",
            addr.port()
        )));
}

#[test]
fn proxy_environment_does_not_hijack_the_loopback_probe() {
    // A container with HTTP_PROXY set (a corporate proxy, a Coolify setting) must still probe
    // its own port directly, not through the proxy.
    let addr = serve();
    cargo_bin_cmd!("codoseo")
        .args(["healthcheck", "--url", &format!("http://{addr}/ok")])
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("http_proxy", "http://127.0.0.1:1")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .assert()
        .success();
}
