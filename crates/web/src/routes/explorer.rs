//! `/s/{site}/explorer`: the URL explorer, modelled on Screaming Frog. Filters down the left
//! (response codes, indexability, content type, failing checks), the URL grid with live search
//! and infinite scroll, and a detail panel with four tabs: URL details, SERP snippet, inlinks
//! and rebuilt HTTP headers. All state lives in the URL (`?filter=&q=&sel=&tab=`), so any view
//! can be shared or reloaded.
//!
//! Fragments: `explorer/rows` (100 grid rows per request, keyset-paginated on `pages.id`),
//! `explorer/detail` (the `#detail` panel) and `key-pages` (the star toggle). Each one sets
//! `HX-Replace-Url` where it changes what the page shows, so the address bar stays shareable.

use std::collections::HashSet;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use codoseo_checks::{MAX_INLINK_SAMPLES, Scope, def};
use codoseo_core::check::{CheckId, IssueBits, Severity};
use codoseo_core::page::Indexability;
use codoseo_store::crawls::{Crawl, StoredSummary};
use codoseo_store::explorer::{FilterCounts, GridRow, Inlink, PageDetail, PageFilter};
use codoseo_store::sites::Site;
use serde::Deserialize;
use url::Url;
use uuid::Uuid;

use crate::auth::{CurrentUser, load_site, urlencode};
use crate::error::AppError;
use crate::fmt;
use crate::layout::{Screen, Shell};
use crate::render::{Hx, html};
use crate::serp::{DESCRIPTION_LIMIT_PX, TITLE_LIMIT_PX, description_px, title_px, truncate_to_px};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/s/{site}/explorer", get(page))
        .route("/s/{site}/explorer/rows", get(rows))
        .route("/s/{site}/explorer/detail", get(detail))
        .route("/s/{site}/key-pages", post(toggle_key_page))
}

/// Grid rows per request.
const PAGE_SIZE: usize = 100;
/// Stored inlinks shown in the Inlinks tab.
const INLINKS_SHOWN: i64 = 50;
/// Thresholds the grid and the details tab highlight (matching the checks where one exists).
const TITLE_MAX_CHARS: usize = 60;
const TITLE_MIN_CHARS: usize = 30;
const DESCRIPTION_MAX_CHARS: usize = 155;
const DESCRIPTION_MIN_CHARS: usize = 70;
const THIN_WORDS: u32 = 200;
const DEEP_CLICKS: u32 = 4;
const SLOW_MS: u32 = 1000;

/// The query string every explorer route reads. Unknown or malformed values fall back to
/// the defaults instead of failing.
#[derive(Debug, Default, Deserialize)]
pub struct Params {
    #[serde(default)]
    filter: String,
    #[serde(default)]
    q: String,
    #[serde(default)]
    sel: String,
    #[serde(default)]
    tab: String,
    #[serde(default)]
    after: String,
}

impl Params {
    fn filter(&self) -> PageFilter {
        PageFilter::parse(&self.filter)
    }

    fn q(&self) -> &str {
        self.q.trim()
    }

    fn sel(&self) -> Option<u64> {
        parse_hex(&self.sel)
    }

    fn tab(&self) -> Tab {
        Tab::parse(&self.tab)
    }
}

/// The detail panel's tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Details,
    Serp,
    Inlinks,
    Headers,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::Details, Tab::Serp, Tab::Inlinks, Tab::Headers];

    fn parse(s: &str) -> Tab {
        match s {
            "serp" => Tab::Serp,
            "inlinks" => Tab::Inlinks,
            "headers" => Tab::Headers,
            _ => Tab::Details,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Tab::Details => "details",
            Tab::Serp => "serp",
            Tab::Inlinks => "inlinks",
            Tab::Headers => "headers",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Tab::Details => "URL details",
            Tab::Serp => "SERP snippet",
            Tab::Inlinks => "Inlinks",
            Tab::Headers => "HTTP headers",
        }
    }
}

/// A URL hash as it appears in URLs: 16 lowercase hex digits.
pub fn hex(hash: u64) -> String {
    format!("{hash:016x}")
}

fn parse_hex(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.len() != 16 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(s, 16).ok()
}

/// The shareable explorer URL for a view. `tab` is left out when it's the default.
fn explorer_url(base: &str, filter: PageFilter, q: &str, sel: Option<u64>, tab: Tab) -> String {
    let mut url = format!("{base}/explorer?filter={}", filter.key());
    if !q.is_empty() {
        url.push_str("&q=");
        url.push_str(&urlencode(q));
    }
    if let Some(h) = sel {
        url.push_str("&sel=");
        url.push_str(&hex(h));
    }
    if tab != Tab::Details {
        url.push_str("&tab=");
        url.push_str(tab.key());
    }
    url
}

fn replace_url(url: &str) -> (HeaderName, HeaderValue) {
    (
        HeaderName::from_static("hx-replace-url"),
        HeaderValue::from_str(url).unwrap_or_else(|_| HeaderValue::from_static("")),
    )
}

// ── Templates ────────────────────────────────────────────

