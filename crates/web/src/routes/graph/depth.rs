//! `/s/{site}/graph/depth`: how many clicks from the home page each page sits, by status and by folder, and the deep pages worth a link from higher up.

use std::collections::{BTreeMap, HashMap};

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use codoseo_core::check::CheckId;
use codoseo_store::graph::{self, StoredGraph};
use codoseo_store::site_graph::{self, DepthBucket, LinkedPage};

use super::sitemap::{RUN_NEW_CRAWL, check_href, page_href, plural, short_url};
use super::{Empty, Tab, TabLink};
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::fmt;
use crate::layout::Shell;
use crate::render::html;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/s/{site}/graph/depth", get(depth_page))
}

/// Pages this many clicks deep or more are too deep.
const TOO_DEEP: u32 = 4;
/// The last column of the chart and the folder grid counts every depth from here on.
const LAST_COL: u32 = 5;
/// Folders the grid shows, most pages first.
const MAX_FOLDERS: usize = 8;
/// Rows of Deep pages worth lifting.
const MAX_LIFT: usize = 10;
/// Deep pages read to rank by link score; the best linked come first.
const LIFT_POOL: i64 = 2000;

#[derive(Template)]
#[template(path = "graph/depth.html")]
pub struct DepthTab {
    pub shell: Shell,
    pub base: String,
    pub tabs: Vec<TabLink>,
    pub empty: Option<Empty>,
    pub view: Option<DepthView>,
}

pub struct DepthView {
    pub tiles: Vec<DepthTile>,
    pub cols: Vec<DepthCol>,
    pub folders: Vec<FolderRow>,
    pub lift: Vec<LiftRow>,
    /// Shown instead of the lift table.
    pub lift_note: Option<&'static str>,
    /// The explorer filtered to deep pages.
    pub deep_href: String,
}

pub struct DepthTile {
    pub label: &'static str,
    pub value: String,
    /// ` gv-tile-warn` or empty.
    pub class: &'static str,
    /// `c-ok` or empty, for the value.
    pub tone: &'static str,
}

/// One column of Pages per click depth.
pub struct DepthCol {
    /// `1 click`, `2`, `5+`, `Not linked`
    pub label: &'static str,
    pub n: String,
    pub aria: String,
    /// The bar's height against the tallest, in percent.
    pub height: u32,
    /// Top to bottom: broken, redirect, noindex, indexable; empty ones left out.
    pub segs: Vec<(&'static str, i64)>,
    pub deep: bool,
    /// Pages here no link reached.
    pub unlinked: bool,
}

pub struct FolderRow {
    /// `/blog`, or `Top level`.
    pub label: String,
    pub cells: Vec<FolderCell>,
}

pub struct FolderCell {
    /// Empty for none.
    pub n: String,
    /// `lv0`..`lv4`, plus ` is-deep` for a deep cell with pages.
    pub class: String,
    pub tip: String,
}

pub struct LiftRow {
    pub path: String,
    pub href: String,
    pub depth: u32,
    pub score: u8,
    /// `3 pages`
    pub linked_from: String,
    /// The hub to link from and the depth that gives; `None` when the crawl tree has none.
    pub hub: Option<(String, String)>,
}

async fn depth_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
) -> Result<Response, AppError> {
    let ctx = super::load(&state, &user, &site, Tab::Depth).await?;
    let view = match &ctx.latest {
        Some(crawl) => {
            let pool = &state.pool;
            let buckets = site_graph::depth_buckets(pool, crawl.id).await?;
            let folders = site_graph::folder_depths(pool, crawl.id, LAST_COL as i32).await?;
            let deep = site_graph::deep_pages(pool, crawl.id, TOO_DEEP as i32, LIFT_POOL).await?;
            let graph = graph::load(pool, crawl.id).await?;
            let (lift, lift_note) = match (&graph, deep.is_empty()) {
                (_, true) => (
                    Vec::new(),
                    Some("No indexable page sits 4 or more clicks deep."),
                ),
                (None, false) => (Vec::new(), Some(RUN_NEW_CRAWL)),
                (Some(g), false) => {
                    let picks = lift_picks(g, &deep);
                    let hubs: Vec<u64> = picks.iter().filter_map(|p| p.hub).collect();
                    let urls = site_graph::urls_by_hash(pool, crawl.id, &hubs).await?;
                    let rows = picks
                        .iter()
                        .map(|p| lift_row(&ctx.base, &ctx.site.domain, p, &urls))
                        .collect();
                    (rows, None)
                }
            };
            Some(DepthView {
                tiles: tiles(&buckets),
                cols: cols(&buckets),
                folders: folder_rows(&folders),
                lift,
                lift_note,
                deep_href: check_href(&ctx.base, CheckId::DeepPage),
            })
        }
        None => None,
    };
    Ok(html(&DepthTab {
        shell: ctx.shell,
        base: ctx.base,
        tabs: ctx.tabs,
        empty: ctx.empty,
        view,
    })?
    .into_response())
}

