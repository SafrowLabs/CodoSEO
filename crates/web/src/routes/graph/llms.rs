//! `/s/{site}/graph/llms`: the llms.txt as a tree (title, sections, links with their status) and the top pages it leaves out.
//!
//! The file comes from the site's newest stored report that fetched it
//! ([`geo::latest_llms_txt`]); the pages it leaves out are the indexable pages of the latest
//! finished crawl with the highest link score ([`codoseo_store::graph::load`]) that no link in the
//! file points to. The tree is an inline SVG laid out here, row by row.

use std::collections::{HashMap, HashSet};

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use codoseo_core::url::{normalize, url_hash};
use codoseo_geo::llms::{LlmsCheckId, LlmsState, LlmsTree, LlmsTxtReport};
use codoseo_store::sites::Site;
use codoseo_store::{geo, hash};
use sqlx::FromRow;
use url::Url;
use uuid::Uuid;

use super::{Empty, Tab, TabLink};
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::layout::Shell;
use crate::render::html;
use crate::routes::ai_access::plural;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/s/{site}/graph/llms", get(llms_page))
}

/// Links a section shows before "+N more".
const SECTION_LINKS: usize = 12;
/// Sections the tree shows before "+N more sections".
const SECTIONS: usize = 12;
/// Top pages by link score the "Not listed" group shows.
const NOT_LISTED: usize = 5;
/// Pages of highest link score read to find the ones the file leaves out.
const CANDIDATES: usize = 60;
/// Pages the starter file lists when there is no llms.txt.
const STARTER: usize = 8;

// The tree's geometry, in viewBox units; the link columns (dot 560, name 576, URL 750, badge
// 970) are fixed in the template.
const WIDTH: u32 = 1060;
const ROW: u32 = 34;
const SECTION_GAP: u32 = 18;
const TOP: u32 = 40;
const ROOT_X: u32 = 20;
const ROOT_W: u32 = 216;
const SECTION_X: u32 = 324;
const SECTION_W: u32 = 176;
const DOT_X: u32 = 560;

#[derive(Template)]
#[template(path = "graph/llms.html")]
pub struct LlmsTab {
    pub shell: Shell,
    pub base: String,
    pub tabs: Vec<TabLink>,
    pub empty: Option<Empty>,
    pub view: Option<LlmsView>,
}

/// What the tab shows once there is a finished crawl.
pub enum LlmsView {
    /// No stored report has fetched the file yet.
    NotChecked,
    /// The site serves no llms.txt: what it is, and a starter file from the top pages.
    Missing {
        why: String,
        starter: String,
    },
    /// The file could not be fetched.
    Unavailable {
        why: String,
    },
    /// Fetched by a crawl from before the sections were kept.
    NoTree {
        summary: String,
    },
    Tree(Box<TreeView>),
}

pub struct TreeView {
    pub svg: TreeSvg,
    pub checks: Vec<CheckRow>,
    pub passed: usize,
    /// `15 links in 4 sections · 3 with problems`
    pub caption: String,
    /// The suggested edit, line by line; empty when there is nothing to change.
    pub edit: Vec<EditLine>,
    /// The same edit as plain text, for the copy button.
    pub edit_text: String,
    pub file_url: String,
}

pub struct CheckRow {
    pub label: &'static str,
    pub ok: bool,
    /// `Pass`, `Fail`, `2 fail`
    pub verdict: String,
    pub detail: String,
}

pub struct EditLine {
    /// `is-ctx`, `is-del`, `is-add`
    pub class: &'static str,
    pub text: String,
}

/// The tree as an SVG: every position is computed here, the template only draws.
pub struct TreeSvg {
    pub width: u32,
    pub height: u32,
    pub label: String,
    pub root: RootCard,
    pub edges: Vec<Edge>,
    pub sections: Vec<SectionNode>,
    pub links: Vec<LinkNode>,
    pub more: Vec<MoreRow>,
    pub missing: Option<MissingGroup>,
}