#[derive(Template)]
#[template(path = "explorer/index.html")]
pub struct ExplorerPage {
    pub shell: Shell,
    pub base: String,
    /// `None` until the site has a finished crawl.
    pub view: Option<ExplorerView>,
    /// A crawl is queued or running (the empty state says so instead of offering one).
    pub crawl_active: bool,
}

pub struct ExplorerView {
    pub groups: Vec<FilterGroup>,
    pub filter_key: String,
    pub q: String,
    pub rows_url: String,
    pub info: InfoSpan,
    pub rows: RowsFragment,
    pub detail: DetailPanel,
}

pub struct FilterGroup {
    pub title: &'static str,
    pub items: Vec<FilterLink>,
}

pub struct FilterLink {
    pub label: String,
    /// `bg-*` class for the swatch.
    pub swatch: &'static str,
    pub count: String,
    pub zero: bool,
    pub href: String,
    pub active: bool,
}

/// `All URLs · 1,284 shown of 1,284`. Swapped out of band after a search.
#[derive(Template)]
#[template(path = "explorer/info.html")]
pub struct InfoSpan {
    pub text: String,
    pub oob: bool,
}

/// A batch of grid rows, plus the sentinel that loads the next batch when scrolled into view.
#[derive(Template)]
#[template(path = "explorer/rows.html")]
pub struct RowsFragment {
    pub rows: Vec<RowView>,
    /// The next batch's URL, when there is one.
    pub more: Option<String>,
    /// The first batch is empty: no URL matches.
    pub empty: bool,
    /// The toolbar count, sent out of band with a search's first batch.
    pub info: Option<InfoSpan>,
}

pub struct RowView {
    pub url: String,
    /// Path plus query, what the Address column shows.
    pub path: String,
    pub status: String,
    /// `t-*` tone for the status badge.
    pub status_tone: &'static str,
    pub index_label: &'static str,
    pub index_class: &'static str,
    pub title: String,
    pub title_class: &'static str,
    pub title_len: String,
    pub title_len_class: &'static str,
    pub words: String,
    pub words_class: &'static str,
    pub depth: String,
    pub depth_class: &'static str,
    pub inlinks: String,
    pub response: String,
    pub response_class: &'static str,
    pub star: StarButton,
    pub selected: bool,
    pub detail_href: String,
}

/// The key-page star, in a grid row (`at = "row"`) or the detail tab bar (`"detail"`).
#[derive(Template)]
#[template(path = "explorer/star.html")]
pub struct StarButton {
    pub base: String,
    pub hex: String,
    pub on: bool,
    /// Play the pop animation (just starred).
    pub pop: bool,
    pub at: &'static str,
}

/// The `#detail` panel.
#[derive(Template)]
#[template(path = "explorer/detail.html")]
pub struct DetailPanel {
    /// `None` when nothing is selected (no URL matches).
    pub page: Option<DetailView>,
}

pub struct DetailView {
    pub hex: String,
    pub url: String,
    pub tab: &'static str,
    pub tabs: Vec<TabLink>,
    pub star: StarButton,
    pub details: Option<DetailsTab>,
    pub serp: Option<SerpView>,
    pub inlinks: Option<InlinksTab>,
    /// The rebuilt response, one header per line.
    pub headers: Option<String>,
}

pub struct TabLink {
    pub label: &'static str,
    pub href: String,
    pub active: bool,
    pub count: Option<String>,
}

pub struct DetailsTab {
    pub fields: Vec<Field>,
    pub issues: Vec<IssueChip>,
}

pub struct Field {
    pub k: &'static str,
    pub v: String,
    /// A `c-*` colour class, or empty.
    pub class: &'static str,
    /// Shown as a link to this URL (opens the page itself, in a new tab).
    pub href: Option<String>,
}

pub struct IssueChip {
    pub title: &'static str,
    /// `sev-*` class.
    pub sev: &'static str,
    pub href: String,
}

pub struct SerpView {
    pub crumb_host: String,
    /// ` › blog › post`
    pub crumb_path: String,
    /// The title as it would show, cut with ` …`; `None` when missing.
    pub title: Option<String>,
    pub description: Option<String>,
    pub meters: Vec<MeterView>,
}

pub struct MeterView {
    pub label: &'static str,
    /// `612 / 580 px`
    pub value: String,
    pub value_class: &'static str,
    /// `bg-*` class for the bar.
    pub bar: &'static str,
    pub pct: u32,
    /// Where the limit marker sits.
    pub limit_pct: u32,
    pub note: &'static str,
}

pub struct InlinksTab {
    pub rows: Vec<InlinkView>,
    pub note: String,
}

pub struct InlinkView {
    pub from: String,
    /// Opens the linking page in the explorer.
    pub href: String,
    pub anchor: String,
    pub anchor_class: &'static str,
    pub kind: &'static str,
    pub kind_class: &'static str,
}

// ── Handlers ─────────────────────────────────────────────

