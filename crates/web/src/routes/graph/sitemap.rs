//! `/s/{site}/graph/sitemap`: Sitemap & depth. The sitemap index and its child sitemaps, each split by what the crawl found for its URLs, against the pages the crawl reached by links; then how deep those pages sit (see `depth`).
//!
//! Also home to the small helpers the other server-drawn views (depth, broken & redirects,
//! changes) share: short paths, explorer links and plurals.

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use codoseo_core::check::CheckId;
use codoseo_core::sitemap::{SitemapKind, SitemapTree, SitemapTreeFile};
use codoseo_store::explorer::PageFilter;
use codoseo_store::site_graph::{self, SitemapOverlap};
use codoseo_store::sitemaps;
use url::Url;

use super::{Empty, Tab, TabLink};
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::fmt;
use crate::layout::Shell;
use crate::render::html;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/s/{site}/graph/sitemap", get(sitemap_page))
}

/// What a view says where the crawl predates the data it draws from.
pub(super) const RUN_NEW_CRAWL: &str = "Run a new crawl to see this.";
/// Child sitemaps one index shows before `+ N more`.
const MAX_ROWS: usize = 12;
/// One child sitemap row's height in px, which the connector layer is drawn against.
const ROW_H: u32 = 44;

#[derive(Template)]
#[template(path = "graph/sitemap.html")]
pub struct SitemapTab {
    pub shell: Shell,
    pub base: String,
    pub tabs: Vec<TabLink>,
    pub empty: Option<Empty>,
    pub view: Option<SitemapView>,
    pub depth: Option<super::depth::DepthView>,
}

pub struct SitemapView {
    /// One group per sitemap the site points to.
    pub groups: Vec<SitemapGroup>,
    /// Shown instead of the tree: no tree kept, or no sitemap found.
    pub tree_note: Option<&'static str>,
    /// Some file has URLs the crawl never recorded, so the legend names them.
    pub any_not_crawled: bool,
    pub venn: Option<Venn>,
    pub fixes: Vec<Fix>,
}

/// An index card and the sitemaps under it, or sitemaps with no index (`root` unset).
pub struct SitemapGroup {
    pub root: Option<RootCard>,
    /// The height of the rows column and its connector layer, in px.
    pub height: u32,
    /// SVG path data, one curve from the index card to each row.
    pub paths: Vec<String>,
    pub rows: Vec<SitemapRow>,
    /// `+ 3 more sitemaps`
    pub more: Option<String>,
}

pub struct RootCard {
    pub name: String,
    pub url: String,
    /// `4 sitemaps · 148 URLs`, or why it failed.
    pub note: String,
    pub failed: bool,
}

pub struct SitemapRow {
    pub name: String,
    pub url: String,
    pub count: String,
    /// The bar's segments, empty for a file that failed.
    pub segs: Vec<Seg>,
    /// The bar's width against the longest file's, in percent.
    pub width: u32,
    pub note: String,
    /// ` is-flag`, ` is-err` or empty, appended to the note's class.
    pub note_class: &'static str,
}

pub struct Seg {
    /// `ok`, `orphan`, `noindex`, `redirect`, `broken`, `uncrawled`
    pub kind: &'static str,
    pub n: u32,
    /// `3 orphans`
    pub tip: String,
}

/// The two-circle picture: listed in a sitemap against reached by links.
pub struct Venn {
    pub in_sitemap: String,
    pub linked: String,
    pub only_sitemap: String,
    pub both: String,
    pub only_linked: String,
    /// `9 not indexable`, or `all indexable`.
    pub both_note: String,
    pub both_bad: bool,
    /// `12 indexable`
    pub linked_note: String,
    pub aria: String,
}

/// One row of What to fix.
pub struct Fix {
    pub n: String,
    /// `c-err`, `c-warn` or `c-ok`.
    pub tone: &'static str,
    pub title: &'static str,
    pub detail: String,
    /// The explorer filtered to these pages, where a check matches them.
    pub href: Option<String>,
}