pub struct RootCard {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub title: String,
    /// The `>` summary, wrapped: each line with its baseline.
    pub summary: Vec<(u32, String)>,
    /// `/llms.txt · 3.1 KB`
    pub meta: String,
}

pub struct Edge {
    pub d: String,
    /// `gl-edge` plus `is-optional` or `is-missing`.
    pub class: &'static str,
}

pub struct SectionNode {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub title: String,
    pub full: String,
    pub count: usize,
    pub optional: bool,
}

pub struct LinkNode {
    /// The row's baseline centre.
    pub y: u32,
    pub name: String,
    pub url: String,
    pub full: String,
    /// `is-ok`, `is-broken`, `is-redirect`, `is-noindex`, `is-unchecked`
    pub class: &'static str,
    pub badge: Option<Badge>,
    pub optional: bool,
}

pub struct Badge {
    pub text: String,
    pub w: u32,
}

pub struct MoreRow {
    pub y: u32,
    pub text: String,
}

pub struct MissingGroup {
    /// The separator line's y.
    pub y: u32,
    pub rows: Vec<MissingRow>,
}

pub struct MissingRow {
    pub y: u32,
    pub name: String,
    pub url: String,
    pub full: String,
    pub score: u8,
}

async fn llms_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
) -> Result<Response, AppError> {
    let ctx = super::load(&state, &user, &site, Tab::Llms).await?;
    let view = match &ctx.latest {
        Some(crawl) => Some(build(&state, &ctx.site, crawl.id).await?),
        None => None,
    };
    Ok(html(&LlmsTab {
        shell: ctx.shell,
        base: ctx.base,
        tabs: ctx.tabs,
        empty: ctx.empty,
        view,
    })?
    .into_response())
}

async fn build(state: &AppState, site: &Site, crawl_id: Uuid) -> Result<LlmsView, AppError> {
    let Some(report) = geo::latest_llms_txt(&state.pool, site.id).await? else {
        return Ok(LlmsView::NotChecked);
    };
    let origin = Url::parse(&site.start_url).ok();
    let file_url = origin
        .as_ref()
        .and_then(|o| o.join("/llms.txt").ok())
        .map_or_else(|| "/llms.txt".to_owned(), |u| u.to_string());
    match report.state {
        LlmsState::Absent | LlmsState::SoftNotFound => {
            let top = top_pages(state, crawl_id, &HashSet::new(), STARTER).await?;
            let home = home_title(state, crawl_id, origin.as_ref()).await?;
            let why = match report.state {
                LlmsState::SoftNotFound => format!(
                    "/llms.txt answered HTTP {} with a web page, so there is no file.",
                    report.status
                ),
                _ => format!("/llms.txt answered HTTP {}.", report.status),
            };
            Ok(LlmsView::Missing {
                why,
                starter: starter(&home, &top, origin.as_ref()),
            })
        }
        LlmsState::Unavailable => Ok(LlmsView::Unavailable {
            why: report.summary(),
        }),
        LlmsState::Available => match &report.tree {
            None => Ok(LlmsView::NoTree {
                summary: report.summary(),
            }),
            Some(tree) => {
                let base = Url::parse(&file_url).ok();
                let listed = listed(tree, base.as_ref());
                let linked =
                    pages_by_hash(state, crawl_id, &listed.iter().copied().collect::<Vec<_>>())
                        .await?
                        .into_iter()
                        .map(|p| (p.url_hash, p))
                        .collect();
                let top = top_pages(state, crawl_id, &listed, NOT_LISTED).await?;
                Ok(LlmsView::Tree(Box::new(tree_view(
                    &report,
                    tree,
                    base.as_ref(),
                    &linked,
                    &top,
                    file_url,
                ))))
            }
        },
    }
}

/// One crawled page, as the tree needs it.
#[derive(Debug, Clone)]
pub struct PageInfo {
    pub url_hash: u64,
    pub url: String,
    pub title: Option<String>,
    pub indexable: bool,
    pub noindex: bool,
    pub status: u16,
    pub redirect_target: Option<String>,
    pub link_score: u8,
}