/// `GET /s/{site}/explorer`: the full screen.
async fn page(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site_id): Path<Uuid>,
    Query(params): Query<Params>,
) -> Result<Response, AppError> {
    let site = load_site(&state, &user, site_id).await?;
    let shell = Shell::load(&state, &user, Some(&site), Screen::Explorer).await?;
    let pool = &state.pool;
    let base = format!("/s/{}", site.id);
    let Some(crawl) = codoseo_store::crawls::latest_done(pool, site.id).await? else {
        let crawl_active = codoseo_store::crawls::active(pool, site.id)
            .await?
            .is_some();
        return Ok(html(&ExplorerPage {
            shell,
            base,
            view: None,
            crawl_active,
        })?
        .into_response());
    };

    let filter = params.filter();
    let q = params.q();
    let tab = params.tab();
    let counts = codoseo_store::explorer::filter_counts(pool, crawl.id).await?;
    let (shown, total) = codoseo_store::explorer::match_count(pool, crawl.id, filter, q).await?;
    let batch =
        codoseo_store::explorer::rows(pool, crawl.id, filter, q, 0, PAGE_SIZE as i64 + 1).await?;

    // The selected page: `sel` when it's in this crawl, else the first row.
    let mut selected = match params.sel() {
        Some(h) => codoseo_store::explorer::page(pool, site.id, crawl.id, h).await?,
        None => None,
    };
    if selected.is_none()
        && let Some(first) = batch.first()
    {
        selected = codoseo_store::explorer::page(pool, site.id, crawl.id, first.url_hash).await?;
    }
    let sel = selected.as_ref().map(|p| p.url_hash);

    let key_pages: HashSet<u64> = site.key_pages.iter().copied().collect();
    let rows = rows_fragment(&base, filter, q, sel, batch, &key_pages, true, None);
    let detail = detail_panel(&state, &site, &crawl, selected, tab, &key_pages).await?;
    let view = ExplorerView {
        groups: filter_groups(&base, filter, &counts, crawl.summary().as_ref()),
        filter_key: filter.key(),
        q: q.to_owned(),
        rows_url: format!("{base}/explorer/rows"),
        info: InfoSpan {
            text: info_text(filter, shown, total),
            oob: false,
        },
        rows,
        detail,
    };
    Ok(html(&ExplorerPage {
        shell,
        base,
        view: Some(view),
        crawl_active: false,
    })?
    .into_response())
}

/// `GET /s/{site}/explorer/rows`: one batch of grid rows. Without `after` it's a fresh search,
/// so it also swaps the toolbar count and updates the address bar.
async fn rows(
    State(state): State<AppState>,
    user: CurrentUser,
    hx: Hx,
    Path(site_id): Path<Uuid>,
    Query(params): Query<Params>,
) -> Result<Response, AppError> {
    let site = load_site(&state, &user, site_id).await?;
    let base = format!("/s/{}", site.id);
    let (filter, q, sel, tab) = (params.filter(), params.q(), params.sel(), params.tab());
    let canonical = explorer_url(&base, filter, q, sel, tab);
    if !hx.partial() {
        return Ok(Redirect::to(&canonical).into_response());
    }
    let pool = &state.pool;
    let crawl = codoseo_store::crawls::latest_done(pool, site.id)
        .await?
        .ok_or(AppError::NotFound)?;
    let after = params.after.trim().parse::<i64>().ok();
    let batch = codoseo_store::explorer::rows(
        pool,
        crawl.id,
        filter,
        q,
        after.unwrap_or(0),
        PAGE_SIZE as i64 + 1,
    )
    .await?;
    let key_pages: HashSet<u64> = site.key_pages.iter().copied().collect();
    let first = after.is_none();
    let info = if first {
        let (shown, total) =
            codoseo_store::explorer::match_count(pool, crawl.id, filter, q).await?;
        Some(InfoSpan {
            text: info_text(filter, shown, total),
            oob: true,
        })
    } else {
        None
    };
    let fragment = rows_fragment(&base, filter, q, sel, batch, &key_pages, first, info);
    let body = html(&fragment)?;
    Ok(if first {
        ([replace_url(&canonical)], body).into_response()
    } else {
        body.into_response()
    })
}

/// `GET /s/{site}/explorer/detail`: the `#detail` panel for one page and tab.
async fn detail(
    State(state): State<AppState>,
    user: CurrentUser,
    hx: Hx,
    Path(site_id): Path<Uuid>,
    Query(params): Query<Params>,
) -> Result<Response, AppError> {
    let site = load_site(&state, &user, site_id).await?;
    let base = format!("/s/{}", site.id);
    let (filter, q, sel, tab) = (params.filter(), params.q(), params.sel(), params.tab());
    let canonical = explorer_url(&base, filter, q, sel, tab);
    if !hx.partial() {
        return Ok(Redirect::to(&canonical).into_response());
    }
    let sel = sel.ok_or_else(|| AppError::BadRequest("Pick a URL from the list.".to_owned()))?;
    let pool = &state.pool;
    let crawl = codoseo_store::crawls::latest_done(pool, site.id)
        .await?
        .ok_or(AppError::NotFound)?;
    let page = codoseo_store::explorer::page(pool, site.id, crawl.id, sel)
        .await?
        .ok_or(AppError::NotFound)?;
    let key_pages: HashSet<u64> = site.key_pages.iter().copied().collect();
    let panel = detail_panel(&state, &site, &crawl, Some(page), tab, &key_pages).await?;
    Ok(([replace_url(&canonical)], html(&panel)?).into_response())
}

