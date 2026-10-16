//! The cloud landing page at `/`: the marketing page, with one URL box that starts a no-signup
//! audit. Its numbers (checks per category, the plan limits, the size of the quick audit) come
//! from the code, so the page can't promise something the app doesn't enforce. Its "pages
//! crawled" counter is the stored running total (`codoseo_store::stats`), cached for a few
//! seconds per process and refreshed by `home.js` from `/stats/pages-crawled`.

use askama::Template;
use axum::Json;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use codoseo_checks::{BASE_CHECKS, CHECKS, Category};
use codoseo_core::plan::{ManualAllowance, Plan, PlanLimits, Schedule};

use crate::config::UmamiConfig;
use crate::error::AppError;
use crate::fmt;
use crate::render::html;
use crate::routes::quick::require_cloud;
use crate::state::AppState;

#[derive(Template)]
#[template(path = "landing/index.html")]
pub struct Landing {
    /// What the visitor typed, kept when the address is refused.
    pub url: String,
    pub error: Option<String>,
    pub checks: usize,
    pub base: String,
    /// The origin without its scheme (`codoseo.com`), where the page names the hosted service.
    pub host: String,
    /// Set when Turnstile is configured: the widget and its script are shown.
    pub turnstile_site_key: Option<String>,
    pub umami: Option<UmamiConfig>,
    pub categories: Vec<CategoryView>,
    /// Pages in the no-signup audit.
    pub quick_pages: u32,
    pub free: Vec<String>,
    pub lite: Vec<String>,
    pub pro: Vec<String>,
    pub lite_price: u32,
    pub pro_price: u32,
    /// Dodo checkout is configured: the paid cards lead to `/billing` (through sign-in).
    pub billing: bool,
    /// Every page crawled so far, `1,284,302`; `None` while the totals can't be read.
    pub pages_crawled: Option<String>,
    /// The hosted vs self-hosted cost comparison under the pricing cards.
    pub selfhost: SelfHostView,
}

pub struct CategoryView {
    pub name: &'static str,
    pub count: usize,
    pub highlights: &'static [Highlight],
}

/// A few of a category's checks in plain words, coloured by their worst severity.
pub struct Highlight {
    pub label: &'static str,
    /// `r` critical, `a` warning, `n0` notice: the dot colour in the stylesheet.
    pub severity: &'static str,
}

const fn h(label: &'static str, severity: &'static str) -> Highlight {
    Highlight { label, severity }
}

const CATEGORIES: [(Category, &str, &[Highlight]); 9] = [
    (
        Category::Response,
        "Response",
        &[
            h("4xx and 5xx errors", "r"),
            h("Fetch failures", "r"),
            h("Redirect loops", "r"),
            h("Redirect chains", "a"),
        ],
    ),
    (
        Category::Indexability,
        "Indexability",
        &[
            h("robots.txt blocking the site", "r"),
            h("Pages blocked by robots.txt", "a"),
            h("Canonical to non-200", "a"),
            h("Noindex and missing canonicals", "n0"),
        ],
    ),
    (
        Category::OnPage,
        "On-page",
        &[
            h("Missing or duplicate titles", "a"),
            h("Missing or duplicate descriptions", "a"),
            h("Missing H1s", "a"),
            h("Length violations", "n0"),
        ],
    ),
    (
        Category::Content,
        "Content",
        &[
            h("Duplicate content", "a"),
            h("Thin pages under 200 words", "n0"),
            h("Missing image alt text", "n0"),
        ],
    ),
    (
        Category::Links,
        "Links",
        &[
            h("Broken links", "a"),
            h("Orphan pages", "a"),
            h("Links to redirects", "n0"),
            h("Deep pages", "n0"),
        ],
    ),
    (
        Category::Technical,
        "Technical",
        &[
            h("HTTP instead of HTTPS", "a"),
            h("Mixed content", "a"),
            h("Slow responses", "n0"),
        ],
    ),
    (
        Category::Sitemap,
        "Sitemap",
        &[
            h("Sitemap pages not 200", "a"),
            h("Sitemap pages that are noindex", "a"),
            h("Pages missing from the sitemap", "n0"),
        ],
    ),
    (
        Category::Social,
        "Social",
        &[h("Missing Open Graph title or image", "n0")],
    ),
    (
        Category::Schema,
        "Schema",
        &[
            h("Invalid JSON-LD", "a"),
            h("hreflang missing self-reference", "n0"),
        ],
    ),
];

