//! Generated fake sites: describe pages, get a running server and hit counters.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::header::{HeaderName, HeaderValue};
use axum::http::{StatusCode, Uri};
use axum::response::Response;
use codoseo_core::Url;

use crate::server::TestServer;

/// One response a fake site can give.
#[derive(Debug, Clone)]
pub struct Page {
    pub status: u16,
    pub body: String,
    pub headers: Vec<(String, String)>,
    pub delay: Option<Duration>,
    /// When not empty, the page is a sequence and the fields above are unused.
    sequence: Vec<Page>,
}

impl Page {
    /// 200 `text/html`.
    pub fn html(body: &str) -> Page {
        Page::status(200, body)
    }

    /// `text/html` with the given status.
    pub fn status(status: u16, body: &str) -> Page {
        Page {
            status,
            body: body.to_owned(),
            headers: vec![(
                "content-type".to_owned(),
                "text/html; charset=utf-8".to_owned(),
            )],
            delay: None,
            sequence: Vec::new(),
        }
    }

    pub fn redirect(status: u16, location: &str) -> Page {
        Page {
            status,
            body: String::new(),
            headers: vec![("location".to_owned(), location.to_owned())],
            delay: None,
            sequence: Vec::new(),
        }
    }

    /// A 200 HTML page that answers after `delay`.
    pub fn slow(delay: Duration) -> Page {
        Page {
            delay: Some(delay),
            ..Page::html("<title>slow</title>")
        }
    }

    /// Sets a header, replacing one with the same name.
    pub fn header(mut self, name: &str, value: &str) -> Page {
        self.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    /// Answers with each page in turn, one per request to the path; the last one repeats.
    pub fn sequence(pages: Vec<Page>) -> Page {
        assert!(!pages.is_empty(), "a sequence needs at least one page");
        Page {
            status: 200,
            body: String::new(),
            headers: Vec::new(),
            delay: None,
            sequence: pages,
        }
    }

    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).expect("a valid HTTP status");
        let mut res = Response::new(Body::from(self.body));
        *res.status_mut() = status;
        for (name, value) in self.headers {
            res.headers_mut().insert(
                HeaderName::from_bytes(name.as_bytes()).expect("a valid header name"),
                HeaderValue::from_str(&value).expect("a valid header value"),
            );
        }
        res
    }
}

/// A full HTML document with a title, an `<h1>`, a description and one link per entry
/// of `links`.
pub fn html_page(title: &str, links: &[&str]) -> String {
    let anchors: String = links
        .iter()
        .map(|href| format!("<a href=\"{href}\">{href}</a>\n"))
        .collect();
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>{title}</title>\n\
         <meta name=\"description\" content=\"A generated test page called {title}.\">\n\
         </head>\n<body>\n<h1>{title}</h1>\n{anchors}</body>\n</html>\n"
    )
}

/// Describes a fake site. Paths are matched with their query, as received; a page
/// registered without a query also answers requests that have one.
#[derive(Default)]
pub struct SiteBuilder {
    pages: HashMap<String, Page>,
    fallback: Option<Page>,
    endless: Option<usize>,
    sitemap: Option<Vec<String>>,
}

impl SiteBuilder {
    pub fn new() -> SiteBuilder {
        SiteBuilder::default()
    }

    pub fn page(mut self, path_and_query: &str, page: Page) -> Self {
        self.pages.insert(path_and_query.to_owned(), page);
        self
    }

    /// A 200 page made with [`html_page`].
    pub fn html(self, path: &str, title: &str, links: &[&str]) -> Self {
        self.page(path, Page::html(&html_page(title, links)))
    }

    pub fn robots(self, status: u16, body: &str) -> Self {
        self.page(
            "/robots.txt",
            Page::status(status, body).header("content-type", "text/plain"),
        )
    }

    /// Serves `/sitemap.xml` listing `paths` as absolute URLs on this site.
    pub fn sitemap(mut self, paths: &[&str]) -> Self {
        self.sitemap = Some(paths.iter().map(|p| (*p).to_owned()).collect());
        self
    }

    /// `/list?page=N` links to `?page=N+1` and to `facets` fresh `?page=N&f=K` pages
    /// (K = 1..=facets), for every N, and each facet page `f=K` links to `facets` fresh
    /// facets of its own (`f = K × facets + 1 ..= K × facets + facets`, a tree, so no
    /// URL repeats). The space has no end and grows with every page fetched.
    pub fn endless(mut self, facets: usize) -> Self {
        self.endless = Some(facets);
        self
    }

    /// Answers every path no other rule matches. `/robots.txt` and `/sitemap.xml` are
    /// not caught: they answer 404 unless set.
    pub fn every_path(mut self, page: Page) -> Self {
        self.fallback = Some(page);
        self
    }

