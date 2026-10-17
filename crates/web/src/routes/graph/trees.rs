//! `/s/{site}/graph/tree`, `/folders` and `/map`: the crawl tree by clicks, the folder tree by URL and the force-directed bubble map, drawn in the browser by `graph.js` from `data.json`.
//!
//! `data.json` is the latest finished crawl's link graph in short keys: one node per page (its
//! crawl-tree parent, link score, status and depth) and the links beyond the tree as flat
//! `[from, to, ...]` pairs, with site-wide navigation left out. A big crawl is cut to the
//! [`MAX_NODES`] best-linked pages and their ancestors, and to [`MAX_LINKS`] extra links.
//! `page.json` is one page for the side panel: who links to it, with what anchor text, and its
//! route from home.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use codoseo_core::output::Edge;
use codoseo_core::page::Indexability;
use codoseo_store::crawls;
use codoseo_store::explorer::{GraphPage, graph_pages};
use codoseo_store::graph::{self, StoredPage};
use serde::{Deserialize, Serialize};
use url::Url;

use super::{Empty, Tab, TabLink};
use crate::auth::{CurrentUser, load_site};
use crate::error::AppError;
use crate::layout::Shell;
use crate::render::html;
use crate::routes::audit::site_id;
use crate::routes::explorer::{hex, parse_hex};
use crate::state::AppState;

/// Pages `data.json` sends at most: the best-linked ones and every page on their way from home.
const MAX_NODES: usize = 5_000;
/// Links beyond the crawl tree `data.json` sends at most.
const MAX_LINKS: usize = 30_000;
/// On a site with at least this many pages, a page half of them link to is navigation (a
/// header or footer link), so links to it are not drawn.
const NAV_MIN_PAGES: usize = 10;
/// Inlinks `page.json` lists, best-linked sources first.
const PAGE_INLINKS: usize = 20;
/// Clicks from home where a page counts as deep.
const DEEP_CLICKS: u32 = 4;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/s/{site}/graph/tree", get(tree_page))
        .route("/s/{site}/graph/folders", get(folders_page))
        .route("/s/{site}/graph/map", get(map_page))
        .route("/s/{site}/graph/data.json", get(data))
        .route("/s/{site}/graph/page.json", get(page))
}

/// Where a view's mount loads from. `v` (the crawl number) only keys the browser's copy, so a
/// new crawl is fetched afresh.
pub struct Mount {
    pub src: String,
    pub page: String,
}

impl Mount {
    fn new(ctx: &super::Ctx) -> Mount {
        let v = ctx.latest.as_ref().map_or(0, |c| c.number);
        Mount {
            src: format!("{}/graph/data.json?v={v}", ctx.base),
            page: format!("{}/graph/page.json", ctx.base),
        }
    }
}

#[derive(Template)]
#[template(path = "graph/tree.html")]
pub struct TreeTab {
    pub shell: Shell,
    pub base: String,
    pub tabs: Vec<TabLink>,
    pub empty: Option<Empty>,
    pub mount: Mount,
}

async fn tree_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
) -> Result<Response, AppError> {
    let ctx = super::load(&state, &user, &site, Tab::Tree).await?;
    Ok(html(&TreeTab {
        mount: Mount::new(&ctx),
        shell: ctx.shell,
        base: ctx.base,
        tabs: ctx.tabs,
        empty: ctx.empty,
    })?
    .into_response())
}

#[derive(Template)]
#[template(path = "graph/folders.html")]
pub struct FoldersTab {
    pub shell: Shell,
    pub base: String,
    pub tabs: Vec<TabLink>,
    pub empty: Option<Empty>,
    pub mount: Mount,
}

async fn folders_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
) -> Result<Response, AppError> {
    let ctx = super::load(&state, &user, &site, Tab::Folders).await?;
    Ok(html(&FoldersTab {
        mount: Mount::new(&ctx),
        shell: ctx.shell,
        base: ctx.base,
        tabs: ctx.tabs,
        empty: ctx.empty,
    })?
    .into_response())
}

#[derive(Template)]
#[template(path = "graph/map.html")]
pub struct MapTab {
    pub shell: Shell,
    pub base: String,
    pub tabs: Vec<TabLink>,
    pub empty: Option<Empty>,
    pub mount: Mount,
}

async fn map_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
) -> Result<Response, AppError> {
    let ctx = super::load(&state, &user, &site, Tab::Map).await?;
    Ok(html(&MapTab {
        mount: Mount::new(&ctx),
        shell: ctx.shell,
        base: ctx.base,
        tabs: ctx.tabs,
        empty: ctx.empty,
    })?
    .into_response())
}