fn tiles(buckets: &[DepthBucket]) -> Vec<DepthTile> {
    let linked: Vec<(u32, i64)> = buckets
        .iter()
        .filter_map(|b| Some((b.depth?, b.total())))
        .collect();
    let pages: i64 = linked.iter().map(|l| l.1).sum();
    let clicks: i64 = linked.iter().map(|l| i64::from(l.0) * l.1).sum();
    let shallow: i64 = linked.iter().filter(|l| l.0 < TOO_DEEP).map(|l| l.1).sum();
    let deep = pages - shallow;
    let deepest = linked.iter().map(|l| l.0).max().unwrap_or(0);
    let average = if pages > 0 {
        format!("{:.1}", clicks as f64 / pages as f64)
    } else {
        "0".to_owned()
    };
    vec![
        DepthTile {
            label: "Average clicks",
            value: average,
            class: "",
            tone: "",
        },
        DepthTile {
            label: "Within 3 clicks",
            value: fmt::percent(shallow, pages),
            class: "",
            tone: if deep == 0 { "c-ok" } else { "" },
        },
        DepthTile {
            label: "4 clicks or more",
            value: plural(deep, "page", "pages"),
            class: if deep > 0 { " gv-tile-warn" } else { "" },
            tone: "",
        },
        DepthTile {
            label: "Deepest page",
            value: plural(i64::from(deepest), "click", "clicks"),
            class: "",
            tone: "",
        },
    ]
}

fn cols(buckets: &[DepthBucket]) -> Vec<DepthCol> {
    // Depth 1 to 4, 5 and deeper together, then pages no link reached. The home page (depth 0)
    // is where the clicks start, so it has no column.
    let mut sums = [DepthBucket::default(); LAST_COL as usize + 1];
    for b in buckets {
        let slot = match b.depth {
            Some(0) => continue,
            Some(d) => d.min(LAST_COL) as usize - 1,
            None => LAST_COL as usize,
        };
        let s = &mut sums[slot];
        s.indexable += b.indexable;
        s.noindex += b.noindex;
        s.redirect += b.redirect;
        s.broken += b.broken;
    }
    let tallest = sums
        .iter()
        .map(DepthBucket::total)
        .max()
        .unwrap_or(0)
        .max(1);
    const LABELS: [&str; 6] = ["1 click", "2", "3", "4", "5+", "Not linked"];
    sums.iter()
        .enumerate()
        .map(|(i, s)| {
            let n = s.total();
            let label = LABELS[i];
            let aria = match i {
                0 => format!("1 click: {}", plural(n, "page", "pages")),
                4 => format!("5 or more clicks: {}", plural(n, "page", "pages")),
                5 => format!("Not linked: {}", plural(n, "page", "pages")),
                _ => format!("{label} clicks: {}", plural(n, "page", "pages")),
            };
            DepthCol {
                label,
                n: fmt::thousands(n),
                aria,
                height: if n > 0 {
                    ((n * 100 + tallest - 1) / tallest) as u32
                } else {
                    0
                },
                segs: [
                    ("broken", s.broken),
                    ("redirect", s.redirect),
                    ("noindex", s.noindex),
                    ("ok", s.indexable),
                ]
                .into_iter()
                .filter(|seg| seg.1 > 0)
                .collect(),
                deep: i + 1 >= TOO_DEEP as usize && i < LAST_COL as usize,
                unlinked: i == LAST_COL as usize,
            }
        })
        .collect()
}

fn folder_rows(rows: &[(String, i32, i64)]) -> Vec<FolderRow> {
    let mut by_folder: BTreeMap<&str, [i64; LAST_COL as usize]> = BTreeMap::new();
    for (folder, depth, n) in rows {
        let Some(col) = usize::try_from(*depth).ok().and_then(|d| d.checked_sub(1)) else {
            continue;
        };
        if let Some(cell) = by_folder.entry(folder).or_default().get_mut(col) {
            *cell += n;
        }
    }
    let mut folders: Vec<(&str, [i64; LAST_COL as usize])> = by_folder.into_iter().collect();
    folders.sort_by(|a, b| {
        let total = |f: &[i64; LAST_COL as usize]| f.iter().sum::<i64>();
        total(&b.1).cmp(&total(&a.1)).then_with(|| a.0.cmp(b.0))
    });
    folders.truncate(MAX_FOLDERS);
    let fullest = folders.iter().flat_map(|f| f.1).max().unwrap_or(0).max(1);
    folders
        .into_iter()
        .map(|(folder, cells)| {
            let label = if folder.is_empty() {
                "Top level".to_owned()
            } else {
                folder.to_owned()
            };
            let cells = cells
                .iter()
                .enumerate()
                .map(|(i, &n)| {
                    let level = if n > 0 {
                        (n * 4 + fullest - 1) / fullest
                    } else {
                        0
                    };
                    let deep = n > 0 && i + 1 >= TOO_DEEP as usize;
                    let at = if i + 1 == LAST_COL as usize {
                        "5 or more clicks".to_owned()
                    } else {
                        plural(i as i64 + 1, "click", "clicks")
                    };
                    FolderCell {
                        n: if n > 0 {
                            fmt::thousands(n)
                        } else {
                            String::new()
                        },
                        class: format!("lv{level}{}", if deep { " is-deep" } else { "" }),
                        tip: format!("{label}, {at}: {}", plural(n, "page", "pages")),
                    }
                })
                .collect();
            FolderRow { label, cells }
        })
        .collect()
}