fn categories() -> Vec<CategoryView> {
    CATEGORIES
        .iter()
        .map(|&(cat, name, highlights)| CategoryView {
            name,
            count: CHECKS.iter().filter(|c| c.category == cat).count(),
            highlights,
        })
        .collect()
}

fn plural(n: u32, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The monthly price of a cloud plan in US dollars, as the pricing cards and `/billing` show
/// it; `None` for self-hosted. Dodo charges what its product says, so keep the two in step.
pub fn monthly_price(plan: Plan) -> Option<u32> {
    match plan {
        Plan::Free => Some(0),
        Plan::Lite => Some(9),
        Plan::Pro => Some(19),
        Plan::SelfHosted => None,
    }
}

/// `10 minutes`, `1 hour`, `2 hours`: a crawl's time limit in whole minutes or hours.
fn crawl_time(d: std::time::Duration) -> String {
    let mins = u32::try_from(d.as_secs() / 60).unwrap_or(u32::MAX);
    if mins % 60 == 0 {
        plural(mins / 60, "hour", "hours")
    } else {
        plural(mins, "minute", "minutes")
    }
}

/// What a plan includes, in the words the pricing cards use.
pub fn plan_features(plan: Plan) -> Vec<String> {
    let l = PlanLimits::for_plan(plan);
    let mut out = vec![match l.max_sites {
        Some(n) => plural(n, "monitored site", "monitored sites"),
        None => "Unlimited sites".to_owned(),
    }];
    if let Some(n) = l.max_pages {
        out.push(format!("Up to {} pages per crawl", fmt::thousands(n)));
    }
    if let Some(d) = l.max_duration {
        out.push(format!("Crawls of up to {}", crawl_time(d)));
    }
    match l.fastest_schedule {
        Some(Schedule::Weekly) => out.push("A scheduled crawl every week".to_owned()),
        Some(Schedule::Daily) => out.push("A scheduled crawl every day".to_owned()),
        None => {}
    }
    out.push(match l.manual_crawls {
        ManualAllowance::PerWeek(n) => {
            format!("{} a week", plural(n, "manual crawl", "manual crawls"))
        }
        ManualAllowance::PerDay(n) => {
            format!("{} a day", plural(n, "manual crawl", "manual crawls"))
        }
        // One crawl runs per site at a time on every plan.
        ManualAllowance::Unlimited => "Unlimited manual crawls, 1 running per site".to_owned(),
    });
    match l.history_days {
        Some(365) => out.push("A year of history".to_owned()),
        Some(d) => out.push(format!("{d} days of history")),
        None => {}
    }
    out.push(if l.email_alerts_only {
        "Email alerts".to_owned()
    } else {
        "Email, Slack, Discord and webhook alerts".to_owned()
    });
    out.push(match l.api_calls_per_day {
        Some(n) => format!("{} API calls a day", fmt::thousands(n)),
        None => "No API call limit".to_owned(),
    });
    out.push(match l.ai_rechecks_per_day {
        Some(n) => format!(
            "{} a day",
            plural(u32::from(n), "AI incident recheck", "AI incident rechecks")
        ),
        None => "Unlimited AI incident rechecks".to_owned(),
    });
    out
}

// ---- Hosted vs self-hosted ----
//
// What it costs to run CodoSEO yourself at the size of the Lite and Pro plans. Every price
// below was read from the linked page in the month `PRICES_AS_OF`; update the table, its
// sources and that date together.
//
// Sizing: the per-page data keeps only each site's latest 2 crawls (`codoseo_store::finalize`),
// so a Pro-sized install holds about 10 sites x 10,000 pages x 2 crawls = 200,000 page rows
// plus their inlinks, a few GB of Postgres at most, and a Lite-sized one a quarter of that.
// The worker reserves about 1.5 KB of memory per page of a running crawl. A 2 GB server runs
// the app and Postgres at Lite size; Pro size gets 4 GB so Postgres can cache its pages.
// Every plan listed has 40 GB of disk or more.

/// The month the self-host prices were checked.
pub const PRICES_AS_OF: &str = "2026-10";

/// Where a price comes from.
pub struct Source {
    pub label: &'static str,
    pub url: &'static str,
}

/// One VPS plan: its name, its size and its monthly price in US cents, before tax.
pub struct Tier {
    pub plan: &'static str,
    pub spec: &'static str,
    pub cents: u32,
}

/// A provider's cheapest plan for a Lite-sized install (2 GB or more) and a Pro-sized one
/// (4 GB or more).
pub struct VpsOffer {
    pub provider: &'static str,
    pub lite: Tier,
    pub pro: Tier,
    /// The catch, if any, said next to the price.
    pub note: &'static str,
    pub sources: &'static [Source],
}

/// A cost of self-hosting besides the server.
pub struct OtherCost {
    pub item: &'static str,
    pub cost: &'static str,
    pub sources: &'static [Source],
}

const fn src(label: &'static str, url: &'static str) -> Source {
    Source { label, url }
}

const fn tier(plan: &'static str, spec: &'static str, cents: u32) -> Tier {
    Tier { plan, spec, cents }
}

const HETZNER: Source = src(
    "Hetzner, price adjustment of 15 June 2026",
    "https://docs.hetzner.com/general/infrastructure-and-availability/price-adjustment/",
);
const OVH: Source = src("OVHcloud VPS", "https://us.ovhcloud.com/vps/");
const VULTR: Source = src("Vultr pricing", "https://www.vultr.com/pricing/");
const VULTR_BACKUPS: Source = src(
    "Vultr automatic backups",
    "https://docs.vultr.com/vps-automatic-backups",
);
const DIGITALOCEAN: Source = src(
    "DigitalOcean Droplet pricing",
    "https://www.digitalocean.com/pricing/droplets",
);
const AKAMAI: Source = src(
    "Akamai (Linode) shared CPU pricing",
    "https://www.akamai.com/cloud/pricing/cpu",
);
const AKAMAI_BACKUPS: Source = src(
    "Akamai (Linode) backups pricing",
    "https://www.akamai.com/cloud/pricing/backups",
);

pub const VPS_OFFERS: [VpsOffer; 5] = [
    VpsOffer {
        provider: "Hetzner",
        lite: tier("CX23", "2 vCPU, 4 GB, 40 GB", 649),
        pro: tier("CX23", "2 vCPU, 4 GB, 40 GB", 649),
        note: "EU only, plus IPv4; often out of stock (the regular CPX22 is $22.99)",
        sources: &[HETZNER],
    },
    VpsOffer {
        provider: "OVHcloud",
        lite: tier("VPS-1", "2 vCPU, 4 GB, 40 GB", 454),
        pro: tier("VPS-1", "2 vCPU, 4 GB, 40 GB", 454),
        note: "paid 12 months upfront; daily backup included",
        sources: &[OVH],
    },
    VpsOffer {
        provider: "Vultr",
        lite: tier("Regular 2 GB", "1 vCPU, 2 GB, 55 GB", 1000),
        pro: tier("Regular 4 GB", "2 vCPU, 4 GB, 80 GB", 2000),
        note: "",
        sources: &[VULTR],
    },
    VpsOffer {
        provider: "DigitalOcean",
        lite: tier("Basic 2 GB", "1 vCPU, 2 GB, 50 GB", 1200),
        pro: tier("Basic 4 GB", "2 vCPU, 4 GB, 80 GB", 2400),
        note: "",
        sources: &[DIGITALOCEAN],
    },
    VpsOffer {
        provider: "Akamai (Linode)",
        lite: tier("Linode 2 GB", "1 vCPU, 2 GB, 50 GB", 1200),
        pro: tier("Linode 4 GB", "2 vCPU, 4 GB, 80 GB", 2400),
        note: "",
        sources: &[AKAMAI],
    },
];

pub const OTHER_COSTS: [OtherCost; 5] = [
    OtherCost {
        item: "Backups",
        cost: "20% on top of the server at DigitalOcean and Vultr, $2.50 to $5 a month at \
               Akamai, a daily copy included at OVHcloud. Or pg_dump to storage you already have.",
        sources: &[DIGITALOCEAN, VULTR_BACKUPS, AKAMAI_BACKUPS],
    },
    OtherCost {
        item: "Bandwidth",
        cost: "$0. Crawling is mostly inbound traffic, and every plan above includes 2 TB or \
               more a month.",
        sources: &[],
    },
    OtherCost {
        item: "Domain and TLS",
        cost: "$0 on a subdomain you already own, or $10.46 a year for a new .com at \
               Cloudflare's at-cost price. TLS certificates are free from Let's Encrypt.",
        sources: &[
            src(
                "Cloudflare Registrar prices (cfdomainpricing.com)",
                "https://cfdomainpricing.com/",
            ),
            src("Let's Encrypt", "https://letsencrypt.org/"),
        ],
    },
    OtherCost {
        item: "Alert email",
        cost: "$0 on a free tier, for example Resend's 3,000 emails a month (100 a day).",
        sources: &[src("Resend pricing", "https://resend.com/pricing")],
    },
    OtherCost {
        item: "Your time",
        cost: "Our estimate: 2 to 3 hours to set up (server, Docker, DNS, TLS, SMTP), then \
               about 1 hour a month for image and OS updates, a backup check and watching \
               health and disk.",
        sources: &[],
    },
];

/// A source in the footnotes, numbered in order of first use.
pub struct Footnote {
    pub n: usize,
    pub label: &'static str,
    pub url: &'static str,
}

pub struct TierView {
    pub price: String,
    pub plan: &'static str,
    pub spec: &'static str,
}

pub struct VpsRow {
    pub provider: &'static str,
    pub lite: TierView,
    pub pro: TierView,
    pub note: &'static str,
    pub refs: Vec<usize>,
}

pub struct OtherRow {
    pub item: &'static str,
    pub cost: &'static str,
    pub refs: Vec<usize>,
}

pub struct SelfHostView {
    pub as_of: &'static str,
    /// `5 sites × 5,000 pages`, from the plan limits.
    pub lite_size: String,
    pub pro_size: String,
    /// `$4.54 to $12`: the cheapest and dearest server for each size.
    pub lite_range: String,
    pub pro_range: String,
    pub vps: Vec<VpsRow>,
    pub other: Vec<OtherRow>,
    pub notes: Vec<Footnote>,
}

/// `$9`, `$4.54`: whole dollars without cents.
fn dollars(cents: u32) -> String {
    if cents.is_multiple_of(100) {
        format!("${}", cents / 100)
    } else {
        format!("${}.{:02}", cents / 100, cents % 100)
    }
}

fn plan_size(plan: Plan) -> String {
    let l = PlanLimits::for_plan(plan);
    format!(
        "{} × {} pages",
        plural(l.max_sites.unwrap_or(0), "site", "sites"),
        fmt::thousands(l.max_pages.unwrap_or(0))
    )
}

fn price_range(cents: impl Iterator<Item = u32> + Clone) -> String {
    let lo = cents.clone().min().unwrap_or(0);
    let hi = cents.max().unwrap_or(0);
    format!("{} to {}", dollars(lo), dollars(hi))
}

/// The comparison as the landing shows it, with each source numbered once.
pub fn selfhost_view() -> SelfHostView {
    let mut notes: Vec<Footnote> = Vec::new();
    let mut cite = |sources: &'static [Source]| -> Vec<usize> {
        sources
            .iter()
            .map(|s| match notes.iter().find(|f| f.url == s.url) {
                Some(f) => f.n,
                None => {
                    let n = notes.len() + 1;
                    notes.push(Footnote {
                        n,
                        label: s.label,
                        url: s.url,
                    });
                    n
                }
            })
            .collect()
    };
    let tier_view = |t: &Tier| TierView {
        price: dollars(t.cents),
        plan: t.plan,
        spec: t.spec,
    };
    let vps = VPS_OFFERS
        .iter()
        .map(|o| VpsRow {
            provider: o.provider,
            lite: tier_view(&o.lite),
            pro: tier_view(&o.pro),
            note: o.note,
            refs: cite(o.sources),
        })
        .collect();
    let other = OTHER_COSTS
        .iter()
        .map(|c| OtherRow {
            item: c.item,
            cost: c.cost,
            refs: cite(c.sources),
        })
        .collect();
    SelfHostView {
        as_of: PRICES_AS_OF,
        lite_size: plan_size(Plan::Lite),
        pro_size: plan_size(Plan::Pro),
        lite_range: price_range(VPS_OFFERS.iter().map(|o| o.lite.cents)),
        pro_range: price_range(VPS_OFFERS.iter().map(|o| o.pro.cents)),
        vps,
        other,
        notes,
    }
}

