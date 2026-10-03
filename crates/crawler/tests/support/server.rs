//! A local HTTP server for crawler tests. Each test file builds the routes it needs.

use std::io::Write;
use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use codoseo_core::Url;
use flate2::Compression;
use flate2::write::GzEncoder;

pub struct TestServer {
    addr: SocketAddr,
}

impl TestServer {
    pub async fn start(router: Router) -> TestServer {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        TestServer { addr }
    }

    pub fn url(&self, path: &str) -> Url {
        Url::parse(&format!("http://{}{}", self.addr, path)).unwrap()
    }

    pub fn localhost_url(&self, path: &str) -> Url {
        Url::parse(&format!("http://localhost:{}{}", self.addr.port(), path)).unwrap()
    }
}

pub fn html(body: impl Into<String>) -> Response {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        body.into(),
    )
        .into_response()
}

fn moved(status: StatusCode, to: &str) -> Response {
    (status, [(header::LOCATION, to.to_owned())]).into_response()
}

pub fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(bytes).unwrap();
    enc.finish().unwrap()
}

/// Routes used by the fetcher tests.
pub fn fetch_routes() -> Router {
    Router::new()
        .route(
            "/r1",
            get(|| async { moved(StatusCode::MOVED_PERMANENTLY, "/r2") }),
        )
        .route("/r2", get(|| async { moved(StatusCode::FOUND, "/r3") }))
        .route(
            "/r3",
            get(|| async { moved(StatusCode::MOVED_PERMANENTLY, "/ok") }),
        )
        .route(
            "/ok",
            get(|| async {
                (
                    [
                        (header::CONTENT_TYPE, "text/html"),
                        (header::HeaderName::from_static("x-robots-tag"), "noindex"),
                    ],
                    "<!doctype html><title>ok</title>",
                )
            }),
        )
        .route("/loop-a", get(|| async { Redirect::permanent("/loop-b") }))
        .route("/loop-b", get(|| async { Redirect::permanent("/loop-a") }))
        .route(
            "/hop/{n}",
            get(|Path(n): Path<u32>| async move {
                if n == 0 {
                    html("<p>done</p>")
                } else {
                    moved(StatusCode::FOUND, &format!("/hop/{}", n - 1))
                }
            }),
        )
        .route(
            "/to-mailto",
            get(|| async { moved(StatusCode::FOUND, "mailto:a@b.c") }),
        )
        .route("/big", get(|| async { html("x".repeat(6 * 1024 * 1024)) }))
        .route(
            "/doc.pdf",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/pdf")],
                    vec![b'%'; 2048],
                )
                    .into_response()
            }),
        )
        .route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(3)).await;
                html("late")
            }),
        )
        .route(
            "/gz",
            get(|| async {
                (
                    [
                        (header::CONTENT_TYPE, "text/html"),
                        (header::CONTENT_ENCODING, "gzip"),
                    ],
                    gzip(b"<!doctype html><title>zipped</title>"),
                )
                    .into_response()
            }),
        )
        .route(
            "/robots.txt",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain")],
                    "User-agent: *\nDisallow:\n",
                )
                    .into_response()
            }),
        )
}