#[derive(Deserialize)]
struct StarForm {
    hash: String,
}

#[derive(Deserialize, Default)]
struct StarAt {
    #[serde(default)]
    at: String,
}

/// `POST /s/{site}/key-pages`: stars or unstars a page. Returns the clicked star; the
/// `keypage` event in `HX-Trigger` lets the other star for the same page follow.
async fn toggle_key_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site_id): Path<Uuid>,
    Query(at): Query<StarAt>,
    Form(form): Form<StarForm>,
) -> Result<Response, AppError> {
    let site = load_site(&state, &user, site_id).await?;
    let pool = &state.pool;
    let hash = parse_hex(&form.hash)
        .ok_or_else(|| AppError::BadRequest("That isn't a page of this site.".to_owned()))?;
    // Only pages the site actually has can be starred (unstarring always works).
    if !site.key_pages.contains(&hash)
        && !codoseo_store::explorer::page_exists(pool, site.id, hash).await?
    {
        return Err(AppError::NotFound);
    }
    let on = codoseo_store::sites::toggle_key_page(pool, user.id(), site.id, hash)
        .await?
        .ok_or(AppError::NotFound)?;
    let star = StarButton {
        base: format!("/s/{}", site.id),
        hex: hex(hash),
        on,
        pop: on,
        at: if at.at == "detail" { "detail" } else { "row" },
    };
    let message = if on {
        "Marked as a key page. Changes to it are alerted first."
    } else {
        "Removed from key pages"
    };
    let trigger = serde_json::json!({
        "toast": { "kind": "ok", "message": message },
        "keypage": { "hash": star.hex, "on": on },
    });
    let trigger = HeaderValue::from_str(&trigger.to_string()).map_err(AppError::internal)?;
    Ok((
        StatusCode::OK,
        [(HeaderName::from_static("hx-trigger"), trigger)],
        Html(star.render()?),
    )
        .into_response())
}

// ── View building ────────────────────────────────────────

fn filter_label(filter: PageFilter) -> String {
    match filter {
        PageFilter::All => "All URLs",
        PageFilter::Status2xx => "2xx Success",
        PageFilter::Status3xx => "3xx Redirect",
        PageFilter::Status4xx => "4xx Client error",
        PageFilter::Status5xx => "5xx Server error",
        PageFilter::NoResponse => "No response",
        PageFilter::Indexable => "Indexable",
        PageFilter::NonIndexable => "Non-indexable",
        PageFilter::Html => "HTML",
        PageFilter::Image => "Images",
        PageFilter::Other => "Other",
        PageFilter::Check(id) => def(id).title,
    }
    .to_owned()
}

fn info_text(filter: PageFilter, shown: i64, total: i64) -> String {
    format!(
        "{} · {} shown of {}",
        filter_label(filter),
        fmt::thousands(shown),
        fmt::thousands(total)
    )
}

fn severity_swatch(s: Severity) -> &'static str {
    match s {
        Severity::Critical => "bg-err",
        Severity::Warning => "bg-warn",
        Severity::Notice => "bg-ghost",
    }
}

fn severity_badge(s: Severity) -> &'static str {
    match s {
        Severity::Critical => "sev-critical",
        Severity::Warning => "sev-warning",
        Severity::Notice => "sev-notice",
    }
}

fn filter_groups(
    base: &str,
    active: PageFilter,
    counts: &FilterCounts,
    summary: Option<&StoredSummary>,
) -> Vec<FilterGroup> {
    let link = |f: PageFilter, n: i64, swatch: &'static str| FilterLink {
        label: filter_label(f),
        swatch,
        count: fmt::thousands(n),
        zero: n == 0,
        href: format!("{base}/explorer?filter={}", f.key()),
        active: f == active,
    };
    let fixed = |f: PageFilter, swatch| link(f, counts.get(f).unwrap_or(0), swatch);

    let mut codes = vec![
        fixed(PageFilter::All, "bg-ink"),
        fixed(PageFilter::Status2xx, "bg-ok"),
        fixed(PageFilter::Status3xx, "bg-warn"),
        fixed(PageFilter::Status4xx, "bg-err"),
        fixed(PageFilter::Status5xx, "bg-5xx"),
    ];
    if counts.s0 > 0 || active == PageFilter::NoResponse {
        codes.push(fixed(PageFilter::NoResponse, "bg-ghost"));
    }

    // Failing checks that set page bits: critical first, then by pages affected.
    let mut failing: Vec<(CheckId, u32)> = summary
        .map(|s| {
            s.counts
                .iter()
                .filter_map(|(slug, n)| Some((CheckId::from_slug(slug)?, *n)))
                .filter(|(id, _)| def(*id).scope != Scope::SiteWide)
                .collect()
        })
        .unwrap_or_default();
    if let PageFilter::Check(id) = active
        && !failing.iter().any(|(c, _)| *c == id)
    {
        failing.push((id, 0));
    }
    failing.sort_by_key(|(id, n)| (def(*id).severity, std::cmp::Reverse(*n), *id));
    let issues = failing
        .into_iter()
        .map(|(id, n)| {
            link(
                PageFilter::Check(id),
                i64::from(n),
                severity_swatch(def(id).severity),
            )
        })
        .collect::<Vec<_>>();

    let mut groups = vec![
        FilterGroup {
            title: "Response codes",
            items: codes,
        },
        FilterGroup {
            title: "Indexability",
            items: vec![
                fixed(PageFilter::Indexable, "bg-ok"),
                fixed(PageFilter::NonIndexable, "bg-ghost"),
            ],
        },
        FilterGroup {
            title: "Content type",
            items: vec![
                fixed(PageFilter::Html, "bg-blue"),
                fixed(PageFilter::Image, "bg-accent"),
                fixed(PageFilter::Other, "bg-ghost"),
            ],
        },
    ];
    if !issues.is_empty() {
        groups.push(FilterGroup {
            title: "Issues",
            items: issues,
        });
    }
    groups
}