#[derive(FromRow)]
struct PageRow {
    url_hash: i64,
    url: String,
    title: Option<String>,
    indexability: String,
    status: i16,
    redirect_target: Option<String>,
}

async fn pages_by_hash(
    state: &AppState,
    crawl_id: Uuid,
    hashes: &[u64],
) -> Result<Vec<PageInfo>, sqlx::Error> {
    if hashes.is_empty() {
        return Ok(Vec::new());
    }
    let db: Vec<i64> = hashes.iter().map(|h| hash::to_db(*h)).collect();
    let rows: Vec<PageRow> = sqlx::query_as(
        "SELECT url_hash, url, title, indexability::text AS indexability, status, redirect_target \
         FROM pages WHERE crawl_id = $1 AND url_hash = ANY($2)",
    )
    .bind(crawl_id)
    .bind(&db)
    .fetch_all(&state.pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| PageInfo {
            url_hash: hash::from_db(r.url_hash),
            url: r.url,
            title: r.title,
            indexable: r.indexability == "indexable",
            noindex: r.indexability == "noindex",
            status: u16::try_from(r.status).unwrap_or(0),
            redirect_target: r.redirect_target,
            link_score: 0,
        })
        .collect())
}

/// Up to `n` indexable 200 pages of the crawl with the highest link score, the home page and the
/// `listed` ones left out, best first. None when the crawl kept no link graph.
async fn top_pages(
    state: &AppState,
    crawl_id: Uuid,
    listed: &HashSet<u64>,
    n: usize,
) -> Result<Vec<PageInfo>, AppError> {
    let Some(graph) = codoseo_store::graph::load(&state.pool, crawl_id).await? else {
        return Ok(Vec::new());
    };
    let mut ranked: Vec<(u64, u8)> = graph
        .pages
        .iter()
        .filter(|p| !listed.contains(&p.url_hash))
        .map(|p| (p.url_hash, p.link_score))
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked.truncate(CANDIDATES);
    let scores: HashMap<u64, u8> = ranked.iter().copied().collect();
    let mut pages = pages_by_hash(
        state,
        crawl_id,
        &ranked.iter().map(|(h, _)| *h).collect::<Vec<_>>(),
    )
    .await?;
    pages.retain(|p| p.indexable && p.status == 200 && !is_home(&p.url));
    for p in &mut pages {
        p.link_score = scores.get(&p.url_hash).copied().unwrap_or(0);
    }
    pages.sort_by(|a, b| b.link_score.cmp(&a.link_score).then(a.url.cmp(&b.url)));
    pages.truncate(n);
    Ok(pages)
}

fn is_home(url: &str) -> bool {
    Url::parse(url).is_ok_and(|u| u.path() == "/" && u.query().is_none())
}

/// The home page's title, else the domain: the starter file's H1.
async fn home_title(
    state: &AppState,
    crawl_id: Uuid,
    origin: Option<&Url>,
) -> Result<String, sqlx::Error> {
    let Some(origin) = origin else {
        return Ok("Your site".to_owned());
    };
    let home = origin.join("/").ok().and_then(|u| normalize(&u, "/"));
    let domain = origin.host_str().unwrap_or("Your site").to_owned();
    let Some(home) = home else {
        return Ok(domain);
    };
    let pages = pages_by_hash(state, crawl_id, &[url_hash(&home)]).await?;
    Ok(pages
        .into_iter()
        .find_map(|p| p.title)
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
        .unwrap_or(domain))
}

