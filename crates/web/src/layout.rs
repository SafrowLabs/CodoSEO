//! The app shell around every signed-in screen (v3 layout, warmbly chrome): sidebar with the
//! site switcher, crawler status and nav groups; header with breadcrumb, ⌘K and Run crawl.
//!
//! Every screen template extends `base.html` and has a `shell: Shell` field, built once per
//! request by [`Shell::load`].

use codoseo_checks::def;
use codoseo_core::check::{CheckId, Severity};
use codoseo_core::plan::{Plan, PlanLimits};
use codoseo_store::crawls::{Crawl, CrawlStatus};
use codoseo_store::sites::Site;
use uuid::Uuid;

use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::fmt;
use crate::state::AppState;

/// Which screen is showing, for the nav highlight and the breadcrumb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Explorer,
    Audit,
    Changes,
    Crawls,
    Sites,
    Account,
    Admin,
}

impl Screen {
    pub fn title(self) -> &'static str {
        match self {
            Screen::Explorer => "URL explorer",
            Screen::Audit => "Site audit",
            Screen::Changes => "Changes",
            Screen::Crawls => "Crawls",
            Screen::Sites => "Sites",
            Screen::Account => "Account",
            Screen::Admin => "Admin",
        }
    }
}

pub struct UserView {
    pub email: String,
    pub initials: String,
    /// `cloud · pro`, `self-hosted · owner`
    pub plan_label: String,
}

pub struct SiteLink {
    pub domain: String,
    pub initial: String,
    pub href: String,
    pub active: bool,
}

pub struct SiteView {
    pub id: Uuid,
    pub domain: String,
    pub initial: String,
    /// `/s/{id}`
    pub base: String,
    /// `1,284 urls` or `not crawled yet`
    pub urls_label: String,
    /// `crawl #48 · 12m ago`
    pub last_crawl: Option<String>,
}

pub struct NavItem {
    pub label: &'static str,
    pub href: String,
    pub count: String,
    /// The count is drawn in red (critical issues, critical changes).
    pub alert: bool,
    pub active: bool,
    /// Keyboard shortcut shown in the tooltip, e.g. `G E`.
    pub keys: &'static str,
    /// Not built yet (M7): shown dimmed with a "soon" tag.
    pub soon: bool,
    /// Symbol id in `partials/icons.html`.
    pub icon: &'static str,
}

pub struct NavGroup {
    pub title: &'static str,
    pub items: Vec<NavItem>,
}

/// The crawler status card in the sidebar.
pub struct CrawlerView {
    /// `idle`, `queued`, `running`, `failed`
    pub state: &'static str,
    pub label: String,
    pub rows: Vec<(String, String)>,
    /// Progress through the page budget while running, 0–100.
    pub pct: Option<u8>,
    /// Set while a crawl is queued or running: the card polls this URL every 2 s.
    pub poll_url: Option<String>,
}

pub struct Shell {
    pub screen: Screen,
    pub user: UserView,
    pub sites: Vec<SiteLink>,
    pub site: Option<SiteView>,
    pub nav: Vec<NavGroup>,
    pub crawler: Option<CrawlerView>,
    pub version: &'static str,
    pub can_add_site: bool,
}

impl Shell {
    pub fn title(&self) -> String {
        match &self.site {
            Some(s) => format!("{} · {} — CodoSEO", self.screen.title(), s.domain),
            None => format!("{} — CodoSEO", self.screen.title()),
        }
    }