#[allow(clippy::too_many_arguments)]
fn rows_fragment(
    base: &str,
    filter: PageFilter,
    q: &str,
    sel: Option<u64>,
    mut batch: Vec<GridRow>,
    key_pages: &HashSet<u64>,
    first: bool,
    info: Option<InfoSpan>,
) -> RowsFragment {
    let more = if batch.len() > PAGE_SIZE {
        batch.truncate(PAGE_SIZE);
        batch.last().map(|last| {
            let mut url = format!("{base}/explorer/rows?filter={}", filter.key());
            if !q.is_empty() {
                url.push_str("&q=");
                url.push_str(&urlencode(q));
            }
            url.push_str(&format!("&after={}", last.id));
            url
        })
    } else {
        None
    };
    RowsFragment {
        empty: first && batch.is_empty(),
        rows: batch
            .iter()
            .map(|r| row_view(base, r, sel == Some(r.url_hash), key_pages))
            .collect(),
        more,
        info,
    }
}

/// A 2xx response that reads as an HTML page (no content type counts, as in the checks).
fn is_html_ok(status: u16, content_type: Option<&str>) -> bool {
    (200..300).contains(&status)
        && content_type.is_none_or(|ct| {
            let ct = ct.to_ascii_lowercase();
            ct.contains("text/html") || ct.contains("application/xhtml+xml")
        })
}

fn status_tone(status: u16) -> &'static str {
    match status {
        200..=299 => "t-ok",
        300..=399 => "t-warn",
        400..=499 => "t-err",
        500..=599 => "t-5xx",
        _ => "t-muted",
    }
}

fn status_colour(status: u16) -> &'static str {
    match status {
        200..=299 => "c-ok",
        300..=399 => "c-warn",
        400..=599 => "c-err",
        _ => "c-muted",
    }
}

/// `301 Moved Permanently`; `No response` for status 0.
fn status_line(status: u16, indexability: Indexability) -> String {
    if status == 0 {
        return if indexability == Indexability::BlockedByRobots {
            "Not fetched (blocked by robots.txt)".to_owned()
        } else {
            "No response".to_owned()
        };
    }
    match StatusCode::from_u16(status)
        .ok()
        .and_then(|c| c.canonical_reason())
    {
        Some(reason) => format!("{status} {reason}"),
        None => status.to_string(),
    }
}

fn indexability_view(i: Indexability) -> (&'static str, &'static str) {
    match i {
        Indexability::Indexable => ("Indexable", "c-ok"),
        Indexability::Noindex => ("Noindex", "c-warn"),
        Indexability::Canonicalised => ("Canonicalised", "c-warn"),
        Indexability::Redirected => ("Redirected", "c-warn"),
        Indexability::ClientError => ("Client error", "c-err"),
        Indexability::ServerError => ("Server error", "c-err"),
        Indexability::BlockedByRobots => ("Blocked by robots.txt", "c-muted"),
    }
}

/// Path plus query of a URL, what the Address column shows.
fn path_of(url: &str) -> String {
    match Url::parse(url) {
        Ok(u) => match u.query() {
            Some(q) => format!("{}?{q}", u.path()),
            None => u.path().to_owned(),
        },
        Err(_) => url.to_owned(),
    }
}