async fn sitemap_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
) -> Result<Response, AppError> {
    let ctx = super::load(&state, &user, &site, Tab::Sitemap).await?;
    let (view, depth) = match &ctx.latest {
        Some(crawl) => {
            let pool = &state.pool;
            let tree = sitemaps::load_tree(pool, crawl.id).await?;
            let overlap = site_graph::sitemap_overlap(pool, crawl.id).await?;
            let orphan_folders =
                site_graph::top_folders(pool, crawl.id, PageFilter::Check(CheckId::Orphan), 1)
                    .await?;
            let missing_folders = site_graph::top_folders(
                pool,
                crawl.id,
                PageFilter::Check(CheckId::NotInSitemap),
                1,
            )
            .await?;
            let sitemap = view(
                &ctx.base,
                tree.as_ref(),
                &overlap,
                orphan_folders.first(),
                missing_folders.first(),
            );
            let depth = super::depth::view(&state, &ctx, crawl).await?;
            (Some(sitemap), Some(depth))
        }
        None => (None, None),
    };
    Ok(html(&SitemapTab {
        shell: ctx.shell,
        base: ctx.base,
        tabs: ctx.tabs,
        empty: ctx.empty,
        view,
        depth,
    })?
    .into_response())
}

fn view(
    base: &str,
    tree: Option<&SitemapTree>,
    overlap: &SitemapOverlap,
    orphan_folder: Option<&(String, i64)>,
    missing_folder: Option<&(String, i64)>,
) -> SitemapView {
    let groups = tree.map(groups).unwrap_or_default();
    let tree_note = match tree {
        None => Some(RUN_NEW_CRAWL),
        Some(_) if groups.is_empty() => Some("The crawl found no sitemap."),
        Some(_) => None,
    };
    let any_not_crawled = tree.is_some_and(|t| {
        t.files
            .iter()
            .any(|f| f.kind == Some(SitemapKind::UrlSet) && f.buckets.not_crawled > 0)
    });
    SitemapView {
        groups,
        tree_note,
        any_not_crawled,
        venn: (overlap.in_sitemap > 0).then(|| venn(overlap)),
        fixes: fixes(base, tree, overlap, orphan_folder, missing_folder),
    }
}

/// The last segment of a file's URL: `posts.xml`.
fn file_name(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|u| {
            u.path_segments()
                .and_then(|mut s| s.rfind(|seg| !seg.is_empty()).map(str::to_owned))
        })
        .unwrap_or_else(|| url.to_owned())
}

/// The file's URLs, as its buckets count them.
fn bucket_total(f: &SitemapTreeFile) -> u32 {
    let b = &f.buckets;
    b.ok + b.orphan + b.noindex + b.redirect + b.broken + b.not_crawled
}

/// The index each file hangs under: the root of its chain of parents.
fn root_of(files: &[SitemapTreeFile], mut i: usize) -> usize {
    // Parents always come first, so the walk ends; the bound guards a damaged tree.
    for _ in 0..files.len() {
        match files[i].parent.map(|p| p as usize) {
            Some(p) if p < i => i = p,
            _ => break,
        }
    }
    i
}

fn groups(tree: &SitemapTree) -> Vec<SitemapGroup> {
    let files = &tree.files;
    // The widest bar is the file with the most URLs.
    let widest = files
        .iter()
        .filter(|f| f.kind == Some(SitemapKind::UrlSet))
        .map(bucket_total)
        .max()
        .unwrap_or(0)
        .max(1);
    let is_leaf = |f: &SitemapTreeFile| f.kind != Some(SitemapKind::Index);
    let mut out = Vec::new();
    let mut loose = Vec::new();
    for (i, f) in files.iter().enumerate() {
        if f.parent.is_some() {
            continue;
        }
        if is_leaf(f) {
            loose.push(i);
            continue;
        }
        let children: Vec<usize> = (0..files.len())
            .filter(|&c| c != i && is_leaf(&files[c]) && root_of(files, c) == i)
            .collect();
        let urls: u32 = children
            .iter()
            .filter(|&&c| !files[c].failed)
            .map(|&c| files[c].listed)
            .sum();
        let root = RootCard {
            name: file_name(&f.url),
            url: f.url.clone(),
            note: format!(
                "{} · {}",
                plural(children.len() as i64, "sitemap", "sitemaps"),
                plural(i64::from(urls), "URL", "URLs")
            ),
            failed: f.failed,
        };
        out.push(group(Some(root), files, &children, widest));
    }
    if !loose.is_empty() {
        out.push(group(None, files, &loose, widest));
    }
    out
}

fn group(
    root: Option<RootCard>,
    files: &[SitemapTreeFile],
    members: &[usize],
    widest: u32,
) -> SitemapGroup {
    let rows: Vec<SitemapRow> = members
        .iter()
        .take(MAX_ROWS)
        .map(|&i| row(&files[i], widest))
        .collect();
    let more = (members.len() > MAX_ROWS).then(|| {
        format!(
            "+ {}",
            plural(
                (members.len() - MAX_ROWS) as i64,
                "more sitemap",
                "more sitemaps"
            )
        )
    });
    let lines = rows.len() as u32 + u32::from(more.is_some());
    let height = (lines * ROW_H).max(ROW_H);
    let mid = height / 2;
    let paths = match root {
        Some(_) => (0..rows.len() as u32)
            .map(|r| {
                let y = r * ROW_H + ROW_H / 2;
                format!("M0 {mid}C28 {mid} 28 {y} 56 {y}")
            })
            .collect(),
        None => Vec::new(),
    };
    SitemapGroup {
        root,
        height,
        paths,
        rows,
        more,
    }
}

