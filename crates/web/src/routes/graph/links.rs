//! `/s/{site}/graph/links`: broken pages with the pages that still link to them, and every redirect chain from the first URL to where it lands.

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use codoseo_store::site_graph::{self, LinkSource, LinkTotals, LinkedPage, RedirectChain};
use url::Url;

use super::sitemap::{page_href, plural, short_url, status_class, status_label};
use super::{Empty, Tab, TabLink};
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::fmt;
use crate::layout::Shell;
use crate::render::html;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/s/{site}/graph/links", get(links_page))
}

/// Broken pages listed under the diagram.
const MAX_BROKEN: i64 = 13;
/// Redirect chains listed.
const MAX_CHAINS: i64 = 12;
/// Linking pages the diagram draws before `+ N more`.
const MAX_SOURCES: usize = 8;
/// Pages read when looking for a stand-in for the broken page.
const STAND_IN_POOL: i64 = 300;
/// The diagram's geometry, in px: a source row's height and pitch, and the cards' x.
const SRC_H: u32 = 36;
const SRC_PITCH: u32 = 44;
const SRC_W: u32 = 280;
const TARGET_X: u32 = 520;
const TARGET_W: u32 = 260;
const TARGET_H: u32 = 72;
const FIX_X: u32 = 820;
const FIX_H: u32 = 112;
const FAN_W: u32 = 1100;

#[derive(Template)]
#[template(path = "graph/links.html")]
pub struct LinksTab {
    pub shell: Shell,
    pub base: String,
    pub tabs: Vec<TabLink>,
    pub empty: Option<Empty>,
    pub view: Option<LinksView>,
}

pub struct LinksView {
    pub tiles: Vec<LinkTile>,
    /// The broken page most pages link to, drawn with its sources.
    pub fan: Option<Fan>,
    /// The other broken pages.
    pub broken: Vec<BrokenRow>,
    /// `+ 4 more broken pages`
    pub broken_more: Option<String>,
    pub chains: Vec<ChainView>,
    pub chains_more: Option<String>,
}

pub struct LinkTile {
    pub label: &'static str,
    pub value: String,
    pub sub: String,
    /// ` gv-tile-err` or empty.
    pub class: &'static str,
    /// `c-err`, `gv-c-redirect` or empty, for the value.
    pub tone: &'static str,
}

/// Sources on the left, the broken page in the middle, the way to fix it on the right, laid
/// out here so the template only places them.
pub struct Fan {
    pub width: u32,
    pub height: u32,
    /// Curves from each source to the target.
    pub paths: Vec<String>,
    /// The dashed line from the target to the fix.
    pub fix_path: String,
    pub sources: Vec<FanSource>,
    pub target: FanTarget,
    pub fix: FanFix,
}

pub struct FanSource {
    pub top: u32,
    pub path: String,
    /// `"rate limits"`, or `+ 3 more` on the last row.
    pub anchor: String,
    pub more: bool,
}

pub struct FanTarget {
    pub top: u32,
    pub path: String,
    pub href: String,
    pub status: String,
    pub status_class: &'static str,
    /// `Linked from 6 pages`
    pub note: String,
}

pub struct FanFix {
    pub top: u32,
    /// A page to redirect to was found.
    pub found: bool,
    pub title: &'static str,
    /// `Redirect it to `, then `path`, then `rest`.
    pub lead: &'static str,
    pub path: String,
    pub rest: String,
}

pub struct BrokenRow {
    pub path: String,
    pub href: String,
    pub status: String,
    pub status_class: &'static str,
    /// `linked from 4 pages`
    pub linked: String,
}

pub struct ChainView {
    /// `13 links`
    pub links: String,
    pub hops: Vec<Hop>,
    /// `2 hops`
    pub hop_count: String,
    /// `Ends broken`, `Temporary redirect`, or empty.
    pub flag: &'static str,
    /// ` is-broken`, ` is-temporary` or empty.
    pub class: &'static str,
    /// The explorer with the first URL selected (its inlinks), when anything links to it.
    pub href: Option<String>,
}