fn row_view(base: &str, r: &GridRow, selected: bool, key_pages: &HashSet<u64>) -> RowView {
    let html_ok = is_html_ok(r.status, r.content_type.as_deref());
    let (index_label, index_class) = indexability_view(r.indexability);
    let title = r.title.as_deref().map(str::trim).unwrap_or_default();
    let title_chars = title.chars().count();
    let (title_text, title_class) = if title.is_empty() && html_ok {
        ("Missing".to_owned(), "c-err")
    } else {
        (title.to_owned(), "")
    };
    let (words, words_class) = match r.word_count {
        Some(n) if html_ok => (
            fmt::thousands(n),
            if n < THIN_WORDS { "c-warn" } else { "" },
        ),
        _ => ("—".to_owned(), "c-ghost"),
    };
    let (depth, depth_class) = match r.depth {
        Some(d) => (d.to_string(), if d > DEEP_CLICKS { "c-warn" } else { "" }),
        None => ("—".to_owned(), "c-ghost"),
    };
    let (response, response_class) = match r.response_ms {
        Some(ms) => (
            format!("{} ms", fmt::thousands(ms)),
            if ms > SLOW_MS { "c-err" } else { "" },
        ),
        None => ("—".to_owned(), "c-ghost"),
    };
    let status = match r.status {
        0 if r.indexability == Indexability::BlockedByRobots => "—".to_owned(),
        0 => "ERR".to_owned(),
        s => s.to_string(),
    };
    let h = hex(r.url_hash);
    RowView {
        url: r.url.clone(),
        path: path_of(&r.url),
        status,
        status_tone: if r.status == 0 && r.indexability != Indexability::BlockedByRobots {
            "t-err"
        } else {
            status_tone(r.status)
        },
        index_label,
        index_class,
        title: title_text,
        title_class,
        title_len: if title.is_empty() {
            String::new()
        } else {
            title_chars.to_string()
        },
        title_len_class: if title_chars > TITLE_MAX_CHARS {
            "c-warn"
        } else {
            ""
        },
        words,
        words_class,
        depth,
        depth_class,
        inlinks: fmt::thousands(r.inlinks),
        response,
        response_class,
        star: StarButton {
            base: base.to_owned(),
            hex: h.clone(),
            on: key_pages.contains(&r.url_hash),
            pop: false,
            at: "row",
        },
        selected,
        detail_href: format!("{base}/explorer/detail?sel={h}"),
    }
}

async fn detail_panel(
    state: &AppState,
    site: &Site,
    crawl: &Crawl,
    page: Option<PageDetail>,
    tab: Tab,
    key_pages: &HashSet<u64>,
) -> Result<DetailPanel, AppError> {
    let Some(p) = page else {
        return Ok(DetailPanel { page: None });
    };
    let base = format!("/s/{}", site.id);
    let h = hex(p.url_hash);
    let tabs = Tab::ALL
        .iter()
        .map(|&t| TabLink {
            label: t.label(),
            href: format!("{base}/explorer/detail?sel={h}&tab={}", t.key()),
            active: t == tab,
            count: (t == Tab::Inlinks).then(|| fmt::thousands(p.inlinks)),
        })
        .collect();
    let mut view = DetailView {
        hex: h.clone(),
        url: p.url.clone(),
        tab: tab.key(),
        tabs,
        star: StarButton {
            base: base.clone(),
            hex: h,
            on: key_pages.contains(&p.url_hash),
            pop: false,
            at: "detail",
        },
        details: None,
        serp: None,
        inlinks: None,
        headers: None,
    };
    match tab {
        Tab::Details => view.details = Some(details_tab(&base, &p)),
        Tab::Serp => view.serp = Some(serp_view(&p)),
        Tab::Inlinks => {
            let links =
                codoseo_store::explorer::inlinks(&state.pool, crawl.id, p.url_hash, INLINKS_SHOWN)
                    .await?;
            view.inlinks = Some(inlinks_tab(&base, links));
        }
        Tab::Headers => view.headers = Some(headers_text(&p)),
    }
    Ok(DetailPanel { page: Some(view) })
}

fn field(k: &'static str, v: impl Into<String>, class: &'static str) -> Field {
    Field {
        k,
        v: v.into(),
        class,
        href: None,
    }
}

/// Text or a dash when it's missing.
fn or_dash(k: &'static str, v: Option<&str>) -> Field {
    match v.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => field(k, s, ""),
        None => field(k, "—", "c-ghost"),
    }
}

/// `58 chars`, `72 chars · too long`.
fn length_field(k: &'static str, text: Option<&str>, (min, max): (usize, usize)) -> Field {
    let n = text.map_or(0, |t| t.trim().chars().count());
    if n == 0 {
        return field(k, "—", "c-ghost");
    }
    let unit = if n == 1 { "char" } else { "chars" };
    if n > max {
        field(k, format!("{n} {unit} · too long"), "c-warn")
    } else if n < min {
        field(k, format!("{n} {unit} · too short"), "c-warn")
    } else {
        field(k, format!("{n} {unit}"), "")
    }
}

/// Missing text on a readable HTML page is an issue (red); elsewhere it's just absent.
fn text_field(k: &'static str, text: Option<&str>, html_ok: bool) -> Field {
    match text.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => field(k, s, ""),
        None if html_ok => field(k, "Missing", "c-err"),
        None => field(k, "—", "c-ghost"),
    }
}