/// URL hashes the file links to: each link resolved against the file, and a `.md` link also
/// counts for the page it is the Markdown of (`/a.md` for `/a`, `/a/index.html.md` for `/a/`).
pub fn listed(tree: &LlmsTree, base: Option<&Url>) -> HashSet<u64> {
    let mut out = HashSet::new();
    let Some(base) = base else {
        return out;
    };
    for l in tree.sections.iter().flat_map(|s| &s.links) {
        let Some(url) = normalize(base, &l.url) else {
            continue;
        };
        out.insert(url_hash(&url));
        let path = url.path().to_owned();
        let page = path
            .strip_suffix("index.html.md")
            .or_else(|| path.strip_suffix(".md"));
        if let Some(page) = page {
            let mut u = url.clone();
            u.set_path(page);
            out.insert(url_hash(&u));
        }
    }
    out
}

/// How a link shows: its dot class and its badge.
fn link_look(status: Option<u16>, page: Option<&PageInfo>) -> (&'static str, Option<String>) {
    match status {
        None => ("is-unchecked", None),
        Some(0) => ("is-broken", Some("error".to_owned())),
        Some(s @ 300..=399) => ("is-redirect", Some(s.to_string())),
        Some(s) if s >= 400 => ("is-broken", Some(s.to_string())),
        Some(_) if page.is_some_and(|p| p.noindex) => ("is-noindex", Some("noindex".to_owned())),
        Some(_) => ("is-ok", None),
    }
}