pub struct Hop {
    pub path: String,
    pub badge: String,
    pub badge_class: &'static str,
    /// ` is-end`, ` is-end-bad` on where the chain lands, else empty.
    pub class: &'static str,
}

async fn links_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
) -> Result<Response, AppError> {
    let ctx = super::load(&state, &user, &site, Tab::Links).await?;
    let view = match &ctx.latest {
        Some(crawl) => {
            let pool = &state.pool;
            let domain = &ctx.site.domain;
            let totals = site_graph::link_totals(pool, crawl.id).await?;
            let broken = site_graph::broken_pages(pool, crawl.id, MAX_BROKEN).await?;
            let chains = site_graph::redirect_chains(pool, crawl.id, MAX_CHAINS).await?;
            let fan = match broken.first() {
                Some(top) => {
                    let sources =
                        site_graph::link_sources(pool, crawl.id, top.url_hash, MAX_SOURCES as i64)
                            .await?;
                    let (folder, slug) = folder_and_slug(&top.url);
                    let candidates = match &folder {
                        Some(f) => {
                            site_graph::stand_ins(pool, crawl.id, f, &slug, STAND_IN_POOL).await?
                        }
                        None => Vec::new(),
                    };
                    let stand_in = best_stand_in(&top.url, &slug, &candidates);
                    Some(fan(&ctx.base, domain, top, &sources, stand_in))
                }
                None => None,
            };
            let rest = broken.iter().skip(1);
            let shown = broken.len().saturating_sub(1) as i64;
            Some(LinksView {
                tiles: tiles(&totals),
                fan,
                broken: rest
                    .map(|p| BrokenRow {
                        path: short_url(&p.url, domain),
                        href: page_href(&ctx.base, p.url_hash),
                        status: status_label(p.status),
                        status_class: status_class(p.status),
                        linked: format!(
                            "linked from {}",
                            plural(i64::from(p.inlinks), "page", "pages")
                        ),
                    })
                    .collect(),
                broken_more: (totals.broken > shown + 1).then(|| {
                    format!(
                        "+ {}",
                        plural(
                            totals.broken - shown - 1,
                            "more broken page",
                            "more broken pages"
                        )
                    )
                }),
                chains: chains
                    .iter()
                    .map(|c| chain_view(&ctx.base, domain, c))
                    .collect(),
                chains_more: (totals.redirects > chains.len() as i64).then(|| {
                    format!(
                        "+ {}",
                        plural(
                            totals.redirects - chains.len() as i64,
                            "more redirect",
                            "more redirects"
                        )
                    )
                }),
            })
        }
        None => None,
    };
    Ok(html(&LinksTab {
        shell: ctx.shell,
        base: ctx.base,
        tabs: ctx.tabs,
        empty: ctx.empty,
        view,
    })?
    .into_response())
}

fn tiles(t: &LinkTotals) -> Vec<LinkTile> {
    vec![
        LinkTile {
            label: "Broken pages",
            value: fmt::thousands(t.broken),
            sub: format!("linked from {}", plural(t.broken_links, "page", "pages")),
            class: if t.broken > 0 { " gv-tile-err" } else { "" },
            tone: if t.broken > 0 { "c-err" } else { "" },
        },
        LinkTile {
            label: "Links pointing at redirects",
            value: fmt::thousands(t.redirect_links),
            sub: format!("across {}", plural(t.redirects, "redirect", "redirects")),
            class: "",
            tone: if t.redirect_links > 0 {
                "gv-c-redirect"
            } else {
                ""
            },
        },
        LinkTile {
            label: "Longest chain",
            value: plural(i64::from(t.longest_chain), "hop", "hops"),
            sub: "before the final page".to_owned(),
            class: "",
            tone: "",
        },
        LinkTile {
            label: "Chains that end broken",
            value: fmt::thousands(t.ends_broken),
            sub: "land on an error or never land".to_owned(),
            class: "",
            tone: if t.ends_broken > 0 { "c-err" } else { "" },
        },
    ]
}