fn row(f: &SitemapTreeFile, widest: u32) -> SitemapRow {
    let name = file_name(&f.url);
    if f.failed {
        return SitemapRow {
            name,
            url: f.url.clone(),
            count: String::new(),
            segs: Vec::new(),
            width: 0,
            note: "Could not be read".to_owned(),
            note_class: " is-err",
        };
    }
    let b = &f.buckets;
    let segs: Vec<Seg> = [
        ("ok", b.ok, "indexable, linked", "indexable, linked"),
        ("orphan", b.orphan, "orphan", "orphans"),
        ("noindex", b.noindex, "noindex", "noindex"),
        ("redirect", b.redirect, "redirect", "redirects"),
        ("broken", b.broken, "broken", "broken"),
        ("uncrawled", b.not_crawled, "not crawled", "not crawled"),
    ]
    .into_iter()
    .filter(|s| s.1 > 0)
    .map(|(kind, n, one, many)| Seg {
        kind,
        n,
        tip: format!("{} {}", fmt::thousands(n), if n == 1 { one } else { many }),
    })
    .collect();
    let (note, note_class) = lastmod_note(f);
    SitemapRow {
        name,
        url: f.url.clone(),
        count: fmt::thousands(f.listed),
        width: (bucket_total(f) * 100).div_ceil(widest).clamp(2, 100),
        segs,
        note,
        note_class,
    }
}

/// What the file's `<lastmod>` values say, flagged when engines would ignore them.
fn lastmod_note(f: &SitemapTreeFile) -> (String, &'static str) {
    let l = &f.lastmod;
    if l.with == 0 {
        ("no lastmod".to_owned(), "")
    } else if l.future > 0 {
        (
            format!("lastmod: {} in the future", fmt::thousands(l.future)),
            " is-flag",
        )
    } else if l.distinct == 1 && l.with > 1 {
        let day = l.min.as_deref().map(day_label).unwrap_or_default();
        (format!("lastmod: all {day}"), " is-flag")
    } else {
        ("lastmod: varied".to_owned(), "")
    }
}

/// `2026-03-03` -> `Mar 3 2026`; anything else as it is.
fn day_label(ymd: &str) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let mut parts = ymd.splitn(3, '-');
    let (Some(y), Some(m), Some(d)) = (parts.next(), parts.next(), parts.next()) else {
        return ymd.to_owned();
    };
    match (m.parse::<usize>(), d.parse::<u32>()) {
        (Ok(m @ 1..=12), Ok(d)) => format!("{} {d} {y}", MONTHS[m - 1]),
        _ => ymd.to_owned(),
    }
}

fn venn(o: &SitemapOverlap) -> Venn {
    Venn {
        in_sitemap: fmt::thousands(o.in_sitemap),
        linked: fmt::thousands(o.linked),
        only_sitemap: fmt::thousands(o.only_sitemap),
        both: fmt::thousands(o.in_both),
        only_linked: fmt::thousands(o.only_linked),
        both_note: if o.both_not_indexable > 0 {
            format!("{} not indexable", fmt::thousands(o.both_not_indexable))
        } else {
            "all indexable".to_owned()
        },
        both_bad: o.both_not_indexable > 0,
        linked_note: format!("{} indexable", fmt::thousands(o.only_linked_indexable)),
        aria: format!(
            "{} URLs in the sitemap, {} found by following links, {} in both; {} only in the \
             sitemap, {} only linked",
            fmt::thousands(o.in_sitemap),
            fmt::thousands(o.linked),
            fmt::thousands(o.in_both),
            fmt::thousands(o.only_sitemap),
            fmt::thousands(o.only_linked)
        ),
    }
}

/// `Most are under /lp. `, when one folder holds most of `n` pages.
fn folder_hint(folder: Option<&(String, i64)>, n: i64) -> String {
    match folder {
        Some((f, k)) if !f.is_empty() && *k == n => format!("All are under {f}. "),
        Some((f, k)) if !f.is_empty() && k * 2 > n => format!("Most are under {f}. "),
        _ => String::new(),
    }
}

