//! `/s/{site}/graph/changes`: the folders that changed since the previous crawl, as a tree from the home page to each changed page, with the health trend and what got better.

use std::collections::BTreeMap;

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use codoseo_core::page::Indexability;
use codoseo_store::crawls::{self, Crawl};
use codoseo_store::reports::{self, HealthPoint};
use codoseo_store::site_graph::{self, PageDiff, PageState, top_folder};
use url::Url;

use super::sitemap::{page_href, plural, short_url, status_label};
use super::{Empty, Tab, TabLink};
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::fmt;
use crate::layout::Shell;
use crate::render::html;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/s/{site}/graph/changes", get(changes_page))
}

/// Changed URLs read; past this the counts say they cover only these.
const MAX_DIFFS: i64 = 10_000;
/// Folders the tree draws, most changes first.
const MAX_FOLDERS: usize = 10;
/// Pages one folder draws before `+ N more`.
const MAX_PAGES: usize = 12;
/// Finished crawls in the health trend.
const HISTORY: i64 = 6;
/// The tree's geometry, in px: card height and pitch, the gap between folders, each column's
/// x and width.
const CARD_H: u32 = 36;
const PITCH: u32 = 44;
const GROUP_GAP: u32 = 16;
const HOME_W: u32 = 140;
const HOME_H: u32 = 64;
const FOLDER_X: u32 = 170;
const FOLDER_W: u32 = 210;
const PAGE_X: u32 = 410;
const TREE_W: u32 = 760;
/// The health sparkline's viewBox.
const SPARK_W: f64 = 280.0;
const SPARK_H: f64 = 70.0;

#[derive(Template)]
#[template(path = "graph/changes.html")]
pub struct ChangesTab {
    pub shell: Shell,
    pub base: String,
    pub tabs: Vec<TabLink>,
    pub empty: Option<Empty>,
    pub view: Option<ChangesView>,
}

pub struct ChangesView {
    /// Shown alone when there is nothing to compare yet.
    pub note: Option<&'static str>,
    /// `#41 · Oct 10`
    pub from_chip: String,
    pub to_chip: String,
    pub chips: Vec<Chip>,
    pub tree: Option<ChangeTree>,
    /// `Nothing changed between #41 and #42.`
    pub nothing: Option<String>,
    /// `Showing the first 10,000 changed URLs.`
    pub capped: Option<String>,
    pub health: Option<Health>,
    pub problem: Option<Problem>,
    /// `No new problems since #41.`
    pub no_problem: String,
    pub better: Vec<(&'static str, String)>,
    pub no_better: String,
    /// The changes screen, with every change the crawl recorded.
    pub changes_href: String,
}

pub struct Chip {
    pub text: String,
    /// `new`, `removed`, `deeper`, `broke`, `orphan`, `noindex`
    pub kind: &'static str,
}

/// The home card, the changed folders and their changed pages, laid out here so the template
/// only places them.
pub struct ChangeTree {
    pub width: u32,
    pub height: u32,
    pub paths: Vec<TreePath>,
    pub home_top: u32,
    /// `13 changes`
    pub home_note: String,
    pub folders: Vec<FolderCard>,
    pub pages: Vec<PageCard>,
    /// `+ 2 more folders changed`
    pub more_folders: Option<String>,
}

pub struct TreePath {
    pub d: String,
    /// ` is-broke`, ` is-orphan`, ` is-removed` or empty.
    pub class: &'static str,
}

pub struct FolderCard {
    pub top: u32,
    pub label: String,
    /// `+3 · 2 deeper`
    pub note: String,
    /// ` is-broke`, ` is-orphan` or empty.
    pub class: &'static str,
}

pub struct PageCard {
    pub top: u32,
    pub path: String,
    /// The explorer with the page selected; `None` for a removed page or `+ N more`.
    pub href: Option<String>,
    pub badge: String,
    /// `new`, `removed`, `deeper`, `broke`, `orphan`, `noindex`, `redirect`, `more`
    pub kind: &'static str,
}

pub struct Health {
    pub score: String,
    /// `−3 since #41`
    pub delta: String,
    /// `c-ok`, `c-err` or `c-muted`.
    pub delta_class: &'static str,
    pub points: String,
    pub dots: Vec<(String, String)>,
    pub last: (String, String),
    pub first_label: String,
    pub last_label: String,
    pub aria: String,
}

pub struct Problem {
    pub path: Option<String>,
    pub text: String,
    pub href: String,
    pub link: &'static str,
}

/// What happened to one URL, the most telling first when several did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Broke,
    Removed,
    /// New, and reached by no link.
    NewOrphan,
    /// Linked before, reached by no link now.
    LostLinks,
    NowNoindex,
    NowRedirects,
    Deeper,
    New,
    Fixed,
    Shallower,
    AddedToSitemap,
}