/// `text` cut to `max` characters with an ellipsis.
pub(super) fn short(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// A URL as the tree shows it: the path on the site's own origin, the URL without its scheme
/// elsewhere.
fn display_url(href: &str, base: Option<&Url>) -> String {
    match base.and_then(|b| normalize(b, href).map(|u| (b, u))) {
        Some((b, u)) if u.origin() == b.origin() => match u.query() {
            Some(q) => format!("{}?{q}", u.path()),
            None => u.path().to_owned(),
        },
        Some((_, u)) => {
            let s = u.to_string();
            s.split_once("://")
                .map_or(s.clone(), |(_, rest)| rest.to_owned())
        }
        None => href.to_owned(),
    }
}

/// Words of `text` in lines of at most `width` characters, at most `lines` of them, the last
/// cut with an ellipsis when there is more.
fn wrap(text: &str, width: usize, lines: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut words = text.split_whitespace().peekable();
    while let Some(w) = words.next() {
        if !cur.is_empty() && cur.chars().count() + 1 + w.chars().count() > width {
            out.push(std::mem::take(&mut cur));
            if out.len() == lines {
                let last = out.pop().unwrap_or_default();
                out.push(short(&format!("{last} {w}"), width));
                return out;
            }
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(w);
        if words.peek().is_none() {
            out.push(short(&cur, width));
        }
    }
    out
}

/// `3.1 KB`
fn kilobytes(bytes: u64) -> String {
    let tenths = (bytes.saturating_mul(10) + 512) / 1024;
    format!("{}.{} KB", tenths / 10, tenths % 10)
}

/// A connector from `(x1, y1)` to `(x2, y2)`, curving halfway across.
fn curve(x1: u32, y1: u32, x2: u32, y2: u32) -> String {
    let mid = (x1 + x2) / 2;
    format!("M{x1} {y1}C{mid} {y1} {mid} {y2} {x2} {y2}")
}

fn badge(text: String) -> Badge {
    let w = 14 + 7 * u32::try_from(text.chars().count()).unwrap_or(8);
    Badge { text, w }
}

pub fn tree_view(
    report: &LlmsTxtReport,
    tree: &LlmsTree,
    base: Option<&Url>,
    linked: &HashMap<u64, PageInfo>,
    top: &[PageInfo],
    file_url: String,
) -> TreeView {
    let page_of = |href: &str| {
        base.and_then(|b| normalize(b, href))
            .and_then(|u| linked.get(&url_hash(&u)))
    };
    let mut edges = Vec::new();
    let mut sections = Vec::new();
    let mut links = Vec::new();
    let mut more = Vec::new();
    let mut y = TOP;
    let mut centres = Vec::new();
    let mut problems = 0;
    let total: usize = tree.sections.iter().map(|s| s.links.len()).sum();
    for s in tree.sections.iter().take(SECTIONS) {
        let first = y;
        for l in s.links.iter().take(SECTION_LINKS) {
            let (class, text) = link_look(l.status, page_of(&l.url));
            if class != "is-ok" && class != "is-unchecked" {
                problems += 1;
            }
            let url = display_url(&l.url, base);
            links.push(LinkNode {
                y,
                name: short(if l.name.is_empty() { &url } else { &l.name }, 22),
                url: short(&url, 28),
                full: format!("{}: {}", l.name, l.url),
                class,
                badge: text.map(badge),
                optional: s.optional,
            });
            y += ROW;
        }
        if s.links.len() > SECTION_LINKS {
            more.push(MoreRow {
                y,
                text: format!("+{} more", s.links.len() - SECTION_LINKS),
            });
            y += ROW;
        }
        if y == first {
            // A section without links still gets its row.
            y += ROW;
        }
        let centre = (first + y - ROW) / 2;
        for l in links.iter().filter(|l| l.y >= first && l.y < y) {
            edges.push(Edge {
                d: curve(SECTION_X + SECTION_W, centre, DOT_X - 8, l.y),
                class: if s.optional {
                    "gl-edge is-optional"
                } else {
                    "gl-edge"
                },
            });
        }
        sections.push(SectionNode {
            x: SECTION_X,
            y: centre - 17,
            w: SECTION_W,
            title: short(&s.title, 15),
            full: s.title.clone(),
            count: s.links.len(),
            optional: s.optional,
        });
        centres.push((centre, s.optional));
        y += SECTION_GAP;
    }
    let hidden = tree.sections.len().saturating_sub(SECTIONS) + tree.more_sections as usize;
    if hidden > 0 {
        more.push(MoreRow {
            y,
            text: format!("+{} more", plural(hidden, "section", "sections")),
        });
        y += ROW;
    }

    let summary = tree
        .summary
        .as_deref()
        .map_or_else(Vec::new, |s| wrap(&format!("> {s}"), 30, 2));
    let root_h = 84 + 18 * u32::try_from(summary.len()).unwrap_or(0);
    let mid = match (centres.first(), centres.last()) {
        (Some(a), Some(b)) => (a.0 + b.0) / 2,
        _ => TOP + root_h / 2,
    };
    let root_y = mid.saturating_sub(root_h / 2).max(10);
    let root_mid = root_y + root_h / 2;
    for (c, optional) in &centres {
        edges.insert(
            0,
            Edge {
                d: curve(ROOT_X + ROOT_W, root_mid, SECTION_X, *c),
                class: if *optional {
                    "gl-edge gl-edge-trunk is-optional"
                } else {
                    "gl-edge gl-edge-trunk"
                },
            },
        );
    }
    let path = base.map_or_else(|| "/llms.txt".to_owned(), |b| b.path().to_owned());
    let root = RootCard {
        x: ROOT_X,
        y: root_y,
        w: ROOT_W,
        h: root_h,
        title: short(tree.title.as_deref().unwrap_or("No H1 title"), 20),
        summary: summary
            .into_iter()
            .zip((root_y + 56..).step_by(18))
            .map(|(line, y)| (y, line))
            .collect(),
        meta: format!("{path} · {}", kilobytes(tree.bytes)),
    };

    let missing = (!top.is_empty()).then(|| {
        let line = y + 4;
        let first = line + 30;
        let rows: Vec<MissingRow> = top
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let url = display_url(&p.url, base);
                let ry = first + ROW * u32::try_from(i).unwrap_or(0);
                edges.push(Edge {
                    d: curve(SECTION_X + SECTION_W, first, DOT_X - 8, ry),
                    class: "gl-edge is-missing",
                });
                MissingRow {
                    y: ry,
                    name: short(p.title.as_deref().unwrap_or(&url), 22),
                    url: short(&url, 28),
                    full: p.url.clone(),
                    score: p.link_score,
                }
            })
            .collect();
        y = first + ROW * u32::try_from(rows.len()).unwrap_or(0);
        MissingGroup { y: line, rows }
    });
    let height = y.max(root_y + root_h + 10) + 10;

    let mut label = format!(
        "llms.txt tree: {} with {} and {}",
        tree.title.as_deref().unwrap_or("no title"),
        plural(tree.sections.len(), "section", "sections"),
        plural(total + tree.more_links as usize, "link", "links"),
    );
    if problems > 0 {
        label.push_str(&format!(", {problems} with problems"));
    }
    if !top.is_empty() {
        label.push_str(&format!(
            ", and {} not listed",
            plural(top.len(), "top page", "top pages")
        ));
    }
    let mut caption = format!(
        "{} in {}",
        plural(total + tree.more_links as usize, "link", "links"),
        plural(tree.sections.len(), "section", "sections")
    );
    if problems > 0 {
        caption.push_str(&format!(" · {problems} with problems"));
    }

    let failing = report.link_summary.failing;
    let checks: Vec<CheckRow> = report
        .checks
        .iter()
        .map(|c| CheckRow {
            label: c.id.label(),
            ok: c.ok,
            verdict: match (c.ok, c.id) {
                (true, _) => "Pass".to_owned(),
                (false, LlmsCheckId::LinksResolve) if failing > 0 => format!("{failing} fail"),
                (false, _) => "Fail".to_owned(),
            },
            detail: c.detail.clone(),
        })
        .collect();
    let passed = checks.iter().filter(|c| c.ok).count();
    let edit = suggested_edit(tree, base, linked, top);
    let edit_text = edit
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    TreeView {
        svg: TreeSvg {
            width: WIDTH,
            height,
            label,
            root,
            edges,
            sections,
            links,
            more,
            missing,
        },
        checks,
        passed,
        caption,
        edit,
        edit_text,
        file_url,
    }
}