fn view(state: &AppState, url: &str, error: Option<String>, pages: Option<i64>) -> Landing {
    let base = state.config.origin();
    Landing {
        url: url.to_owned(),
        error,
        checks: BASE_CHECKS,
        host: base
            .split_once("://")
            .map_or(base.as_str(), |(_, host)| host)
            .to_owned(),
        base,
        turnstile_site_key: state.config.turnstile.as_ref().map(|t| t.site_key.clone()),
        umami: state.config.umami.clone(),
        categories: categories(),
        quick_pages: PlanLimits::quick_audit().max_pages.unwrap_or(100),
        free: plan_features(Plan::Free),
        lite: plan_features(Plan::Lite),
        pro: plan_features(Plan::Pro),
        lite_price: monthly_price(Plan::Lite).unwrap_or(0),
        pro_price: monthly_price(Plan::Pro).unwrap_or(0),
        billing: state.config.billing.is_some(),
        pages_crawled: pages.map(fmt::thousands),
        selfhost: selfhost_view(),
    }
}

/// The landing page for a signed-out visitor.
pub async fn page(state: &AppState) -> Result<Response, AppError> {
    let pages = state
        .crawl_stats
        .totals(&state.pool)
        .await
        .map(|t| t.pages_crawled);
    Ok(html(&view(state, "", None, pages))?.into_response())
}