impl Kind {
    /// The changes the tree draws; the rest only count as better.
    fn drawn(self) -> bool {
        !matches!(self, Kind::Fixed | Kind::Shallower | Kind::AddedToSitemap)
    }

    fn class(self) -> &'static str {
        match self {
            Kind::Broke => "broke",
            Kind::Removed => "removed",
            Kind::NewOrphan | Kind::LostLinks => "orphan",
            Kind::NowNoindex => "noindex",
            Kind::NowRedirects => "redirect",
            Kind::Deeper => "deeper",
            Kind::New | Kind::Fixed | Kind::Shallower | Kind::AddedToSitemap => "new",
        }
    }
}

fn is_broken(s: &PageState) -> bool {
    matches!(
        s.indexability,
        Indexability::ClientError | Indexability::ServerError
    )
}

fn is_kept_out(s: &PageState) -> bool {
    matches!(
        s.indexability,
        Indexability::Noindex | Indexability::Canonicalised | Indexability::BlockedByRobots
    )
}

/// What happened to a URL between the crawls; `None` for a change no view shows (a sitemap
/// listing dropped, a status moving within 2xx).
fn classify(d: &PageDiff) -> Option<Kind> {
    let (b, a) = match (&d.before, &d.after) {
        (_, None) => return Some(Kind::Removed),
        (None, Some(a)) => {
            return Some(if a.depth.is_none() {
                Kind::NewOrphan
            } else {
                Kind::New
            });
        }
        (Some(b), Some(a)) => (b, a),
    };
    Some(if !is_broken(b) && is_broken(a) {
        Kind::Broke
    } else if is_broken(b) && !is_broken(a) {
        Kind::Fixed
    } else if b.depth.is_some() && a.depth.is_none() {
        Kind::LostLinks
    } else if !is_kept_out(b) && is_kept_out(a) {
        Kind::NowNoindex
    } else if b.indexability != Indexability::Redirected
        && a.indexability == Indexability::Redirected
    {
        Kind::NowRedirects
    } else if let (Some(bd), Some(ad)) = (b.depth, a.depth)
        && ad != bd
    {
        if ad > bd {
            Kind::Deeper
        } else {
            Kind::Shallower
        }
    } else if !b.in_sitemap && a.in_sitemap {
        Kind::AddedToSitemap
    } else {
        return None;
    })
}

fn badge(kind: Kind, d: &PageDiff) -> String {
    let (b, a) = (d.before.as_ref(), d.after.as_ref());
    match kind {
        Kind::Broke | Kind::NowRedirects => format!(
            "{} → {}",
            b.map_or_else(String::new, |s| status_label(s.status)),
            a.map_or_else(String::new, |s| status_label(s.status))
        ),
        Kind::Removed => "removed".to_owned(),
        Kind::NewOrphan => "new, not linked".to_owned(),
        Kind::LostLinks => "not linked now".to_owned(),
        Kind::NowNoindex => "now noindex".to_owned(),
        Kind::Deeper => format!(
            "{} → {} clicks",
            b.and_then(|s| s.depth).unwrap_or(0),
            a.and_then(|s| s.depth).unwrap_or(0)
        ),
        Kind::New => "new".to_owned(),
        Kind::Fixed => "fixed".to_owned(),
        Kind::Shallower => "closer".to_owned(),
        Kind::AddedToSitemap => "in sitemap".to_owned(),
    }
}