/// The section a page fits best: the one whose links share the most leading path segments with
/// it, else the first that is not Optional. `None` when the file has no such section.
pub fn fitting_section(tree: &LlmsTree, base: Option<&Url>, page: &str) -> Option<usize> {
    let segments = |u: &str| -> Vec<String> {
        Url::parse(u)
            .ok()
            .or_else(|| base.and_then(|b| normalize(b, u)))
            .map(|u| {
                u.path()
                    .split('/')
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    let want = segments(page);
    let mut best: Option<(usize, usize)> = None;
    for (i, s) in tree.sections.iter().enumerate() {
        if s.optional {
            continue;
        }
        let score = s
            .links
            .iter()
            .map(|l| {
                segments(&l.url)
                    .iter()
                    .zip(&want)
                    .take_while(|(a, b)| a == b)
                    .count()
            })
            .max()
            .unwrap_or(0);
        if score > 0 && best.is_none_or(|(_, b)| score > b) {
            best = Some((i, score));
        }
    }
    best.map(|(i, _)| i)
        .or_else(|| tree.sections.iter().position(|s| !s.optional))
}

/// Replace broken links with where they redirect to (when the crawl knows), drop the ones that
/// go nowhere, and add the top pages the file leaves out to the section that fits them best.
pub fn suggested_edit(
    tree: &LlmsTree,
    base: Option<&Url>,
    linked: &HashMap<u64, PageInfo>,
    top: &[PageInfo],
) -> Vec<EditLine> {
    let mut adds: HashMap<Option<usize>, Vec<&PageInfo>> = HashMap::new();
    for p in top {
        adds.entry(fitting_section(tree, base, &p.url))
            .or_default()
            .push(p);
    }
    let item = |name: &str, url: &str| format!("[{name}]({url})");
    let add = |p: &PageInfo| {
        let url = display_url(&p.url, base);
        let name = p.title.as_deref().map_or(url.clone(), |t| short(t, 60));
        EditLine {
            class: "is-add",
            text: format!("+ {}", item(&name, &url)),
        }
    };
    let mut out = Vec::new();
    for (i, s) in tree.sections.iter().enumerate() {
        let mut lines = Vec::new();
        for l in &s.links {
            let broken =
                matches!(l.status, Some(0) | Some(300..)) && !matches!(l.status, Some(304));
            if !broken {
                continue;
            }
            let page = base
                .and_then(|b| normalize(b, &l.url))
                .and_then(|u| linked.get(&url_hash(&u)));
            let target = page.and_then(|p| p.redirect_target.as_deref());
            let redirect = matches!(l.status, Some(300..=399));
            match (redirect, target) {
                (true, None) => continue,
                (_, Some(t)) => {
                    lines.push(EditLine {
                        class: "is-del",
                        text: format!("- {}", item(&l.name, &l.url)),
                    });
                    lines.push(EditLine {
                        class: "is-add",
                        text: format!("+ {}", item(&l.name, &display_url(t, base))),
                    });
                }
                (false, None) => lines.push(EditLine {
                    class: "is-del",
                    text: format!("- {}", item(&l.name, &l.url)),
                }),
            }
        }
        if let Some(ps) = adds.get(&Some(i)) {
            lines.extend(ps.iter().map(|p| add(p)));
        }
        if !lines.is_empty() {
            out.push(EditLine {
                class: "is-ctx",
                text: format!("## {}", s.title),
            });
            out.extend(lines);
        }
    }
    if let Some(ps) = adds.get(&None) {
        out.push(EditLine {
            class: "is-add",
            text: "+ ## Pages".to_owned(),
        });
        out.extend(ps.iter().map(|p| add(p)));
    }
    out
}

/// A first llms.txt from the home page's title and the top pages by link score.
pub fn starter(title: &str, top: &[PageInfo], origin: Option<&Url>) -> String {
    let mut out = format!(
        "# {}\n\n> One sentence on what the site is and who it is for.\n\n## Pages\n\n",
        title.trim()
    );
    if top.is_empty() {
        out.push_str("- [About](/about): what you do\n");
    }
    for p in top {
        let url = display_url(&p.url, origin);
        let name = p.title.as_deref().map_or(url.clone(), |t| short(t, 60));
        out.push_str(&format!("- [{name}]({url})\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use codoseo_geo::llms::{LlmsTreeLink, LlmsTreeSection};

    fn link(name: &str, url: &str, status: Option<u16>) -> LlmsTreeLink {
        LlmsTreeLink {
            name: name.to_owned(),
            url: url.to_owned(),
            status,
        }
    }

    fn tree() -> LlmsTree {
        LlmsTree {
            title: Some("Brightdesk".to_owned()),
            summary: Some("Shared inbox and help desk for support teams.".to_owned()),
            bytes: 3174,
            chars: 3100,
            sections: vec![
                LlmsTreeSection {
                    title: "Docs".to_owned(),
                    optional: false,
                    links: vec![
                        link("Quickstart", "/docs/start", Some(200)),
                        link("Rate limits", "/docs/api/rate-limits", Some(404)),
                        link("Macros", "/guides/macros", Some(301)),
                    ],
                },
                LlmsTreeSection {
                    title: "Guides".to_owned(),
                    optional: false,
                    links: vec![link("Migrate", "/guides/migrate.md", Some(200))],
                },
                LlmsTreeSection {
                    title: "Optional".to_owned(),
                    optional: true,
                    links: vec![link("Blog", "https://other.example/blog", None)],
                },
            ],
            more_links: 0,
            more_sections: 0,
        }
    }

    fn base() -> Url {
        Url::parse("https://brightdesk.example/llms.txt").unwrap()
    }

    fn page(url: &str, score: u8) -> PageInfo {
        let u = Url::parse(url).unwrap();
        PageInfo {
            url_hash: url_hash(&u),
            url: url.to_owned(),
            title: Some(format!("Title of {}", u.path())),
            indexable: true,
            noindex: false,
            status: 200,
            redirect_target: None,
            link_score: score,
        }
    }

    #[test]
    fn a_markdown_link_lists_its_page() {
        let l = listed(&tree(), Some(&base()));
        let u = |s: &str| url_hash(&Url::parse(s).unwrap());
        assert!(l.contains(&u("https://brightdesk.example/docs/start")));
        assert!(l.contains(&u("https://brightdesk.example/guides/migrate")));
        assert!(l.contains(&u("https://brightdesk.example/guides/migrate.md")));
        assert!(!l.contains(&u("https://brightdesk.example/pricing")));
    }

    #[test]
    fn a_page_goes_to_the_section_with_the_closest_paths() {
        let t = tree();
        let b = base();
        assert_eq!(
            fitting_section(&t, Some(&b), "https://brightdesk.example/guides/sla"),
            Some(0),
            "Docs links /guides/macros too, and comes first"
        );
        assert_eq!(
            fitting_section(&t, Some(&b), "https://brightdesk.example/docs/webhooks"),
            Some(0)
        );
        assert_eq!(
            fitting_section(&t, Some(&b), "https://brightdesk.example/pricing"),
            Some(0),
            "no match falls back to the first section that is not Optional"
        );
    }

    #[test]
    fn the_edit_replaces_redirects_drops_broken_links_and_adds_top_pages() {
        let t = tree();
        let b = base();
        let mut macros = page("https://brightdesk.example/guides/macros", 0);
        macros.redirect_target = Some("https://brightdesk.example/docs/macros".to_owned());
        let linked = HashMap::from([(macros.url_hash, macros)]);
        let top = [page("https://brightdesk.example/docs/webhooks", 70)];
        let edit: Vec<String> = suggested_edit(&t, Some(&b), &linked, &top)
            .into_iter()
            .map(|l| l.text)
            .collect();
        assert_eq!(
            edit,
            [
                "## Docs",
                "- [Rate limits](/docs/api/rate-limits)",
                "- [Macros](/guides/macros)",
                "+ [Macros](/docs/macros)",
                "+ [Title of /docs/webhooks](/docs/webhooks)",
            ]
        );
    }

    #[test]
    fn the_tree_lays_out_every_link_with_its_status() {
        let report = LlmsTxtReport {
            state: LlmsState::Available,
            status: 200,
            error: None,
            adopted: true,
            title: Some("Brightdesk".to_owned()),
            optional_section: true,
            checks: Vec::new(),
            link_summary: Default::default(),
            tree: Some(tree()),
        };
        let top = [page("https://brightdesk.example/pricing", 70)];
        let v = tree_view(
            &report,
            &tree(),
            Some(&base()),
            &HashMap::new(),
            &top,
            "https://brightdesk.example/llms.txt".to_owned(),
        );
        let classes: Vec<_> = v.svg.links.iter().map(|l| l.class).collect();
        assert_eq!(
            classes,
            ["is-ok", "is-broken", "is-redirect", "is-ok", "is-unchecked"]
        );
        assert_eq!(v.svg.sections.len(), 3);
        assert!(v.svg.sections[2].optional);
        assert_eq!(v.svg.links[4].url, "other.example/blog");
        assert_eq!(v.caption, "5 links in 3 sections · 2 with problems");
        let m = v.svg.missing.as_ref().unwrap();
        assert_eq!(m.rows[0].url, "/pricing");
        assert!(m.y > v.svg.links[4].y);
        assert!(v.svg.height > m.rows[0].y);
        assert_eq!(v.svg.root.meta, "/llms.txt · 3.1 KB");
    }

    #[test]
    fn wrap_keeps_to_its_lines() {
        assert_eq!(wrap("a bb ccc", 4, 2), ["a bb", "ccc"]);
        assert_eq!(wrap("aaaa bbbb cccc", 4, 2), ["aaaa", "bbb…"]);
        assert_eq!(short("abcdef", 4), "abc…");
    }
}