/// The folder a URL sits in (absolute, ending in `/`) and its last path segment.
fn folder_and_slug(url: &str) -> (Option<String>, String) {
    let Ok(u) = Url::parse(url) else {
        return (None, String::new());
    };
    let path = u.path().trim_end_matches('/');
    let (dir, slug) = path.rsplit_once('/').unwrap_or(("", path));
    let mut folder = u.clone();
    folder.set_path(&format!("{dir}/"));
    folder.set_query(None);
    folder.set_fragment(None);
    (Some(folder.to_string()), slug.to_owned())
}

/// The words of a path segment: `rate-limits.html` -> `rate`, `limits`, `html`.
fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The candidate most like the broken URL: the same last segment elsewhere beats sharing
/// words with it, more shared words beat fewer, then the shorter URL. `None` when none share
/// a word.
fn best_stand_in<'a>(broken: &str, slug: &str, candidates: &'a [String]) -> Option<&'a str> {
    let want = words(slug);
    candidates
        .iter()
        .filter(|c| c.as_str() != broken)
        .filter_map(|c| {
            let (_, their_slug) = folder_and_slug(c);
            let theirs = words(&their_slug);
            let shared = want.iter().filter(|w| theirs.contains(w)).count();
            let same = !slug.is_empty() && their_slug == slug;
            let score = usize::from(same) * 1000 + shared;
            (score > 0).then_some((score, c))
        })
        .max_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.len().cmp(&a.1.len())))
        .map(|(_, c)| c.as_str())
}

fn fan(
    base: &str,
    domain: &str,
    target: &LinkedPage,
    sources: &[LinkSource],
    stand_in: Option<&str>,
) -> Fan {
    let extra = (target.inlinks as usize).saturating_sub(sources.len());
    let mut rows: Vec<FanSource> = sources
        .iter()
        .map(|s| FanSource {
            top: 0,
            path: short_url(&s.from_url, domain),
            anchor: match s.anchor.as_deref().map(str::trim) {
                Some(a) if !a.is_empty() => format!("\"{a}\""),
                _ => "no anchor text".to_owned(),
            },
            more: false,
        })
        .collect();
    if extra > 0 {
        rows.push(FanSource {
            top: 0,
            path: String::new(),
            anchor: format!("+ {}", plural(extra as i64, "more page", "more pages")),
            more: true,
        });
    }
    for (i, r) in rows.iter_mut().enumerate() {
        r.top = i as u32 * SRC_PITCH;
    }
    let lines = rows.len() as u32;
    let height = (lines * SRC_PITCH)
        .saturating_sub(SRC_PITCH - SRC_H)
        .max(FIX_H);
    let mid = height / 2;
    let bend = (SRC_W + TARGET_X) / 2;
    let paths = rows
        .iter()
        .map(|r| {
            let y = r.top + SRC_H / 2;
            format!("M{SRC_W} {y}C{bend} {y} {bend} {mid} {TARGET_X} {mid}")
        })
        .collect();
    let links = plural(i64::from(target.inlinks), "page", "pages");
    let fix = match stand_in {
        Some(url) => FanFix {
            top: mid - FIX_H / 2,
            found: true,
            title: "Suggested fix",
            lead: "Redirect it to ",
            path: short_url(url, domain),
            rest: format!(". That page returns 200. Fixes the links from all {links} at once."),
        },
        None => FanFix {
            top: mid - FIX_H / 2,
            found: false,
            title: "Restore or redirect",
            lead: "",
            path: String::new(),
            rest: "No page with a similar path returns 200. Restore the page, or redirect it \
                   to its closest match."
                .to_owned(),
        },
    };
    Fan {
        width: FAN_W,
        height,
        paths,
        fix_path: format!("M{} {mid}H{FIX_X}", TARGET_X + TARGET_W),
        sources: rows,
        target: FanTarget {
            top: mid - TARGET_H / 2,
            path: short_url(&target.url, domain),
            href: page_href(base, target.url_hash),
            status: status_label(target.status),
            status_class: status_class(target.status),
            note: format!("Linked from {links}"),
        },
        fix,
    }
}