/// `data.json`. `empty` is set, with no nodes, when the site has no finished crawl or the crawl
/// kept no graph (one finished before graphs were kept).
#[derive(Debug, Default, Serialize)]
pub struct Data {
    pub empty: bool,
    /// The home page's node.
    pub root: Option<u32>,
    pub nodes: Vec<Node>,
    /// Links beyond the crawl tree, as `[from, to, from, to, ...]` node indexes.
    pub e: Vec<u32>,
    pub stats: Stats,
    /// What was left out of a big crawl; `None` when nothing was.
    pub capped: Option<Capped>,
}

#[derive(Debug, Serialize)]
pub struct Node {
    /// The URL's path and query, or the whole URL on another host.
    pub u: String,
    /// The URL hash for `page.json`, as 16 hex digits.
    pub h: String,
    pub t: String,
    pub s: u16,
    /// `ok`, `noindex`, `redirect` or `broken`.
    pub x: &'static str,
    /// Clicks from home; `None` for a page found only in a sitemap.
    pub d: Option<u32>,
    /// The crawl-tree parent's node index.
    pub p: Option<u32>,
    /// Link score, 1..=100.
    pub k: u8,
    pub i: u32,
    /// Not linked from any page.
    pub o: bool,
}

/// The whole crawl, before any cap.
#[derive(Debug, Default, Serialize)]
pub struct Stats {
    pub pages: usize,
    pub links: usize,
    pub orphans: usize,
    pub broken: usize,
    pub redirects: usize,
    pub noindex: usize,
    /// Pages [`DEEP_CLICKS`] or more clicks from home.
    pub deep: usize,
    /// The deepest page's clicks from home.
    pub depth: u32,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Capped {
    /// Pages left out.
    pub pages: usize,
    /// Extra links left out (navigation is never counted).
    pub links: usize,
}

async fn data(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
) -> Result<Response, AppError> {
    let site = load_site(&state, &user, site_id(&site)?).await?;
    let Some(crawl) = crawls::latest_done(&state.pool, site.id).await? else {
        return Ok(Json(Data::empty()).into_response());
    };
    let Some(g) = graph::load(&state.pool, crawl.id).await? else {
        return Ok(Json(Data::empty()).into_response());
    };
    let details = by_hash(graph_pages(&state.pool, crawl.id, None).await?);
    Ok(Json(build(&g.pages, &g.edges, &details, MAX_NODES, MAX_LINKS)).into_response())
}

impl Data {
    fn empty() -> Data {
        Data {
            empty: true,
            ..Data::default()
        }
    }
}

fn by_hash(pages: Vec<GraphPage>) -> HashMap<u64, GraphPage> {
    let mut map = HashMap::with_capacity(pages.len());
    for p in pages {
        map.entry(p.url_hash).or_insert(p);
    }
    map
}

/// How a view colours a page.
fn category(p: &GraphPage) -> &'static str {
    match p.indexability {
        Indexability::Indexable => "ok",
        Indexability::Noindex | Indexability::Canonicalised | Indexability::BlockedByRobots => {
            "noindex"
        }
        Indexability::Redirected => "redirect",
        Indexability::ClientError | Indexability::ServerError => "broken",
    }
}

/// The home page: the crawl's start (depth 0), else its first page.
fn root_of(pages: &[StoredPage], details: &HashMap<u64, GraphPage>) -> Option<u32> {
    pages
        .iter()
        .position(|p| details.get(&p.url_hash).and_then(|d| d.depth) == Some(0))
        .or((!pages.is_empty()).then_some(0))
        .map(|i| i as u32)
}

/// The pages a capped payload keeps, in index order: the root, then pages by link score with
/// every ancestor they need, while all of them fit in `max`.
fn keep(pages: &[StoredPage], root: Option<u32>, max: usize) -> Vec<u32> {
    let n = pages.len();
    if n <= max {
        return (0..n as u32).collect();
    }
    let mut order: Vec<u32> = (0..n as u32).collect();
    order.sort_by_key(|&i| (Reverse(pages[i as usize].link_score), i));
    let mut kept = vec![false; n];
    let mut count = 0;
    let mut chain = Vec::new();
    for i in root.into_iter().chain(order) {
        if count >= max {
            break;
        }
        chain.clear();
        let mut at = Some(i);
        while let Some(j) = at {
            // A damaged graph could loop; a chain never outgrows the pages.
            if kept[j as usize] || chain.len() >= n {
                break;
            }
            chain.push(j);
            at = pages[j as usize].parent;
        }
        if count + chain.len() > max {
            continue;
        }
        for &j in &chain {
            kept[j as usize] = true;
        }
        count += chain.len();
    }
    (0..n as u32).filter(|&i| kept[i as usize]).collect()
}