async fn changes_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
) -> Result<Response, AppError> {
    let ctx = super::load(&state, &user, &site, Tab::Changes).await?;
    let view = match &ctx.latest {
        Some(latest) => {
            let pool = &state.pool;
            let previous = crawls::previous_done(pool, ctx.site.id, latest.id).await?;
            Some(match previous {
                None => noted("Changes appear after your second crawl."),
                Some(previous) if site_graph::page_count(pool, previous.id).await? == 0 => noted(
                    "The crawl before this one is no longer stored. Changes appear after the \
                     next crawl.",
                ),
                Some(previous) => {
                    let diffs =
                        site_graph::page_diffs(pool, previous.id, latest.id, MAX_DIFFS).await?;
                    let history = reports::health_history(pool, ctx.site.id, HISTORY).await?;
                    view(
                        &ctx.base,
                        &ctx.site.domain,
                        &previous,
                        latest,
                        &diffs,
                        &history,
                    )
                }
            })
        }
        None => None,
    };
    Ok(html(&ChangesTab {
        shell: ctx.shell,
        base: ctx.base,
        tabs: ctx.tabs,
        empty: ctx.empty,
        view,
    })?
    .into_response())
}

fn noted(note: &'static str) -> ChangesView {
    ChangesView {
        note: Some(note),
        from_chip: String::new(),
        to_chip: String::new(),
        chips: Vec::new(),
        tree: None,
        nothing: None,
        capped: None,
        health: None,
        problem: None,
        no_problem: String::new(),
        better: Vec::new(),
        no_better: String::new(),
        changes_href: String::new(),
    }
}

/// `#47 · Oct 2`
fn chip(c: &Crawl) -> String {
    match c.finished_at {
        Some(at) => format!("#{} · {}", c.number, fmt::date(at)),
        None => format!("#{}", c.number),
    }
}

fn view(
    base: &str,
    domain: &str,
    previous: &Crawl,
    latest: &Crawl,
    diffs: &[PageDiff],
    history: &[HealthPoint],
) -> ChangesView {
    let changed: Vec<(Kind, &PageDiff)> = diffs
        .iter()
        .filter_map(|d| Some((classify(d)?, d)))
        .collect();
    let count = |kinds: &[Kind]| changed.iter().filter(|c| kinds.contains(&c.0)).count() as i64;
    let chips: Vec<Chip> = [
        (count(&[Kind::New, Kind::NewOrphan]), "new", "new", "new"),
        (count(&[Kind::Removed]), "removed", "removed", "removed"),
        (
            count(&[Kind::Deeper]),
            "moved deeper",
            "moved deeper",
            "deeper",
        ),
        (count(&[Kind::Broke]), "broke", "broke", "broke"),
        (
            count(&[Kind::NewOrphan, Kind::LostLinks]),
            "new orphan",
            "new orphans",
            "orphan",
        ),
        (
            count(&[Kind::NowNoindex]),
            "now noindex",
            "now noindex",
            "noindex",
        ),
    ]
    .into_iter()
    .filter(|c| c.0 > 0)
    .map(|(n, one, many, kind)| Chip {
        text: plural(n, one, many),
        kind,
    })
    .collect();
    let drawn: Vec<(Kind, &PageDiff)> = changed.iter().copied().filter(|c| c.0.drawn()).collect();
    let tree = (!drawn.is_empty()).then(|| tree(base, domain, &drawn));
    let nothing = drawn.is_empty().then(|| {
        format!(
            "No page changed between #{} and #{}.",
            previous.number, latest.number
        )
    });
    let better: Vec<(&'static str, String)> = [
        ("Broken pages fixed", count(&[Kind::Fixed])),
        ("Moved closer to home", count(&[Kind::Shallower])),
        ("Added to the sitemap", count(&[Kind::AddedToSitemap])),
    ]
    .into_iter()
    .filter(|b| b.1 > 0)
    .map(|(label, n)| (label, fmt::thousands(n)))
    .collect();
    ChangesView {
        note: None,
        from_chip: chip(previous),
        to_chip: chip(latest),
        chips,
        tree,
        nothing,
        capped: (diffs.len() as i64 >= MAX_DIFFS).then(|| {
            format!(
                "Counting the first {} changed URLs.",
                fmt::thousands(MAX_DIFFS)
            )
        }),
        health: health(history),
        problem: problem(base, domain, &changed),
        no_problem: format!("No new problems since #{}.", previous.number),
        better,
        no_better: format!("Nothing got better since #{}.", previous.number),
        changes_href: format!("{base}/changes"),
    }
}

/// The folder a changed page sits under: `/blog`, or `/` at the top level.
fn folder_of(url: &str) -> String {
    let path = Url::parse(url)
        .map(|u| u.path().to_owned())
        .unwrap_or_default();
    match top_folder(&path) {
        "" => "/".to_owned(),
        f => f.to_owned(),
    }
}