    /// Loads everything the shell shows. `site` is the site in view, if the screen has one.
    pub async fn load(
        state: &AppState,
        user: &CurrentUser,
        site: Option<&Site>,
        screen: Screen,
    ) -> Result<Shell, AppError> {
        let pool = &state.pool;
        let sites = codoseo_store::sites::list_for_account(pool, user.id()).await?;
        let limits = PlanLimits::for_plan(user.account.plan);
        let can_add_site = limits
            .max_sites
            .is_none_or(|max| (sites.len() as u32) < max);

        let mut site_view = None;
        let mut nav = Vec::new();
        let mut crawler = None;
        if let Some(site) = site {
            let latest = codoseo_store::crawls::latest_done(pool, site.id).await?;
            let active = codoseo_store::crawls::active(pool, site.id).await?;
            let changes = match &latest {
                Some(c) => change_counts(state, c.id).await?,
                None => (0, false),
            };
            let summary = latest.as_ref().and_then(Crawl::summary);
            let base = format!("/s/{}", site.id);
            site_view = Some(SiteView {
                id: site.id,
                domain: site.domain.clone(),
                initial: initial(&site.domain),
                base: base.clone(),
                urls_label: match &summary {
                    Some(s) => {
                        let n = s.report_summary.pages;
                        format!("{} url{}", fmt::thousands(n), if n == 1 { "" } else { "s" })
                    }
                    None => "not crawled yet".to_owned(),
                },
                last_crawl: latest.as_ref().map(|c| {
                    let when = c.finished_at.map(fmt::ago).unwrap_or_default();
                    format!("crawl #{} · {when}", c.number)
                }),
            });
            nav = nav_groups(&base, screen, latest.as_ref(), summary.as_ref(), changes);
            crawler = Some(crawler_view(
                &base,
                latest.as_ref(),
                active.as_ref(),
                user.account.plan,
            ));
        } else {
            nav.push(NavGroup {
                title: "WORKSPACE",
                items: vec![workspace_item(
                    "Sites",
                    "/sites",
                    screen == Screen::Sites,
                    "",
                    "i-globe",
                )],
            });
        }
        nav.push(NavGroup {
            title: if site.is_some() {
                "WORKSPACE"
            } else {
                "ACCOUNT"
            },
            items: vec![
                NavItem {
                    soon: true,
                    ..workspace_item("Alert rules", "#", false, "", "i-bell")
                },
                NavItem {
                    soon: true,
                    ..workspace_item("Schedule", "#", false, "", "i-calendar")
                },
                workspace_item(
                    "Settings",
                    "/account",
                    screen == Screen::Account,
                    "G S",
                    "i-settings",
                ),
            ],
        });

        let sites = sites
            .iter()
            .map(|s| SiteLink {
                domain: s.domain.clone(),
                initial: initial(&s.domain),
                href: format!("/s/{}/audit", s.id),
                active: site.is_some_and(|cur| cur.id == s.id),
            })
            .collect();

        Ok(Shell {
            screen,
            user: user_view(user),
            sites,
            site: site_view,
            nav,
            crawler,
            version: env!("CARGO_PKG_VERSION"),
            can_add_site,
        })
    }
}

/// The sidebar crawler card for one site, on its own (the `/s/{site}/status` poll).
pub async fn crawler_for(
    state: &AppState,
    site: &Site,
    plan: Plan,
) -> Result<CrawlerView, AppError> {
    let latest = codoseo_store::crawls::latest_done(&state.pool, site.id).await?;
    let active = codoseo_store::crawls::active(&state.pool, site.id).await?;
    Ok(crawler_view(
        &format!("/s/{}", site.id),
        latest.as_ref(),
        active.as_ref(),
        plan,
    ))
}

fn workspace_item(
    label: &'static str,
    href: &str,
    active: bool,
    keys: &'static str,
    icon: &'static str,
) -> NavItem {
    NavItem {
        label,
        href: href.to_owned(),
        count: String::new(),
        alert: false,
        active,
        keys,
        soon: false,
        icon,
    }
}

async fn change_counts(state: &AppState, crawl_id: Uuid) -> Result<(i64, bool), AppError> {
    let row: (i64, Option<bool>) = sqlx::query_as(
        "SELECT count(*), bool_or(severity = 'critical') FROM changes WHERE crawl_id = $1",
    )
    .bind(crawl_id)
    .fetch_one(&state.pool)
    .await?;
    Ok((row.0, row.1.unwrap_or(false)))
}

pub fn initial(domain: &str) -> String {
    domain
        .trim_start_matches("www.")
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_owned())
}

fn user_view(user: &CurrentUser) -> UserView {
    let email = &user.account.email;
    let local = email.split('@').next().unwrap_or(email);
    let mut parts = local
        .split(['.', '_', '-', '+'])
        .filter(|p| !p.is_empty())
        .filter_map(|p| p.chars().next());
    let initials: String = match (parts.next(), parts.next()) {
        (Some(a), Some(b)) => [a, b].iter().collect(),
        (Some(_), None) => local.chars().take(2).collect(),
        _ => "?".to_owned(),
    };
    let plan = match user.account.plan {
        Plan::Free => "cloud · free",
        Plan::Pro => "cloud · pro",
        Plan::Agency => "cloud · agency",
        Plan::SelfHosted if user.account.is_owner => "self-hosted · owner",
        Plan::SelfHosted => "self-hosted",
    };
    UserView {
        email: email.clone(),
        initials: initials.to_uppercase(),
        plan_label: plan.to_owned(),
    }
}

/// Affected-page counts by check slug, from the stored summary.
fn check_count(summary: &codoseo_store::crawls::StoredSummary, id: CheckId) -> u32 {
    summary
        .counts
        .iter()
        .find(|(slug, _)| slug == id.slug())
        .map_or(0, |(_, n)| *n)
}

