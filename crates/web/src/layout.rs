//! The app shell around every signed-in screen (v3 layout, warmbly chrome): sidebar with the
//! site switcher, crawler status and nav groups; header with breadcrumb, ⌘K and Run crawl.
//!
//! Every screen template extends `base.html` and has a `shell: Shell` field, built once per
//! request by [`Shell::load`].

use axum::extract::{Request, State};
use axum::http::{Method, header};
use axum::middleware::Next;
use axum::response::Response;
use codoseo_checks::def;
use codoseo_core::check::{CheckId, Severity};
use codoseo_core::plan::{Plan, PlanLimits};
use codoseo_store::crawls::{Crawl, CrawlStatus};
use codoseo_store::sites::Site;
use uuid::Uuid;

use crate::auth::{CurrentUser, session};
use crate::config::Mode;
use crate::error::AppError;
use crate::fmt;
use crate::state::AppState;

/// Which screen is showing, for the nav highlight and the breadcrumb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Explorer,
    Graph,
    Audit,
    Changes,
    AiAccess,
    Crawls,
    Policy,
    Sites,
    Account,
    Alerts,
    ApiKeys,
    Billing,
    Admin,
}

impl Screen {
    pub fn title(self) -> &'static str {
        match self {
            Screen::Explorer => "URL explorer",
            Screen::Graph => "Site graph",
            Screen::Audit => "Site audit",
            Screen::Changes => "Changes",
            Screen::AiAccess => "AI access",
            Screen::Crawls => "Crawls",
            Screen::Policy => "Site policy",
            Screen::Sites => "Sites",
            Screen::Account => "Account",
            Screen::Alerts => "Alerts",
            Screen::ApiKeys => "API keys",
            Screen::Billing => "Billing",
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

#[derive(Clone)]
pub struct SiteView {
    pub id: Uuid,
    pub domain: String,
    pub initial: String,
    /// The site's own icon, served by us, when its newest crawl found one.
    pub icon: Option<String>,
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
    /// A crawl is running: the count reads "Live" with a pulsing dot.
    pub live: bool,
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
    /// The mascot's mood (`partials/mascot.html`): `crawling` while a crawl runs, `rest` while
    /// one waits, then the latest crawl's health as `ok`, `warning` or `error`, and `idle`
    /// before the first crawl.
    pub mood: &'static str,
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
    /// The site in view, if the screen has one: the breadcrumb and Run crawl.
    pub site: Option<SiteView>,
    /// The sidebar's site: the one in view, or else the user's current site. `None` only
    /// when the user has no site yet.
    pub current: Option<SiteView>,
    pub nav: Vec<NavGroup>,
    pub crawler: Option<CrawlerView>,
    pub version: &'static str,
    pub can_add_site: bool,
    /// On the cloud, the account menu links the Terms, Privacy Policy and Refund Policy.
    pub legal: bool,
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

        // The sidebar is the same on every page: a screen without a site in view (settings,
        // billing, the site list) still shows the user's current site, the one they last
        // looked at or else the first, which is where `/` goes.
        let current = site.or_else(|| {
            user.last_site
                .and_then(|id| sites.iter().find(|s| s.id == id))
                .or(sites.first())
        });

        let mut current_view = None;
        let mut nav = Vec::new();
        let mut crawler = None;
        if let Some(cur) = current {
            let latest = codoseo_store::crawls::latest_done(pool, cur.id).await?;
            let active = codoseo_store::crawls::active(pool, cur.id).await?;
            let changes = match &latest {
                Some(c) => change_counts(state, c.id).await?,
                None => (0, false),
            };
            let ai_issues = codoseo_store::geo::open_counts(pool, cur.id).await?;
            let summary = latest.as_ref().and_then(Crawl::summary);
            let base = format!("/s/{}", cur.id);
            let icon = codoseo_store::sites::icon_version(pool, cur.id)
                .await?
                .map(|v| format!("/s/{}/icon?v={v}", cur.id));
            current_view = Some(SiteView {
                id: cur.id,
                domain: cur.domain.clone(),
                initial: initial(&cur.domain),
                icon,
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
            nav.push(site_group(
                &base,
                screen,
                latest.as_ref(),
                summary.as_ref(),
                changes,
                ai_issues,
                active
                    .as_ref()
                    .is_some_and(|c| c.status == codoseo_store::crawls::CrawlStatus::Running),
            ));
            crawler = Some(crawler_view(
                &base,
                latest.as_ref(),
                active.as_ref(),
                user.account.plan,
            ));
        }

        let mut workspace = vec![
            NavItem {
                count: fmt::thousands(sites.len() as i64),
                ..workspace_item(
                    "All sites",
                    "/sites",
                    screen == Screen::Sites,
                    "",
                    "i-globe",
                )
            },
            workspace_item(
                "Alerts",
                "/settings/alerts",
                screen == Screen::Alerts,
                "",
                "i-bell",
            ),
            workspace_item(
                "API keys",
                "/settings/api-keys",
                screen == Screen::ApiKeys,
                "",
                "i-key",
            ),
        ];
        // Billing exists on the cloud only; a free account is nudged toward the upgrade.
        if state.config.mode == Mode::Cloud {
            let mut billing = workspace_item(
                "Billing",
                "/billing",
                screen == Screen::Billing,
                "",
                "i-card",
            );
            if user.account.plan == Plan::Free {
                billing.count = "upgrade".to_owned();
            }
            workspace.push(billing);
        }
        workspace.push(workspace_item(
            "Settings",
            "/account",
            screen == Screen::Account,
            "G S",
            "i-settings",
        ));
        nav.push(NavGroup {
            title: "WORKSPACE",
            items: workspace,
        });

        let sites = sites
            .iter()
            .map(|s| SiteLink {
                domain: s.domain.clone(),
                initial: initial(&s.domain),
                href: format!("/s/{}/audit", s.id),
                active: current.is_some_and(|cur| cur.id == s.id),
            })
            .collect();

        Ok(Shell {
            screen,
            user: user_view(user),
            sites,
            site: site.and(current_view.clone()),
            current: current_view,
            nav,
            crawler,
            version: env!("CARGO_PKG_VERSION"),
            can_add_site,
            legal: state.config.mode == Mode::Cloud,
        })
    }
}

/// Remembers the site last in view, so screens without one keep it in the sidebar.
pub const SITE_COOKIE: &str = "codoseo_site";
const SITE_COOKIE_TTL_SECS: i64 = 365 * 24 * 60 * 60;

/// Middleware: a successful `GET /s/{site}/…` sets [`SITE_COOKIE`] when it changes. Only the
/// account's own sites answer 2xx there, so the cookie never names someone else's site.
pub async fn remember_site(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let seen = (req.method() == Method::GET)
        .then(|| site_in_path(req.uri().path()))
        .flatten()
        .filter(|id| {
            session::cookie(req.headers(), SITE_COOKIE).as_deref() != Some(&id.to_string())
        });
    let mut res = next.run(req).await;
    if let Some(id) = seen
        && res.status().is_success()
    {
        res.headers_mut().append(
            header::SET_COOKIE,
            session::set_cookie(
                SITE_COOKIE,
                &id.to_string(),
                SITE_COOKIE_TTL_SECS,
                state.config.secure_cookies(),
            ),
        );
    }
    res
}

/// The site id in a `/s/{site}/…` path.
fn site_in_path(path: &str) -> Option<Uuid> {
    let rest = path.strip_prefix("/s/")?;
    rest.split('/').next()?.parse().ok()
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
        live: false,
        active,
        keys,
        soon: false,
        icon,
    }
}

impl NavItem {
    /// While a crawl runs, Site audit says so instead of its count.
    fn crawling(self, crawling: bool) -> NavItem {
        if crawling {
            NavItem {
                count: "Live".to_owned(),
                alert: false,
                live: true,
                ..self
            }
        } else {
            self
        }
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
        Plan::Lite => "cloud · lite",
        Plan::Pro => "cloud · pro",
        Plan::SelfHosted if user.account.is_owner => "self-hosted · owner",
        Plan::SelfHosted => "self-hosted",
    };
    UserView {
        email: email.clone(),
        initials: initials.to_uppercase(),
        plan_label: plan.to_owned(),
    }
}

/// THIS SITE: the current site's screens, Site audit first. The old REPORTS links live on as
/// URL explorer filters.
fn site_group(
    base: &str,
    screen: Screen,
    latest: Option<&Crawl>,
    summary: Option<&codoseo_store::crawls::StoredSummary>,
    (changes, critical_change): (i64, bool),
    (ai_open, ai_critical): (i64, i64),
    crawling: bool,
) -> NavGroup {
    let critical_checks = summary.map_or(0, |s| {
        s.counts
            .iter()
            .filter_map(|(slug, _)| CheckId::from_slug(slug))
            .filter(|id| def(*id).severity == Severity::Critical)
            .count() as u32
    });
    let pages = summary.map(|s| fmt::thousands(s.report_summary.pages));

    let item = |label, href: String, count: String, alert, active, keys, icon| NavItem {
        label,
        href,
        count,
        alert,
        live: false,
        active,
        keys,
        soon: false,
        icon,
    };
    NavGroup {
        title: "THIS SITE",
        items: vec![
            item(
                "Site audit",
                format!("{base}/audit"),
                if critical_checks > 0 {
                    fmt::thousands(critical_checks)
                } else {
                    String::new()
                },
                critical_checks > 0,
                screen == Screen::Audit,
                "G A",
                "i-audit",
            )
            .crawling(crawling),
            item(
                "URL explorer",
                format!("{base}/explorer"),
                pages.unwrap_or_default(),
                false,
                screen == Screen::Explorer,
                "G E",
                "i-explorer",
            ),
            item(
                "Site graph",
                format!("{base}/graph"),
                String::new(),
                false,
                screen == Screen::Graph,
                "G G",
                "i-graph",
            ),
            item(
                "AI access",
                format!("{base}/ai-access"),
                if ai_open > 0 {
                    fmt::thousands(ai_open)
                } else {
                    String::new()
                },
                ai_critical > 0,
                screen == Screen::AiAccess,
                "G I",
                "i-ai",
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
            item(
                "Site policy",
                format!("{base}/policy"),
                String::new(),
                false,
                screen == Screen::Policy,
                "G P",
                "i-policy",
            ),
        ],
    }
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
                mood: "crawling",
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
            mood: "rest",
            label: format!("QUEUED #{}", a.number),
            rows: vec![("queued".to_owned(), fmt::ago(a.queued_at))],
            pct: None,
            poll_url,
        };
    }
    match latest {
        Some(c) => CrawlerView {
            state: "idle",
            mood: health_mood(c.health_score),
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
            mood: "idle",
            label: "NO CRAWLS YET".to_owned(),
            rows: vec![("next".to_owned(), "run your first crawl".to_owned())],
            pct: None,
            poll_url: None,
        },
    }
}

/// The mascot's mood for a finished crawl, banded like the audit's health swatch.
fn health_mood(score: Option<i16>) -> &'static str {
    match score {
        Some(80..) => "ok",
        Some(50..80) => "warning",
        Some(_) => "error",
        None => "idle",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mascot_reads_the_latest_health_score() {
        assert_eq!(health_mood(Some(100)), "ok");
        assert_eq!(health_mood(Some(80)), "ok");
        assert_eq!(health_mood(Some(79)), "warning");
        assert_eq!(health_mood(Some(50)), "warning");
        assert_eq!(health_mood(Some(49)), "error");
        assert_eq!(health_mood(None), "idle");
    }
}