/// `data.json` for a graph and its pages' details, cut to `max_nodes` pages and `max_links`
/// extra links.
fn build(
    pages: &[StoredPage],
    edges: &[Edge],
    details: &HashMap<u64, GraphPage>,
    max_nodes: usize,
    max_links: usize,
) -> Data {
    let n = pages.len();
    let root = root_of(pages, details);
    let host = root
        .and_then(|r| details.get(&pages[r as usize].url_hash))
        .and_then(|d| Url::parse(&d.url).ok())
        .and_then(|u| u.host_str().map(str::to_owned));

    let mut stats = Stats {
        pages: n,
        links: edges.len(),
        ..Stats::default()
    };
    for p in pages {
        let Some(d) = details.get(&p.url_hash) else {
            continue;
        };
        match category(d) {
            "broken" => stats.broken += 1,
            "redirect" => stats.redirects += 1,
            "noindex" => stats.noindex += 1,
            _ => {}
        }
        match d.depth {
            None => stats.orphans += 1,
            Some(depth) => {
                stats.deep += usize::from(depth >= DEEP_CLICKS);
                stats.depth = stats.depth.max(depth);
            }
        }
    }

    let kept = keep(pages, root, max_nodes);
    let mut new_of = vec![None; n];
    for (at, &i) in kept.iter().enumerate() {
        new_of[i as usize] = Some(at as u32);
    }
    let nodes = kept
        .iter()
        .map(|&i| {
            let p = &pages[i as usize];
            let d = details.get(&p.url_hash);
            Node {
                u: d.map_or_else(String::new, |d| short_url(&d.url, host.as_deref())),
                h: hex(p.url_hash),
                t: d.and_then(|d| d.title.clone()).unwrap_or_default(),
                s: d.map_or(0, |d| d.status),
                x: d.map_or("broken", category),
                d: d.and_then(|d| d.depth),
                p: p.parent
                    .and_then(|p| new_of.get(p as usize).copied().flatten()),
                k: p.link_score,
                i: d.map_or(0, |d| d.inlinks),
                o: Some(i) != root && d.and_then(|d| d.depth).is_none(),
            }
        })
        .collect();

    // Navigation: links to home, and on a big enough site to pages half the site links to.
    let mut indegree = vec![0usize; n];
    for e in edges {
        if let Some(c) = indegree.get_mut(e.to as usize) {
            *c += 1;
        }
    }
    let nav = |to: u32| Some(to) == root || (n >= NAV_MIN_PAGES && indegree[to as usize] * 2 >= n);
    let mut seen = HashSet::new();
    let mut e = Vec::new();
    let mut dropped_links = 0;
    for edge in edges {
        let (Some(&Some(from)), Some(&Some(to))) =
            (new_of.get(edge.from as usize), new_of.get(edge.to as usize))
        else {
            continue;
        };
        if from == to || pages[edge.to as usize].parent == Some(edge.from) || nav(edge.to) {
            continue;
        }
        if !seen.insert((from, to)) {
            continue;
        }
        if e.len() / 2 < max_links {
            e.extend([from, to]);
        } else {
            dropped_links += 1;
        }
    }

    let dropped_pages = n - kept.len();
    Data {
        empty: n == 0,
        root: root.and_then(|r| new_of[r as usize]),
        nodes,
        e,
        stats,
        capped: (dropped_pages > 0 || dropped_links > 0).then_some(Capped {
            pages: dropped_pages,
            links: dropped_links,
        }),
    }
}

/// The URL as the views label it: its path and query on the site's host, else all of it.
fn short_url(url: &str, host: Option<&str>) -> String {
    match Url::parse(url) {
        Ok(u) if u.host_str() == host => match u.query() {
            Some(q) => format!("{}?{q}", u.path()),
            None => u.path().to_owned(),
        },
        _ => url.to_owned(),
    }
}

#[derive(Debug, Deserialize)]
pub struct PageParams {
    #[serde(default)]
    h: String,
}

/// `page.json`: one page for the side panel.
#[derive(Debug, Serialize)]
pub struct PageInfo {
    pub url: String,
    pub title: String,
    pub status: u16,
    pub x: &'static str,
    pub depth: Option<u32>,
    pub score: u8,
    /// Links to it the crawl counted.
    pub inlinks: u32,
    /// Pages in the graph that link to it.
    pub linked_from: usize,
    /// The best-linked of those, at most [`PAGE_INLINKS`].
    pub links: Vec<Inlink>,
    pub outlinks: usize,
    pub in_sitemap: bool,
    /// Home first, the page last; just the page when it hangs off no tree.
    pub route: Vec<Step>,
}