/// `/stats/pages-crawled` (cloud only): the landing counter's live number, polled by `home.js`.
/// Served from the same cache as the page; only the page total, never the website count.
pub async fn pages_crawled(State(state): State<AppState>) -> Result<Response, AppError> {
    require_cloud(&state)?;
    let Some(totals) = state.crawl_stats.totals(&state.pool).await else {
        return Err(AppError::Unavailable);
    };
    Ok((
        [(header::CACHE_CONTROL, "public, max-age=10")],
        Json(serde_json::json!({
            "pages_crawled": totals.pages_crawled,
            "label": fmt::thousands(totals.pages_crawled),
        })),
    )
        .into_response())
}

/// The landing page again with a message under the box, for a submit we turned away.
pub fn refuse(
    state: &AppState,
    url: &str,
    status: StatusCode,
    message: String,
) -> Result<Response, AppError> {
    // The cached total, however old: a refused submit never waits on the database for it.
    let pages = state.crawl_stats.cached().map(|t| t.pages_crawled);
    Ok((
        status,
        html(&view(state, url.trim(), Some(message), pages))?,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_check_is_in_a_listed_category() {
        let listed: usize = categories().iter().map(|c| c.count).sum();
        assert_eq!(listed, BASE_CHECKS, "every check but the policy ones");
    }

    #[test]
    fn the_free_card_matches_the_free_plan() {
        assert_eq!(
            plan_features(Plan::Free),
            [
                "1 monitored site",
                "Up to 500 pages per crawl",
                "Crawls of up to 10 minutes",
                "A scheduled crawl every week",
                "1 manual crawl a week",
                "30 days of history",
                "Email alerts",
                "100 API calls a day",
                "3 AI incident rechecks a day",
            ]
        );
    }

    #[test]
    fn the_paid_cards_match_the_paid_plans() {
        assert_eq!(
            plan_features(Plan::Lite),
            [
                "5 monitored sites",
                "Up to 5,000 pages per crawl",
                "Crawls of up to 1 hour",
                "A scheduled crawl every day",
                "1 manual crawl a day",
                "A year of history",
                "Email, Slack, Discord and webhook alerts",
                "1,000 API calls a day",
                "20 AI incident rechecks a day",
            ]
        );
        assert_eq!(
            plan_features(Plan::Pro),
            [
                "10 monitored sites",
                "Up to 10,000 pages per crawl",
                "Crawls of up to 2 hours",
                "A scheduled crawl every day",
                "Unlimited manual crawls, 1 running per site",
                "A year of history",
                "Email, Slack, Discord and webhook alerts",
                "5,000 API calls a day",
                "60 AI incident rechecks a day",
            ]
        );
        assert_eq!(monthly_price(Plan::Free), Some(0));
        assert_eq!(monthly_price(Plan::Lite), Some(9));
        assert_eq!(monthly_price(Plan::Pro), Some(19));
        assert_eq!(monthly_price(Plan::SelfHosted), None);
    }

    #[test]
    fn the_self_host_comparison_reads_from_the_price_table() {
        let v = selfhost_view();
        assert_eq!(v.as_of, "2026-10");
        assert_eq!(v.lite_size, "5 sites × 5,000 pages");
        assert_eq!(v.pro_size, "10 sites × 10,000 pages");
        assert_eq!(v.lite_range, "$4.54 to $12");
        assert_eq!(v.pro_range, "$4.54 to $24");
        assert_eq!(v.vps.len(), VPS_OFFERS.len());
        assert_eq!(v.vps[2].lite.price, "$10");
        assert_eq!(v.other.len(), OTHER_COSTS.len());
        // Each source is numbered once, in order, and every priced row cites one.
        let urls: Vec<&str> = v.notes.iter().map(|f| f.url).collect();
        let mut unique = urls.clone();
        unique.dedup();
        assert_eq!(urls.len(), unique.len());
        assert!(v.notes.iter().enumerate().all(|(i, f)| f.n == i + 1));
        assert!(urls.iter().all(|u| u.starts_with("https://")));
        assert!(v.vps.iter().all(|r| !r.refs.is_empty()));
        assert_eq!(
            v.vps[3].refs,
            v.other[0].refs[..1],
            "DigitalOcean cited once"
        );
    }

    #[test]
    fn dollars_drop_whole_cents() {
        assert_eq!(dollars(900), "$9");
        assert_eq!(dollars(454), "$4.54");
        assert_eq!(dollars(1005), "$10.05");
    }

    #[test]
    fn the_self_hosted_card_has_no_plan_caps() {
        let f = plan_features(Plan::SelfHosted);
        assert_eq!(f[0], "Unlimited sites");
        assert!(f.contains(&"Up to 100,000 pages per crawl".to_owned()));
        assert!(f.contains(&"No API call limit".to_owned()));
        assert!(f.contains(&"Email, Slack, Discord and webhook alerts".to_owned()));
    }
}