fn folder_note(kinds: &[Kind]) -> String {
    let n = |ks: &[Kind]| kinds.iter().filter(|k| ks.contains(k)).count() as i64;
    let parts: Vec<String> = [
        (n(&[Kind::New]), "+", ""),
        (n(&[Kind::Removed]), "−", ""),
        (n(&[Kind::Deeper]), "", " deeper"),
        (n(&[Kind::Broke]), "", " broke"),
        (n(&[Kind::NewOrphan, Kind::LostLinks]), "", " not linked"),
        (n(&[Kind::NowNoindex]), "", " noindex"),
        (n(&[Kind::NowRedirects]), "", " redirect"),
    ]
    .into_iter()
    .filter(|p| p.0 > 0)
    .map(|(k, pre, post)| format!("{pre}{}{post}", fmt::thousands(k)))
    .collect();
    parts.join(" · ")
}

fn tree(base: &str, domain: &str, drawn: &[(Kind, &PageDiff)]) -> ChangeTree {
    let mut by_folder: BTreeMap<String, Vec<(Kind, &PageDiff)>> = BTreeMap::new();
    for &(kind, d) in drawn {
        by_folder
            .entry(folder_of(&d.url))
            .or_default()
            .push((kind, d));
    }
    let mut folders: Vec<(String, Vec<(Kind, &PageDiff)>)> = by_folder.into_iter().collect();
    folders.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
    let more_folders = (folders.len() > MAX_FOLDERS).then(|| {
        format!(
            "+ {} changed",
            plural(
                (folders.len() - MAX_FOLDERS) as i64,
                "more folder",
                "more folders"
            )
        )
    });
    folders.truncate(MAX_FOLDERS);

    let mut y = 0;
    let mut cards = Vec::new();
    let mut pages = Vec::new();
    let mut page_paths = Vec::new();
    for (label, mut items) in folders {
        items.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.url.cmp(&b.1.url)));
        let kinds: Vec<Kind> = items.iter().map(|i| i.0).collect();
        let extra = items.len().saturating_sub(MAX_PAGES);
        let rows = items.len().min(MAX_PAGES) + usize::from(extra > 0);
        let group_h = rows as u32 * PITCH - (PITCH - CARD_H);
        let folder_top = y + (group_h - CARD_H) / 2;
        let fy = folder_top + CARD_H / 2;
        for (r, (kind, d)) in items.iter().take(MAX_PAGES).enumerate() {
            let top = y + r as u32 * PITCH;
            page_paths.push(TreePath {
                d: link(FOLDER_X + FOLDER_W, fy, PAGE_X, top + CARD_H / 2),
                class: match kind {
                    Kind::Broke => " is-broke",
                    Kind::NewOrphan | Kind::LostLinks => " is-orphan",
                    Kind::Removed => " is-removed",
                    _ => "",
                },
            });
            pages.push(PageCard {
                top,
                path: short_url(&d.url, domain),
                href: (*kind != Kind::Removed).then(|| page_href(base, d.url_hash)),
                badge: badge(*kind, d),
                kind: kind.class(),
            });
        }
        if extra > 0 {
            let top = y + MAX_PAGES as u32 * PITCH;
            page_paths.push(TreePath {
                d: link(FOLDER_X + FOLDER_W, fy, PAGE_X, top + CARD_H / 2),
                class: " is-removed",
            });
            pages.push(PageCard {
                top,
                path: format!("+ {}", plural(extra as i64, "more page", "more pages")),
                href: None,
                badge: String::new(),
                kind: "more",
            });
        }
        let all_orphans = kinds
            .iter()
            .all(|k| matches!(k, Kind::NewOrphan | Kind::LostLinks));
        cards.push(FolderCard {
            top: folder_top,
            note: folder_note(&kinds),
            label,
            class: if kinds.contains(&Kind::Broke) {
                " is-broke"
            } else if all_orphans {
                " is-orphan"
            } else {
                ""
            },
        });
        y += group_h + GROUP_GAP;
    }
    let height = (y - GROUP_GAP).max(HOME_H);
    let home_top = (height - HOME_H) / 2;
    let hy = home_top + HOME_H / 2;
    let mut paths: Vec<TreePath> = cards
        .iter()
        .map(|c| TreePath {
            d: link(HOME_W, hy, FOLDER_X, c.top + CARD_H / 2),
            class: "",
        })
        .collect();
    paths.extend(page_paths);
    ChangeTree {
        width: TREE_W,
        height,
        paths,
        home_top,
        home_note: plural(drawn.len() as i64, "change", "changes"),
        folders: cards,
        pages,
        more_folders,
    }
}