fn chain_view(base: &str, domain: &str, c: &RedirectChain) -> ChainView {
    let temporary = c.hops.iter().any(|h| matches!(h.0, 302 | 303 | 307));
    let end_bad = match (&c.target, c.target_status) {
        (None, _) => true,
        (Some(_), Some(s)) => s == 0 || s >= 400,
        (Some(_), None) => false,
    };
    let mut hops: Vec<Hop> = c
        .hops
        .iter()
        .map(|(status, url)| Hop {
            path: short_url(url, domain),
            badge: if matches!(status, 302 | 303 | 307) {
                format!("{status} temporary")
            } else {
                status.to_string()
            },
            badge_class: status_class(*status),
            class: "",
        })
        .collect();
    hops.push(match &c.target {
        Some(url) => Hop {
            path: short_url(url, domain),
            badge: c
                .target_status
                .map_or_else(|| "not crawled".to_owned(), status_label),
            badge_class: c.target_status.map_or("t-muted", status_class),
            class: if end_bad { " is-end-bad" } else { " is-end" },
        },
        None => Hop {
            path: "never lands".to_owned(),
            badge: "loop".to_owned(),
            badge_class: "t-err",
            class: " is-end-bad",
        },
    });
    let (flag, class) = if end_bad {
        ("Ends broken", " is-broken")
    } else if temporary {
        ("Temporary redirect", " is-temporary")
    } else {
        ("", "")
    };
    ChainView {
        links: plural(i64::from(c.inlinks), "link", "links"),
        hops,
        hop_count: plural(c.hops.len() as i64, "hop", "hops"),
        flag,
        class,
        href: (c.inlinks > 0).then(|| page_href(base, c.url_hash)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_and_slug_come_off_the_path() {
        assert_eq!(
            folder_and_slug("https://e.com/docs/api/rate-limits?x=1"),
            (
                Some("https://e.com/docs/api/".to_owned()),
                "rate-limits".to_owned()
            )
        );
        assert_eq!(
            folder_and_slug("https://e.com/old/"),
            (Some("https://e.com/".to_owned()), "old".to_owned())
        );
    }

    #[test]
    fn the_stand_in_shares_the_most_words() {
        let c = vec![
            "https://e.com/docs/api/auth".to_owned(),
            "https://e.com/docs/api/limits".to_owned(),
            "https://e.com/docs/api/rate-limits-v2".to_owned(),
        ];
        assert_eq!(
            best_stand_in("https://e.com/docs/api/rate-limits", "rate-limits", &c),
            Some("https://e.com/docs/api/rate-limits-v2")
        );
        let moved = vec![
            "https://e.com/docs/api/limits".to_owned(),
            "https://e.com/guides/rate-limits".to_owned(),
        ];
        assert_eq!(
            best_stand_in("https://e.com/docs/api/rate-limits", "rate-limits", &moved),
            Some("https://e.com/guides/rate-limits")
        );
        assert_eq!(best_stand_in("https://e.com/x/gone", "gone", &c), None);
    }

    #[test]
    fn a_chain_ending_in_an_error_is_flagged() {
        let c = RedirectChain {
            url: "https://e.com/a".to_owned(),
            url_hash: 7,
            hops: vec![
                (301, "https://e.com/a".to_owned()),
                (302, "https://e.com/b".to_owned()),
            ],
            target: Some("https://e.com/c".to_owned()),
            target_status: Some(404),
            inlinks: 3,
        };
        let v = chain_view("/s/x", "e.com", &c);
        assert_eq!(v.flag, "Ends broken");
        assert_eq!(v.links, "3 links");
        assert_eq!(v.hop_count, "2 hops");
        assert_eq!(v.hops.len(), 3);
        assert_eq!(v.hops[1].badge, "302 temporary");
        assert_eq!(v.hops[2].path, "/c");
        assert_eq!(v.hops[2].class, " is-end-bad");
        let fine = RedirectChain {
            target_status: Some(200),
            ..c
        };
        assert_eq!(
            chain_view("/s/x", "e.com", &fine).flag,
            "Temporary redirect"
        );
    }
}