fn details_tab(base: &str, p: &PageDetail) -> DetailsTab {
    let html_ok = is_html_ok(p.status, p.content_type.as_deref());
    let (index_label, index_class) = indexability_view(p.indexability);
    let canonical = match p
        .canonical
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        Some(c) => Field {
            href: Some(c.to_owned()),
            ..field("Canonical", c, if c == p.url { "" } else { "c-warn" })
        },
        None => field("Canonical", "—", "c-ghost"),
    };
    let chain = if p.redirect_chain.is_empty() {
        field("Redirect target / chain", "—", "c-ghost")
    } else {
        let hops = p
            .redirect_chain
            .iter()
            .map(|(status, url)| format!("{status} {url}"))
            .collect::<Vec<_>>()
            .join(" → ");
        let n = p.redirect_chain.len();
        field(
            "Redirect target / chain",
            format!("{hops} ({n} hop{})", if n == 1 { "" } else { "s" }),
            if n > 1 { "c-warn" } else { "" },
        )
    };
    let fields = vec![
        Field {
            href: Some(p.url.clone()),
            ..field("Address", p.url.clone(), "")
        },
        field(
            "Status",
            status_line(p.status, p.indexability),
            status_colour(p.status),
        ),
        field("Indexability", index_label, index_class),
        or_dash("Content type", p.content_type.as_deref()),
        text_field("Title 1", p.title.as_deref(), html_ok),
        length_field(
            "Title length",
            p.title.as_deref(),
            (TITLE_MIN_CHARS, TITLE_MAX_CHARS),
        ),
        text_field("Meta description", p.meta_description.as_deref(), html_ok),
        length_field(
            "Description length",
            p.meta_description.as_deref(),
            (DESCRIPTION_MIN_CHARS, DESCRIPTION_MAX_CHARS),
        ),
        text_field("H1-1", p.h1.first().map(String::as_str), html_ok),
        field("H2 count", p.h2.len().to_string(), ""),
        canonical,
        or_dash("Meta robots", p.meta_robots.as_deref()),
        or_dash("X-Robots-Tag", p.x_robots_tag.as_deref()),
        match p.word_count {
            Some(n) if html_ok => field(
                "Word count",
                fmt::thousands(n),
                if n < THIN_WORDS { "c-warn" } else { "" },
            ),
            _ => field("Word count", "—", "c-ghost"),
        },
        match p.depth {
            Some(d) => field(
                "Crawl depth",
                format!("{d} click{}", if d == 1 { "" } else { "s" }),
                if d > DEEP_CLICKS { "c-warn" } else { "" },
            ),
            None => field("Crawl depth", "Not linked (sitemap only)", "c-muted"),
        },
        field(
            "Inlinks / outlinks",
            format!(
                "{} / {} internal · {} external",
                fmt::thousands(p.inlinks),
                fmt::thousands(p.outlinks_internal),
                fmt::thousands(p.outlinks_external)
            ),
            "",
        ),
        match p.response_ms {
            Some(ms) => field(
                "Response time",
                format!("{} ms", fmt::thousands(ms)),
                if ms > SLOW_MS { "c-err" } else { "" },
            ),
            None => field("Response time", "—", "c-ghost"),
        },
        match p.size_bytes {
            Some(b) => field("Size", kilobytes(b), ""),
            None => field("Size", "—", "c-ghost"),
        },
        field("In sitemap", if p.in_sitemap { "Yes" } else { "No" }, ""),
        chain,
    ];

    let mut checks: Vec<CheckId> = IssueBits(p.issues)
        .iter()
        .filter_map(CheckId::from_bit)
        .collect();
    checks.sort_by_key(|id| (def(*id).severity, *id));
    let issues = checks
        .into_iter()
        .map(|id| IssueChip {
            title: def(id).title,
            sev: severity_badge(def(id).severity),
            href: format!("{base}/explorer?filter=check:{}", id.slug()),
        })
        .collect();
    DetailsTab { fields, issues }
}

/// `23.4 KB`
fn kilobytes(bytes: u64) -> String {
    let tenths = (bytes.saturating_mul(10) + 512) / 1024;
    let whole = i64::try_from(tenths / 10).unwrap_or(i64::MAX);
    format!("{}.{} KB", fmt::thousands(whole), tenths % 10)
}

/// Runs of whitespace as one space, the way the results page renders text.
fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn meter(
    label: &'static str,
    text: Option<&str>,
    limit: u32,
    measure: fn(&str) -> u32,
    missing: &'static str,
) -> MeterView {
    // The bar's full width is 125% of the limit, so the limit marker sits at 80%.
    let scale = limit * 5 / 4;
    match text {
        None => MeterView {
            label,
            value: format!("0 / {limit} px"),
            value_class: "c-err",
            bar: "bg-err",
            pct: 100,
            limit_pct: 80,
            note: missing,
        },
        Some(t) => {
            let px = measure(t);
            let over = px > limit;
            MeterView {
                label,
                value: format!("{} / {limit} px", fmt::thousands(px)),
                value_class: if over { "c-warn" } else { "" },
                bar: if over { "bg-warn" } else { "bg-ok" },
                pct: (px.saturating_mul(100) / scale.max(1)).min(100),
                limit_pct: 80,
                note: if over {
                    "Will be truncated in results"
                } else {
                    "Fits in results"
                },
            }
        }
    }
}