/// A connector from `(x1, y1)` to `(x2, y2)`, leaving and arriving level.
fn link(x1: u32, y1: u32, x2: u32, y2: u32) -> String {
    let mid = (x1 + x2) / 2;
    format!("M{x1} {y1}C{mid} {y1} {mid} {y2} {x2} {y2}")
}

fn health(history: &[HealthPoint]) -> Option<Health> {
    let (last, prev) = match history {
        [.., prev, last] => (last, prev),
        _ => return None,
    };
    let lo = history.iter().map(|p| p.score).min().unwrap_or(0);
    let hi = history.iter().map(|p| p.score).max().unwrap_or(0);
    let span = f64::from((hi - lo).max(1));
    let step = (SPARK_W - 20.0) / (history.len() - 1) as f64;
    let xy: Vec<(String, String)> = history
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let x = 10.0 + step * i as f64;
            let y = SPARK_H - 8.0 - f64::from(p.score - lo) / span * (SPARK_H - 20.0);
            (format!("{x:.1}"), format!("{y:.1}"))
        })
        .collect();
    let diff = i64::from(last.score) - i64::from(prev.score);
    let (delta, delta_class) = match diff {
        0 => (format!("same as #{}", prev.number), "c-muted"),
        d if d > 0 => (format!("+{d} since #{}", prev.number), "c-ok"),
        d => (format!("−{} since #{}", -d, prev.number), "c-err"),
    };
    let scores: Vec<String> = history.iter().map(|p| p.score.to_string()).collect();
    Some(Health {
        score: last.score.to_string(),
        delta,
        delta_class,
        points: xy
            .iter()
            .map(|(x, y)| format!("{x},{y}"))
            .collect::<Vec<_>>()
            .join(" "),
        last: xy.last().cloned().unwrap_or_default(),
        dots: xy[..xy.len() - 1].to_vec(),
        first_label: format!("#{}", history[0].number),
        last_label: format!("#{}", last.number),
        aria: format!(
            "Health score over the last {}: {}",
            plural(history.len() as i64, "crawl", "crawls"),
            scores.join(", ")
        ),
    })
}

