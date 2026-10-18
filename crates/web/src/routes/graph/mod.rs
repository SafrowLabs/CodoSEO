//! `/s/{site}/graph/…`: the site graph. Pictures of how the site hangs together (crawl tree,
//! folder tree, bubble map, sitemap, click depth, broken links and redirects, changes) and of
//! what AI can reach and read (llms.txt, AI reach). Each tab is its own page under one tab bar,
//! drawn from the latest finished crawl.

mod changes;
mod depth;
mod links;
mod llms;
mod reach;
mod sitemap;
mod trees;

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use codoseo_store::crawls::{self, Crawl};
use codoseo_store::sites::Site;

use crate::auth::{CurrentUser, load_site};
use crate::error::AppError;
use crate::layout::{Screen, Shell};
use crate::routes::audit::site_id;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/s/{site}/graph", get(index))
        .merge(trees::routes())
        .merge(sitemap::routes())
        .merge(depth::routes())
        .merge(links::routes())
        .merge(changes::routes())
        .merge(llms::routes())
        .merge(reach::routes())
}

/// `/s/{site}/graph` opens the bubble map.
async fn index(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
) -> Result<Response, AppError> {
    let site = load_site(&state, &user, site_id(&site)?).await?;
    Ok(Redirect::to(&format!("/s/{}/graph/{}", site.id, Tab::Map.slug())).into_response())
}

/// One tab of the site graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Tree,
    Folders,
    Map,
    Sitemap,
    Depth,
    Links,
    Changes,
    Llms,
    Reach,
}

impl Tab {
    /// In tab order; the bubble map comes first and is where `/graph` opens.
    pub const ALL: [Tab; 9] = [
        Tab::Map,
        Tab::Tree,
        Tab::Folders,
        Tab::Sitemap,
        Tab::Depth,
        Tab::Links,
        Tab::Changes,
        Tab::Llms,
        Tab::Reach,
    ];

    /// The tab's path segment: `/s/{site}/graph/{slug}`.
    pub fn slug(self) -> &'static str {
        match self {
            Tab::Tree => "tree",
            Tab::Folders => "folders",
            Tab::Map => "map",
            Tab::Sitemap => "sitemap",
            Tab::Depth => "depth",
            Tab::Links => "links",
            Tab::Changes => "changes",
            Tab::Llms => "llms",
            Tab::Reach => "reach",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Tab::Tree => "Crawl tree",
            Tab::Folders => "Folder tree",
            Tab::Map => "Bubble map",
            Tab::Sitemap => "Sitemap",
            Tab::Depth => "Depth",
            Tab::Links => "Broken & redirects",
            Tab::Changes => "Changes",
            Tab::Llms => "llms.txt",
            Tab::Reach => "AI reach",
        }
    }

    /// GEO tabs are about AI; the rest are classic SEO.
    pub fn geo(self) -> bool {
        matches!(self, Tab::Llms | Tab::Reach)
    }
}

pub struct TabLink {
    pub label: &'static str,
    pub href: String,
    pub active: bool,
    /// Set on the first tab of a group: `SEO`, `GEO`.
    pub group: Option<&'static str>,
}

pub fn tabs(base: &str, active: Tab) -> Vec<TabLink> {
    let mut prev_geo = None;
    Tab::ALL
        .iter()
        .map(|&t| {
            let group = (prev_geo != Some(t.geo())).then_some(if t.geo() { "GEO" } else { "SEO" });
            prev_geo = Some(t.geo());
            TabLink {
                label: t.label(),
                href: format!("{base}/graph/{}", t.slug()),
                active: t == active,
                group,
            }
        })
        .collect()
}

/// Shown on every tab until the site has a finished crawl.
pub struct Empty {
    pub text: &'static str,
    /// Offer Run crawl (no crawl is queued or running).
    pub can_run: bool,
}

/// What every tab starts from.
pub struct Ctx {
    pub site: Site,
    pub shell: Shell,
    /// `/s/{site}`
    pub base: String,
    pub tabs: Vec<TabLink>,
    /// The crawl the tab draws; `None` before the first finished crawl, with `empty` set.
    pub latest: Option<Crawl>,
    pub empty: Option<Empty>,
}

/// Loads the site (a 404 for another account's), the shell, the tab bar and the latest finished
/// crawl.
pub async fn load(
    state: &AppState,
    user: &CurrentUser,
    raw_site: &str,
    tab: Tab,
) -> Result<Ctx, AppError> {
    let site = load_site(state, user, site_id(raw_site)?).await?;
    let shell = Shell::load(state, user, Some(&site), Screen::Graph).await?;
    let base = format!("/s/{}", site.id);
    let latest = crawls::latest_done(&state.pool, site.id).await?;
    let empty = match latest {
        Some(_) => None,
        None => {
            let can_run = crawls::active(&state.pool, site.id).await?.is_none();
            Some(Empty {
                text: if can_run {
                    "The site graph is drawn from a finished crawl. Run your first crawl to see it."
                } else {
                    "Your first crawl is on its way. The site graph appears when it finishes."
                },
                can_run,
            })
        }
    };
    Ok(Ctx {
        tabs: tabs(&base, tab),
        site,
        shell,
        base,
        latest,
        empty,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_are_grouped_seo_then_geo() {
        let t = tabs("/s/x", Tab::Depth);
        let groups: Vec<_> = t.iter().filter_map(|l| l.group).collect();
        assert_eq!(groups, ["SEO", "GEO"]);
        assert_eq!(t.iter().filter(|l| l.active).count(), 1);
        assert!(t.iter().any(|l| l.active && l.href == "/s/x/graph/depth"));
    }
}