fn fixes(
    base: &str,
    tree: Option<&SitemapTree>,
    o: &SitemapOverlap,
    orphan_folder: Option<&(String, i64)>,
    missing_folder: Option<&(String, i64)>,
) -> Vec<Fix> {
    let mut out = Vec::new();
    let bad = o.sitemap_not_indexable();
    if bad > 0 {
        let parts: Vec<String> = [
            (o.sitemap_redirect, "redirect", "redirects"),
            (o.sitemap_noindex, "noindex", "noindex"),
            (o.sitemap_canonicalised, "canonicalised", "canonicalised"),
            (o.sitemap_broken, "error", "errors"),
        ]
        .into_iter()
        .filter(|p| p.0 > 0)
        .map(|(n, one, many)| plural(n, one, many))
        .collect();
        // The explorer has a check per reason; link the one that covers the most.
        let check = [
            (
                o.sitemap_redirect + o.sitemap_broken,
                CheckId::SitemapNon200,
            ),
            (o.sitemap_noindex, CheckId::SitemapNoindex),
            (o.sitemap_canonicalised, CheckId::SitemapCanonicalised),
        ]
        .into_iter()
        .max_by_key(|c| c.0)
        .map(|c| c.1);
        out.push(Fix {
            n: fmt::thousands(bad),
            tone: "c-err",
            title: "Non-indexable URLs in the sitemap",
            detail: format!("{}. Remove them.", parts.join(", ")),
            href: check.map(|c| check_href(base, c)),
        });
    }
    if o.orphans > 0 {
        out.push(Fix {
            n: fmt::thousands(o.orphans),
            tone: "c-warn",
            title: "Orphans: listed but never linked",
            detail: format!(
                "{}Link the ones you keep.",
                folder_hint(orphan_folder, o.orphans)
            ),
            href: Some(check_href(base, CheckId::Orphan)),
        });
    }
    if o.not_in_sitemap > 0 {
        out.push(Fix {
            n: fmt::thousands(o.not_in_sitemap),
            tone: "c-ok",
            title: "Indexable pages missing from the sitemap",
            detail: format!(
                "{}Add them so engines find them.",
                folder_hint(missing_folder, o.not_in_sitemap)
            ),
            href: Some(check_href(base, CheckId::NotInSitemap)),
        });
    }
    let urlsets = tree
        .map(|t| t.files.as_slice())
        .unwrap_or_default()
        .iter()
        .filter(|f| f.kind == Some(SitemapKind::UrlSet) && !f.failed);
    for f in urlsets {
        let l = &f.lastmod;
        if l.distinct == 1 && l.with > 1 {
            out.push(Fix {
                n: fmt::thousands(l.with),
                tone: "c-warn",
                title: "Same lastmod on every URL",
                detail: format!(
                    "{} stamps {} on all {} URLs, so engines ignore it.",
                    file_name(&f.url),
                    l.min.as_deref().map(day_label).unwrap_or_default(),
                    fmt::thousands(l.with)
                ),
                href: None,
            });
        }
        if l.future > 0 {
            out.push(Fix {
                n: fmt::thousands(l.future),
                tone: "c-warn",
                title: "Lastmod dates in the future",
                detail: format!(
                    "{} dates {} after the day it was read.",
                    file_name(&f.url),
                    plural(i64::from(l.future), "URL", "URLs")
                ),
                href: None,
            });
        }
    }
    out
}

/// `1 page`, `3 pages`, with thousands separators.
pub(super) fn plural(n: i64, one: &str, many: &str) -> String {
    format!("{} {}", fmt::thousands(n), if n == 1 { one } else { many })
}

/// The URL explorer filtered to pages failing `check`.
pub(super) fn check_href(base: &str, check: CheckId) -> String {
    format!("{base}/explorer?filter=check:{}", check.slug())
}

/// The URL explorer with one page selected.
pub(super) fn page_href(base: &str, url_hash: u64) -> String {
    format!("{base}/explorer?sel={url_hash:016x}")
}

/// How a view prints a URL: its path and query on the site (`www.` or not), the URL without
/// its scheme elsewhere.
pub(super) fn short_url(url: &str, domain: &str) -> String {
    let Ok(u) = Url::parse(url) else {
        return url.to_owned();
    };
    let path = match u.query() {
        Some(q) => format!("{}?{q}", u.path()),
        None => u.path().to_owned(),
    };
    let bare = |h: &str| h.trim_start_matches("www.").to_ascii_lowercase();
    match u.host_str() {
        Some(h) if bare(h) == bare(domain) => path,
        Some(h) => format!("{h}{path}"),
        None => url.to_owned(),
    }
}