/// The worst new problem: a page that broke (the most linked one), else pages that lost their
/// links, moved deeper or left search.
fn problem(base: &str, domain: &str, changed: &[(Kind, &PageDiff)]) -> Option<Problem> {
    let broke = changed
        .iter()
        .filter(|c| c.0 == Kind::Broke)
        .filter_map(|c| Some((c.1, c.1.after.as_ref()?)))
        .max_by_key(|(_, a)| a.inlinks);
    if let Some((d, a)) = broke {
        let n = i64::from(a.inlinks);
        let still = match n {
            0 => "No page links to it now.".to_owned(),
            1 => "1 page still links to it.".to_owned(),
            n => format!("{} pages still link to it.", fmt::thousands(n)),
        };
        let what = if a.status == 0 {
            "stopped responding".to_owned()
        } else {
            format!("started returning {}", a.status)
        };
        return Some(Problem {
            path: Some(short_url(&d.url, domain)),
            text: format!(" {what}. {still}"),
            href: format!("{base}/graph/links"),
            link: "See who links to it",
        });
    }
    let n = |ks: &[Kind]| changed.iter().filter(|c| ks.contains(&c.0)).count() as i64;
    let orphans = n(&[Kind::NewOrphan, Kind::LostLinks]);
    if orphans > 0 {
        return Some(Problem {
            path: None,
            text: format!(
                "{} no link reaches. Search engines may drop them.",
                plural(orphans, "page", "pages")
            ),
            href: format!("{base}/graph/sitemap"),
            link: "See the sitemap",
        });
    }
    let deeper = n(&[Kind::Deeper]);
    if deeper > 0 {
        let deepest = changed
            .iter()
            .filter(|c| c.0 == Kind::Deeper)
            .filter_map(|c| c.1.after.as_ref()?.depth)
            .max()
            .unwrap_or(0);
        return Some(Problem {
            path: None,
            text: format!(
                "{} moved deeper. The deepest now sits {} from home.",
                plural(deeper, "page", "pages"),
                plural(i64::from(deepest), "click", "clicks")
            ),
            href: format!("{base}/graph/sitemap#depth"),
            link: "See click depth",
        });
    }
    let noindex = n(&[Kind::NowNoindex]);
    (noindex > 0).then(|| Problem {
        path: None,
        text: format!(
            "{} left search: noindex, canonical elsewhere or blocked.",
            plural(noindex, "page", "pages")
        ),
        href: format!("{base}/explorer?filter=nx"),
        link: "See non-indexable pages",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(status: u16, depth: Option<u32>, ix: Indexability) -> PageState {
        PageState {
            status,
            depth,
            indexability: ix,
            in_sitemap: true,
            inlinks: 2,
        }
    }

    fn diff(path: &str, before: Option<PageState>, after: Option<PageState>) -> PageDiff {
        PageDiff {
            url: format!("https://e.com{path}"),
            url_hash: path.len() as u64,
            before,
            after,
        }
    }

    const OK: Indexability = Indexability::Indexable;

    #[test]
    fn each_diff_gets_its_most_telling_kind() {
        let ok = |d| Some(state(200, d, OK));
        let cases = [
            (diff("/a", None, ok(Some(2))), Some(Kind::New)),
            (diff("/b", None, ok(None)), Some(Kind::NewOrphan)),
            (diff("/c", ok(Some(1)), None), Some(Kind::Removed)),
            (
                diff(
                    "/d",
                    ok(Some(3)),
                    Some(state(404, Some(5), Indexability::ClientError)),
                ),
                Some(Kind::Broke),
            ),
            (diff("/e", ok(Some(3)), ok(Some(5))), Some(Kind::Deeper)),
            (diff("/f", ok(Some(3)), ok(None)), Some(Kind::LostLinks)),
            (
                diff(
                    "/g",
                    ok(Some(2)),
                    Some(state(200, Some(2), Indexability::Noindex)),
                ),
                Some(Kind::NowNoindex),
            ),
            (
                diff(
                    "/h",
                    Some(state(404, Some(2), Indexability::ClientError)),
                    ok(Some(2)),
                ),
                Some(Kind::Fixed),
            ),
            (diff("/i", ok(Some(4)), ok(Some(2))), Some(Kind::Shallower)),
            (diff("/j", ok(Some(2)), ok(Some(2))), None),
        ];
        for (d, want) in cases {
            assert_eq!(classify(&d), want, "{}", d.url);
        }
        let d = diff("/e", ok(Some(3)), ok(Some(5)));
        assert_eq!(badge(Kind::Deeper, &d), "3 → 5 clicks");
        let d = diff(
            "/d",
            ok(Some(3)),
            Some(state(404, Some(5), Indexability::ClientError)),
        );
        assert_eq!(badge(Kind::Broke, &d), "200 → 404");
    }

    #[test]
    fn the_tree_groups_pages_under_their_folders() {
        let ok = |d| Some(state(200, d, OK));
        let diffs = [
            diff("/blog/a", None, ok(Some(2))),
            diff("/blog/b", ok(Some(3)), ok(Some(5))),
            diff("/pricing", ok(Some(1)), None),
        ];
        let drawn: Vec<(Kind, &PageDiff)> =
            diffs.iter().map(|d| (classify(d).unwrap(), d)).collect();
        let t = tree("/s/x", "e.com", &drawn);
        assert_eq!(t.folders.len(), 2);
        assert_eq!(t.folders[0].label, "/blog");
        assert_eq!(t.folders[0].note, "+1 · 1 deeper");
        assert_eq!(t.folders[1].label, "/");
        assert_eq!(t.folders[1].note, "−1");
        // Two rows (80 px) for /blog, a 16 px gap, one row for /.
        assert_eq!((t.folders[0].top, t.folders[1].top), (22, 96));
        assert_eq!(t.height, 132);
        assert_eq!(t.pages[2].href, None);
        assert_eq!(t.pages[2].badge, "removed");
        assert_eq!(t.paths.len(), 5);
        assert_eq!(t.home_note, "3 changes");
    }

    #[test]
    fn health_draws_a_line_and_the_last_change() {
        let h = health(&[
            HealthPoint {
                number: 1,
                score: 80,
            },
            HealthPoint {
                number: 2,
                score: 77,
            },
        ])
        .unwrap();
        assert_eq!(h.delta, "−3 since #1");
        assert_eq!(h.points, "10.0,12.0 270.0,62.0");
        assert_eq!(h.dots.len(), 1);
        assert!(health(&[]).is_none());
    }
}