/// A deep page picked for the lift table, with the hub to link it from.
struct LiftPick<'a> {
    page: &'a LinkedPage,
    score: u8,
    /// The URL hash of its crawl-tree ancestor at depth 2 (or 1), and that ancestor's depth.
    hub: Option<u64>,
    hub_depth: u32,
}

/// The best-scored deep pages, each with its ancestor at depth 2 in the crawl tree (depth 1
/// when the walk stops short), which one link from would bring it within 3 clicks.
fn lift_picks<'a>(g: &StoredGraph, deep: &'a [LinkedPage]) -> Vec<LiftPick<'a>> {
    let mut picks: Vec<LiftPick> = deep
        .iter()
        .map(|page| {
            let at = g.index_of(page.url_hash);
            let score = at.map_or(0, |i| g.pages[i as usize].link_score);
            let depth = page.depth.unwrap_or(0);
            // Each parent sits one click closer to the home page.
            let mut chain = Vec::new();
            let mut cur = at;
            for _ in 0..depth.saturating_sub(1) {
                cur = cur.and_then(|i| g.pages[i as usize].parent);
                match cur {
                    Some(i) => chain.push(i),
                    None => break,
                }
            }
            // chain[k] sits at depth - 1 - k.
            let at_depth = |d: u32| {
                depth
                    .checked_sub(1 + d)
                    .and_then(|k| chain.get(k as usize))
                    .map(|&i| g.pages[i as usize].url_hash)
            };
            let (hub, hub_depth) = match (at_depth(2), at_depth(1)) {
                (Some(h), _) => (Some(h), 2),
                (None, Some(h)) => (Some(h), 1),
                (None, None) => (None, 0),
            };
            LiftPick {
                page,
                score,
                hub,
                hub_depth,
            }
        })
        .collect();
    // Highest link score first; the store's order (most inlinks) breaks ties.
    picks.sort_by_key(|p| std::cmp::Reverse(p.score));
    picks.truncate(MAX_LIFT);
    picks
}

fn lift_row(base: &str, domain: &str, p: &LiftPick, urls: &HashMap<u64, String>) -> LiftRow {
    LiftRow {
        path: short_url(&p.page.url, domain),
        href: page_href(base, p.page.url_hash),
        depth: p.page.depth.unwrap_or(0),
        score: p.score,
        linked_from: plural(i64::from(p.page.inlinks), "page", "pages"),
        hub: p.hub.and_then(|h| urls.get(&h)).map(|url| {
            (
                short_url(url, domain),
                format!("→ {} clicks", p.hub_depth + 1),
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bucket(depth: Option<u32>, indexable: i64, broken: i64) -> DepthBucket {
        DepthBucket {
            depth,
            indexable,
            broken,
            ..DepthBucket::default()
        }
    }

    #[test]
    fn tiles_and_columns_count_from_the_buckets() {
        let b = [
            bucket(Some(0), 1, 0),
            bucket(Some(1), 3, 0),
            bucket(Some(3), 2, 1),
            bucket(Some(4), 2, 0),
            bucket(Some(7), 1, 0),
            bucket(None, 2, 0),
        ];
        let t = tiles(&b);
        // 10 linked pages, 0 + 3 + 9 + 8 + 7 = 27 clicks.
        assert_eq!(t[0].value, "2.7");
        assert_eq!(t[1].value, "70%");
        assert_eq!(t[2].value, "3 pages");
        assert_eq!(t[3].value, "7 clicks");
        let c = cols(&b);
        assert_eq!(c.len(), 6);
        assert_eq!(
            c.iter().map(|c| c.n.as_str()).collect::<Vec<_>>(),
            ["3", "0", "3", "2", "1", "2"]
        );
        assert_eq!(c[0].height, 100);
        assert_eq!(c[2].segs, [("broken", 1), ("ok", 2)]);
        assert!(c[3].deep && c[4].deep && !c[5].deep && c[5].unlinked);
        assert_eq!(c[4].aria, "5 or more clicks: 1 page");
    }

    #[test]
    fn folders_keep_the_fullest_and_flag_deep_cells() {
        let rows = vec![
            ("/blog".to_owned(), 1, 1),
            ("/blog".to_owned(), 3, 8),
            ("/blog".to_owned(), 5, 2),
            (String::new(), 1, 4),
        ];
        let f = folder_rows(&rows);
        assert_eq!(f[0].label, "/blog");
        assert_eq!(f[1].label, "Top level");
        assert_eq!(f[0].cells[2].class, "lv4");
        assert_eq!(f[0].cells[4].class, "lv1 is-deep");
        assert_eq!(f[0].cells[3].n, "");
        assert_eq!(f[0].cells[4].tip, "/blog, 5 or more clicks: 2 pages");
    }
}