/// The badge class for an HTTP status: green 2xx, blue permanent redirects, amber temporary
/// ones, red 4xx and no response, purple 5xx.
pub(super) fn status_class(status: u16) -> &'static str {
    match status {
        200..=299 => "t-ok",
        301 | 308 => "gv-t-redirect",
        300..=399 => "t-warn",
        500..=599 => "t-5xx",
        _ => "t-err",
    }
}

/// `404`, or `No response` for status 0.
pub(super) fn status_label(status: u16) -> String {
    if status == 0 {
        "No response".to_owned()
    } else {
        status.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codoseo_core::sitemap::{LastmodStats, SitemapBuckets, SitemapSamples};

    fn file(url: &str, kind: SitemapKind, parent: Option<u32>, listed: u32) -> SitemapTreeFile {
        SitemapTreeFile {
            url: url.to_owned(),
            kind: Some(kind),
            parent,
            failed: false,
            listed,
            lastmod: LastmodStats::default(),
            buckets: SitemapBuckets {
                ok: listed,
                ..SitemapBuckets::default()
            },
            samples: SitemapSamples::default(),
        }
    }

    #[test]
    fn an_index_groups_every_sitemap_under_it() {
        let tree = SitemapTree {
            files: vec![
                file(
                    "https://e.com/sitemap_index.xml",
                    SitemapKind::Index,
                    None,
                    2,
                ),
                file("https://e.com/pages.xml", SitemapKind::UrlSet, Some(0), 10),
                file("https://e.com/nested.xml", SitemapKind::Index, Some(0), 1),
                file("https://e.com/posts.xml", SitemapKind::UrlSet, Some(2), 40),
                file("https://e.com/loose.xml", SitemapKind::UrlSet, None, 5),
            ],
        };
        let g = groups(&tree);
        assert_eq!(g.len(), 2);
        let root = g[0].root.as_ref().unwrap();
        assert_eq!(root.name, "sitemap_index.xml");
        assert_eq!(root.note, "2 sitemaps · 50 URLs");
        let names: Vec<_> = g[0].rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["pages.xml", "posts.xml"]);
        assert_eq!(g[0].rows[1].width, 100);
        assert_eq!(g[0].rows[0].width, 25);
        assert_eq!(g[0].height, 88);
        assert_eq!(
            g[0].paths,
            ["M0 44C28 44 28 22 56 22", "M0 44C28 44 28 66 56 66"]
        );
        assert!(g[1].root.is_none() && g[1].paths.is_empty());
        assert_eq!(g[1].rows[0].name, "loose.xml");
    }

    #[test]
    fn a_long_index_shows_twelve_and_counts_the_rest() {
        let mut files = vec![file("https://e.com/i.xml", SitemapKind::Index, None, 0)];
        for n in 0..15 {
            files.push(file(
                &format!("https://e.com/s{n}.xml"),
                SitemapKind::UrlSet,
                Some(0),
                1,
            ));
        }
        let g = groups(&SitemapTree { files });
        assert_eq!(g[0].rows.len(), 12);
        assert_eq!(g[0].more.as_deref(), Some("+ 3 more sitemaps"));
        assert_eq!(g[0].height, 13 * ROW_H);
    }

    #[test]
    fn lastmod_is_flagged_when_engines_would_ignore_it() {
        let mut f = file("https://e.com/p.xml", SitemapKind::UrlSet, None, 3);
        assert_eq!(lastmod_note(&f), ("no lastmod".to_owned(), ""));
        f.lastmod = LastmodStats {
            with: 3,
            distinct: 1,
            min: Some("2026-03-03".to_owned()),
            max: Some("2026-03-03".to_owned()),
            future: 0,
        };
        assert_eq!(
            lastmod_note(&f),
            ("lastmod: all Mar 3 2026".to_owned(), " is-flag")
        );
        f.lastmod.future = 2;
        assert_eq!(lastmod_note(&f).0, "lastmod: 2 in the future");
        f.lastmod = LastmodStats {
            with: 3,
            distinct: 3,
            ..LastmodStats::default()
        };
        assert_eq!(lastmod_note(&f), ("lastmod: varied".to_owned(), ""));
        assert_eq!(day_label("nope"), "nope");
    }

    #[test]
    fn urls_print_short_on_the_site() {
        assert_eq!(short_url("https://www.e.com/a?b=1", "e.com"), "/a?b=1");
        assert_eq!(short_url("https://other.com/a", "e.com"), "other.com/a");
        assert_eq!(status_class(302), "t-warn");
        assert_eq!(status_class(301), "gv-t-redirect");
        assert_eq!(status_label(0), "No response");
    }
}