fn serp_view(p: &PageDetail) -> SerpView {
    let title = p.title.as_deref().map(collapse).filter(|t| !t.is_empty());
    let description = p
        .meta_description
        .as_deref()
        .map(collapse)
        .filter(|d| !d.is_empty());
    let (crumb_host, crumb_path) = match Url::parse(&p.url) {
        Ok(u) => (
            u.host_str().unwrap_or_default().to_owned(),
            u.path_segments()
                .map(|segs| {
                    segs.filter(|s| !s.is_empty())
                        .map(|s| format!(" › {s}"))
                        .collect::<String>()
                })
                .unwrap_or_default(),
        ),
        Err(_) => (p.url.clone(), String::new()),
    };
    SerpView {
        crumb_host,
        crumb_path,
        title: title
            .as_deref()
            .map(|t| truncate_to_px(t, TITLE_LIMIT_PX, title_px).0),
        description: description
            .as_deref()
            .map(|d| truncate_to_px(d, DESCRIPTION_LIMIT_PX, description_px).0),
        meters: vec![
            meter(
                "Title width",
                title.as_deref(),
                TITLE_LIMIT_PX,
                title_px,
                "Missing title",
            ),
            meter(
                "Description width",
                description.as_deref(),
                DESCRIPTION_LIMIT_PX,
                description_px,
                "Missing meta description",
            ),
        ],
    }
}

fn inlinks_tab(base: &str, links: Vec<Inlink>) -> InlinksTab {
    let rows = links
        .into_iter()
        .map(|l| {
            let href = match Url::parse(&l.from_url) {
                Ok(u) => format!(
                    "{base}/explorer?sel={}",
                    hex(codoseo_core::url::url_hash(&u))
                ),
                Err(_) => l.from_url.clone(),
            };
            let (anchor, anchor_class) = match l
                .anchor_text
                .as_deref()
                .map(str::trim)
                .filter(|a| !a.is_empty())
            {
                Some(a) => (a.to_owned(), ""),
                None => ("No anchor text".to_owned(), "c-ghost"),
            };
            InlinkView {
                from: l.from_url,
                href,
                anchor,
                anchor_class,
                kind: if l.nofollow { "Nofollow" } else { "Follow" },
                kind_class: if l.nofollow { "c-warn" } else { "c-ok" },
            }
        })
        .collect();
    InlinksTab {
        rows,
        note: format!(
            "Up to {MAX_INLINK_SAMPLES} sample inlinks are kept per page, plus every link to a \
             broken or redirecting page."
        ),
    }
}

/// The response rebuilt from what the crawl stored (raw headers are never kept).
fn headers_text(p: &PageDetail) -> String {
    let mut lines = Vec::new();
    lines.push(if p.status == 0 {
        format!("HTTP/1.1 —  {}", status_line(0, p.indexability))
    } else {
        format!("HTTP/1.1 {}", status_line(p.status, p.indexability))
    });
    if let Some(ct) = p.content_type.as_deref().filter(|c| !c.is_empty()) {
        lines.push(format!("content-type: {ct}"));
    }
    if p.status != 0
        && let Some(size) = p.size_bytes
    {
        lines.push(format!("content-length: {size}"));
    }
    if let Some(x) = p.x_robots_tag.as_deref().filter(|x| !x.is_empty()) {
        lines.push(format!("x-robots-tag: {x}"));
    }
    // Each hop is the URL that answered with that status, so the page's own redirect points
    // at the next hop.
    if (300..400).contains(&p.status)
        && let Some((_, next)) = p.redirect_chain.get(1)
    {
        lines.push(format!("location: {next}"));
    }
    if !p.redirect_chain.is_empty() {
        lines.push(String::new());
        lines.push("# redirect chain".to_owned());
        for (i, (status, url)) in p.redirect_chain.iter().enumerate() {
            lines.push(format!("{}. {status} {url}", i + 1));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_round_trip_as_sixteen_hex_digits() {
        for h in [0, 1, 0xab, u64::MAX, 0x00ab_12cd_0000_ffff] {
            let s = hex(h);
            assert_eq!(s.len(), 16);
            assert_eq!(parse_hex(&s), Some(h));
        }
        assert_eq!(parse_hex("abc"), None);
        assert_eq!(parse_hex("zzzzzzzzzzzzzzzz"), None);
        assert_eq!(parse_hex("+bcdef0123456789"), None);
    }

    #[test]
    fn explorer_urls_leave_out_defaults() {
        let base = "/s/x";
        assert_eq!(
            explorer_url(base, PageFilter::All, "", None, Tab::Details),
            "/s/x/explorer?filter=all"
        );
        assert_eq!(
            explorer_url(base, PageFilter::Status4xx, "a b&c", Some(0xab), Tab::Serp),
            "/s/x/explorer?filter=s4&q=a+b%26c&sel=00000000000000ab&tab=serp"
        );
    }

    #[test]
    fn sizes_in_kilobytes() {
        assert_eq!(kilobytes(0), "0.0 KB");
        assert_eq!(kilobytes(24_000), "23.4 KB");
        assert_eq!(kilobytes(2_048_000), "2,000.0 KB");
    }

    #[test]
    fn address_column_shows_path_and_query() {
        assert_eq!(path_of("https://e.com/"), "/");
        assert_eq!(path_of("https://e.com/a/b?x=1"), "/a/b?x=1");
        assert_eq!(path_of("not a url"), "not a url");
    }
}