fn nav_groups(
    base: &str,
    screen: Screen,
    latest: Option<&Crawl>,
    summary: Option<&codoseo_store::crawls::StoredSummary>,
    (changes, critical_change): (i64, bool),
) -> Vec<NavGroup> {
    let n = |v: u32| {
        if summary.is_some() {
            fmt::thousands(v)
        } else {
            String::new()
        }
    };
    let critical_checks = summary.map_or(0, |s| {
        s.counts
            .iter()
            .filter_map(|(slug, _)| CheckId::from_slug(slug))
            .filter(|id| def(*id).severity == Severity::Critical)
            .count() as u32
    });
    let (pages, status, indexable) = summary.map_or((0, Default::default(), 0), |s| {
        (
            s.report_summary.pages,
            s.report_summary.status,
            s.report_summary.indexable,
        )
    });
    let title_issues = summary.map_or(0, |s| {
        [
            CheckId::TitleMissing,
            CheckId::TitleTooLong,
            CheckId::TitleTooShort,
            CheckId::TitleDuplicate,
        ]
        .iter()
        .map(|&c| check_count(s, c))
        .sum()
    });
    let alt = summary.map_or(0, |s| check_count(s, CheckId::ImagesMissingAlt));

    let item = |label, href: String, count: String, alert, active, keys, icon| NavItem {
        label,
        href,
        count,
        alert,
        active,
        keys,
        soon: false,
        icon,
    };
    let explorer = |f: &str| format!("{base}/explorer?filter={f}");
    vec![
        NavGroup {
            title: "PROJECT",
            items: vec![
                item(
                    "URL explorer",
                    format!("{base}/explorer"),
                    n(pages),
                    false,
                    screen == Screen::Explorer,
                    "G E",
                    "i-explorer",
                ),
                item(
                    "Site audit",
                    format!("{base}/audit"),
                    if critical_checks > 0 {
                        n(critical_checks)
                    } else {
                        String::new()
                    },
                    critical_checks > 0,
                    screen == Screen::Audit,
                    "G A",
                    "i-audit",
                ),
                item(
                    "Changes",
                    format!("{base}/changes"),
                    if changes > 0 {
                        fmt::thousands(changes)
                    } else {
                        String::new()
                    },
                    critical_change,
                    screen == Screen::Changes,
                    "G C",
                    "i-changes",
                ),
                item(
                    "Crawls",
                    format!("{base}/crawls"),
                    latest.map(|c| fmt::thousands(c.number)).unwrap_or_default(),
                    false,
                    screen == Screen::Crawls,
                    "G H",
                    "i-crawls",
                ),
            ],
        },
        NavGroup {
            title: "REPORTS",
            items: vec![
                item(
                    "Response codes",
                    explorer("s4"),
                    n(status.client_error + status.server_error),
                    false,
                    false,
                    "",
                    "i-codes",
                ),
                item(
                    "Redirects",
                    explorer("s3"),
                    n(status.redirect),
                    false,
                    false,
                    "",
                    "i-redirect",
                ),
                item(
                    "Indexability",
                    explorer("nx"),
                    n(pages.saturating_sub(indexable)),
                    false,
                    false,
                    "",
                    "i-index",
                ),
                item(
                    "Page titles",
                    explorer("check:title_too_long"),
                    n(title_issues),
                    false,
                    false,
                    "",
                    "i-title",
                ),
                item(
                    "Image alt text",
                    explorer("check:images_missing_alt"),
                    n(alt),
                    false,
                    false,
                    "",
                    "i-image",
                ),
            ],
        },
    ]
}

fn crawler_view(
    base: &str,
    latest: Option<&Crawl>,
    active: Option<&Crawl>,
    plan: Plan,
) -> CrawlerView {
    let poll_url = Some(format!("{base}/status"));
    if let Some(a) = active {
        if a.status == CrawlStatus::Running {
            let p = a.progress();
            let done = p.map_or(0, |p| p.pages_done);
            let budget = PlanLimits::for_plan(plan)
                .max_pages
                .unwrap_or(10_000)
                .max(1);
            let pct = ((u64::from(done) * 100) / u64::from(budget)).min(99) as u8;
            return CrawlerView {
                state: "running",
                label: format!("CRAWLING #{}", a.number),
                rows: vec![
                    ("pages".to_owned(), fmt::thousands(done)),
                    (
                        "elapsed".to_owned(),
                        p.map(|p| fmt::millis(p.elapsed_ms))
                            .unwrap_or_else(|| "—".to_owned()),
                    ),
                ],
                pct: Some(pct),
                poll_url,
            };
        }
        return CrawlerView {
            state: "queued",
            label: format!("QUEUED #{}", a.number),
            rows: vec![("queued".to_owned(), fmt::ago(a.queued_at))],
            pct: None,
            poll_url,
        };
    }
    match latest {
        Some(c) => CrawlerView {
            state: "idle",
            label: "CRAWLER IDLE".to_owned(),
            rows: vec![
                (
                    "last crawl".to_owned(),
                    c.finished_at.map(fmt::ago).unwrap_or_default(),
                ),
                (
                    "health".to_owned(),
                    c.health_score
                        .map(|s| format!("{s}/100"))
                        .unwrap_or_else(|| "—".to_owned()),
                ),
            ],
            pct: None,
            poll_url: None,
        },
        None => CrawlerView {
            state: "idle",
            label: "NO CRAWLS YET".to_owned(),
            rows: vec![("next".to_owned(), "run your first crawl".to_owned())],
            pct: None,
            poll_url: None,
        },
    }
}
