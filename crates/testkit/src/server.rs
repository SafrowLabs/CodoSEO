//! A local HTTP server for tests. Each test builds the routes it needs.

use std::io::Write;
use std::net::SocketAddr;

use axum::Router;
use codoseo_core::Url;
use flate2::Compression;
use flate2::write::GzEncoder;

/// Serves a [`Router`] on a free port of 127.0.0.1 until the test's runtime ends.
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

    /// The address of `path` (with an optional query) on this server.
    pub fn url(&self, path: &str) -> Url {
        Url::parse(&format!("http://{}{}", self.addr, path)).unwrap()
    }

    /// The same port reached through the `localhost` name.
    pub fn localhost_url(&self, path: &str) -> Url {
        Url::parse(&format!("http://localhost:{}{}", self.addr.port(), path)).unwrap()
    }

    /// `http://127.0.0.1:PORT`, without a trailing slash.
    pub fn base(&self) -> String {
        format!("http://{}", self.addr)
    }
}

pub fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(bytes).unwrap();
    enc.finish().unwrap()
}