    pub async fn start(self) -> TestSite {
        let state = Arc::new(SiteState {
            pages: self.pages,
            fallback: self.fallback,
            endless: self.endless,
            sitemap: self.sitemap,
            base: OnceLock::new(),
            hits: AtomicUsize::new(0),
            counters: Mutex::new(Counters::default()),
        });
        let router = Router::new()
            .fallback(handle)
            .with_state(Arc::clone(&state));
        let server = TestServer::start(router).await;
        state.base.set(server.base()).expect("the base is set once");
        TestSite { server, state }
    }
}

/// A running fake site.
pub struct TestSite {
    pub server: TestServer,
    state: Arc<SiteState>,
}

impl TestSite {
    pub fn url(&self, path: &str) -> Url {
        self.server.url(path)
    }

    /// Every request received so far, whatever its path.
    pub fn hits(&self) -> usize {
        self.state.hits.load(Ordering::SeqCst)
    }

    /// Requests received for `path_and_query`, as written in the request line.
    pub fn path_hits(&self, path_and_query: &str) -> usize {
        self.state
            .counters()
            .by_path
            .get(path_and_query)
            .copied()
            .unwrap_or(0)
    }
}

#[derive(Default)]
struct Counters {
    by_path: HashMap<String, usize>,
    sequence: HashMap<String, usize>,
}

struct SiteState {
    pages: HashMap<String, Page>,
    fallback: Option<Page>,
    endless: Option<usize>,
    sitemap: Option<Vec<String>>,
    base: OnceLock<String>,
    hits: AtomicUsize,
    counters: Mutex<Counters>,
}

impl SiteState {
    fn counters(&self) -> std::sync::MutexGuard<'_, Counters> {
        self.counters.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Picks the page for a request and counts it. Holds no lock afterwards.
    fn resolve(&self, uri: &Uri) -> Page {
        let path = uri.path();
        let key = uri.path_and_query().map_or(path, |pq| pq.as_str());
        self.hits.fetch_add(1, Ordering::SeqCst);
        let mut counters = self.counters();
        *counters.by_path.entry(key.to_owned()).or_default() += 1;

        let registered = [key, path]
            .into_iter()
            .find(|k| self.pages.contains_key(*k));
        if let Some(k) = registered {
            let page = &self.pages[k];
            if page.sequence.is_empty() {
                return page.clone();
            }
            let n = counters.sequence.entry(k.to_owned()).or_default();
            let page = page.sequence[(*n).min(page.sequence.len() - 1)].clone();
            *n += 1;
            return page;
        }
        drop(counters);

        let not_found = || Page::status(404, "<title>not found</title>");
        match path {
            "/sitemap.xml" => self
                .sitemap
                .as_ref()
                .map_or_else(not_found, |paths| self.sitemap_page(paths)),
            "/robots.txt" => not_found(),
            "/list" if self.endless.is_some() => self.endless_page(uri.query().unwrap_or("")),
            _ => self.fallback.clone().unwrap_or_else(not_found),
        }
    }

    fn sitemap_page(&self, paths: &[String]) -> Page {
        let base = self.base.get().expect("the server is running");
        let urls: String = paths
            .iter()
            .map(|p| format!("<url><loc>{base}{p}</loc></url>"))
            .collect();
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
             <urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">{urls}</urlset>"
        );
        Page::status(200, &body).header("content-type", "application/xml")
    }

    fn endless_page(&self, query: &str) -> Page {
        let facets = self.endless.unwrap_or(0);
        let value = |name: &str| {
            query
                .split('&')
                .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
                .and_then(|v| v.parse::<usize>().ok())
        };
        let n = value("page").unwrap_or(0);
        // The list page is node 0 of the facet tree; node K's children are
        // K × facets + 1 ..= K × facets + facets.
        let node = value("f");
        let first_child = node
            .unwrap_or(0)
            .checked_mul(facets)
            .and_then(|c| c.checked_add(1));
        let mut links = Vec::new();
        if node.is_none() {
            links.push(format!("/list?page={}", n + 1));
        }
        if let Some(first) = first_child {
            links.extend((0..facets).map(|j| format!("/list?page={n}&f={}", first + j)));
        }
        let title = match node {
            Some(f) => format!("List {n} facet {f}"),
            None => format!("List {n}"),
        };
        let refs: Vec<&str> = links.iter().map(String::as_str).collect();
        Page::html(&html_page(&title, &refs))
    }
}

async fn handle(State(state): State<Arc<SiteState>>, uri: Uri) -> Response {
    let page = state.resolve(&uri);
    if let Some(delay) = page.delay {
        tokio::time::sleep(delay).await;
    }
    page.into_response()
}