#[derive(Debug, Serialize)]
pub struct Inlink {
    pub url: String,
    pub h: String,
    pub anchor: String,
    pub nofollow: bool,
}

#[derive(Debug, Serialize)]
pub struct Step {
    pub url: String,
    pub h: String,
    pub title: String,
}

async fn page(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
    Query(params): Query<PageParams>,
) -> Result<Response, AppError> {
    let site = load_site(&state, &user, site_id(&site)?).await?;
    let hash = parse_hex(&params.h).ok_or(AppError::NotFound)?;
    let crawl = crawls::latest_done(&state.pool, site.id)
        .await?
        .ok_or(AppError::NotFound)?;
    let g = graph::load(&state.pool, crawl.id)
        .await?
        .ok_or(AppError::NotFound)?;
    let i = g.index_of(hash).ok_or(AppError::NotFound)?;

    let mut into: Vec<&Edge> = g.inlinks(i).collect();
    let linked_from = into.len();
    into.sort_by_key(|e| Reverse(g.pages[e.from as usize].link_score));
    into.truncate(PAGE_INLINKS);
    let mut route = vec![i];
    while let Some(p) = g.pages[*route.last().unwrap_or(&i) as usize].parent {
        if route.len() > g.pages.len() || route.contains(&p) {
            break;
        }
        route.push(p);
    }
    route.reverse();

    let wanted: Vec<u64> = into
        .iter()
        .map(|e| e.from)
        .chain(route.iter().copied())
        .map(|j| g.pages[j as usize].url_hash)
        .collect();
    let details = by_hash(graph_pages(&state.pool, crawl.id, Some(&wanted)).await?);
    let me = details.get(&hash).ok_or(AppError::NotFound)?;
    let url_of = |j: u32| {
        let h = g.pages[j as usize].url_hash;
        (details.get(&h), hex(h))
    };
    let info = PageInfo {
        url: me.url.clone(),
        title: me.title.clone().unwrap_or_default(),
        status: me.status,
        x: category(me),
        depth: me.depth,
        score: g.pages[i as usize].link_score,
        inlinks: me.inlinks,
        linked_from,
        links: into
            .iter()
            .map(|e| {
                let (d, h) = url_of(e.from);
                Inlink {
                    url: d.map(|d| d.url.clone()).unwrap_or_default(),
                    h,
                    anchor: g.anchor(e).to_owned(),
                    nofollow: e.nofollow,
                }
            })
            .collect(),
        outlinks: g.outlinks(i).count(),
        in_sitemap: me.in_sitemap,
        route: route
            .iter()
            .map(|&j| {
                let (d, h) = url_of(j);
                Step {
                    url: d.map(|d| d.url.clone()).unwrap_or_default(),
                    h,
                    title: d.and_then(|d| d.title.clone()).unwrap_or_default(),
                }
            })
            .collect(),
    };
    Ok(Json(info).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(
        n: usize,
        parent: impl Fn(u32) -> Option<u32>,
        score: impl Fn(u32) -> u8,
    ) -> Vec<StoredPage> {
        (0..n as u32)
            .map(|i| StoredPage {
                url_hash: 1000 + u64::from(i),
                parent: parent(i),
                link_score: score(i),
            })
            .collect()
    }

    fn details(
        pages: &[StoredPage],
        depth: impl Fn(u32) -> Option<u32>,
    ) -> HashMap<u64, GraphPage> {
        pages
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let path = if i == 0 {
                    String::new()
                } else {
                    format!("p{i}")
                };
                (
                    p.url_hash,
                    GraphPage {
                        url_hash: p.url_hash,
                        url: format!("https://e.com/{path}"),
                        title: Some(format!("Page {i}")),
                        status: 200,
                        indexability: Indexability::Indexable,
                        depth: depth(i as u32),
                        inlinks: 1,
                        in_sitemap: true,
                    },
                )
            })
            .collect()
    }

    fn edge(from: u32, to: u32) -> Edge {
        Edge {
            from,
            to,
            anchor: 0,
            nofollow: false,
        }
    }

    /// Home, 60 sections under it, and every other page under a section.
    fn site(n: usize) -> (Vec<StoredPage>, HashMap<u64, GraphPage>) {
        let parent = |i: u32| match i {
            0 => None,
            1..=60 => Some(0),
            _ => Some(i % 60 + 1),
        };
        let pages = stored(n, parent, |i| (i % 97 + 1) as u8);
        let details = details(&pages, |i| {
            Some(match i {
                0 => 0,
                1..=60 => 1,
                _ => 2,
            })
        });
        (pages, details)
    }

    #[test]
    fn a_big_graph_is_cut_to_the_best_linked_pages_and_their_ancestors() {
        let (pages, details) = site(6_000);
        let data = build(&pages, &[], &details, MAX_NODES, MAX_LINKS);
        assert_eq!(data.nodes.len(), MAX_NODES);
        assert_eq!(
            data.capped,
            Some(Capped {
                pages: 1_000,
                links: 0
            })
        );
        assert_eq!(data.stats.pages, 6_000);
        assert_eq!(data.root, Some(0));
        // Every kept page's ancestors are kept: each chain reaches home.
        for (at, node) in data.nodes.iter().enumerate() {
            let mut hops = 0;
            let mut p = node.p;
            while let Some(j) = p {
                p = data.nodes[j as usize].p;
                hops += 1;
                assert!(hops <= 2, "node {at}");
            }
            assert!(at == 0 || hops > 0, "node {at} hangs off nothing");
        }
        // Every page left out scores no better than any page kept for its own score.
        let kept: HashSet<&str> = data.nodes.iter().map(|n| n.h.as_str()).collect();
        let worst_kept = data
            .nodes
            .iter()
            .filter(|n| n.d == Some(2))
            .map(|n| n.k)
            .min()
            .unwrap();
        let best_dropped = pages
            .iter()
            .filter(|p| !kept.contains(hex(p.url_hash).as_str()))
            .map(|p| p.link_score)
            .max()
            .unwrap();
        assert!(best_dropped <= worst_kept, "{best_dropped} > {worst_kept}");
    }

    #[test]
    fn a_small_graph_is_sent_whole() {
        let (pages, details) = site(200);
        let data = build(&pages, &[], &details, MAX_NODES, MAX_LINKS);
        assert_eq!(data.nodes.len(), 200);
        assert_eq!(data.capped, None);
        assert_eq!(data.nodes[0].u, "/");
        assert_eq!(data.nodes[5].u, "/p5");
        assert_eq!(data.nodes[5].h, "00000000000003ed");
    }

    #[test]
    fn tree_links_and_navigation_are_left_out_and_extra_links_capped() {
        let (pages, details) = site(200);
        let mut edges = vec![
            // A tree link (page 61's parent is 2).
            edge(2, 61),
            // To home.
            edge(70, 0),
            // A self-link.
            edge(70, 70),
            edge(70, 71),
            // Again: one per pair.
            edge(70, 71),
        ];
        // Page 150 is linked from every page: navigation.
        edges.extend((0..200).filter(|&i| i != 150).map(|i| edge(i, 150)));
        edges.extend((80..90).map(|i| edge(i, 100)));
        let data = build(&pages, &edges, &details, MAX_NODES, MAX_LINKS);
        let pairs: Vec<_> = data.e.chunks(2).map(|c| (c[0], c[1])).collect();
        assert_eq!(pairs[0], (70, 71));
        assert_eq!(pairs.len(), 11);
        assert!(pairs.iter().all(|&(_, to)| to != 150 && to != 0));

        let data = build(&pages, &edges, &details, MAX_NODES, 4);
        assert_eq!(data.e.len(), 8);
        assert_eq!(data.capped, Some(Capped { pages: 0, links: 7 }));
    }

    #[test]
    fn orphans_and_statuses_are_read_from_the_pages() {
        let pages = stored(4, |i| (i == 1).then_some(0), |_| 50);
        let mut details = details(&pages, |i| match i {
            0 => Some(0),
            1 => Some(1),
            _ => None,
        });
        details.get_mut(&1002).unwrap().indexability = Indexability::ClientError;
        details.get_mut(&1003).unwrap().url = "https://other.org/x?y=1".to_owned();
        let data = build(&pages, &[], &details, MAX_NODES, MAX_LINKS);
        let o: Vec<_> = data.nodes.iter().map(|n| n.o).collect();
        assert_eq!(o, [false, false, true, true]);
        assert_eq!(data.nodes[2].x, "broken");
        assert_eq!(data.nodes[3].u, "https://other.org/x?y=1");
        assert_eq!(data.stats.orphans, 2);
        assert_eq!(data.stats.broken, 1);
        assert_eq!(data.stats.depth, 1);
    }

    #[test]
    fn short_urls_keep_the_query() {
        assert_eq!(
            short_url("https://e.com/a/b?c=1", Some("e.com")),
            "/a/b?c=1"
        );
        assert_eq!(
            short_url("https://f.com/a", Some("e.com")),
            "https://f.com/a"
        );
        assert_eq!(short_url("not a url", Some("e.com")), "not a url");
    }
}
