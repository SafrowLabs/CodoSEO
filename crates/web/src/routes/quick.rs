//! The no-signup audit (cloud only): `POST /audit` starts a 100-page quick crawl, `/audit/{id}`
//! is the public report that walks from waiting through running to a score and the top five
//! issues, and `POST /audit/{id}/unlock` emails a sign-in link that attaches the audited site to
//! a new account and queues its first full crawl.
//!
//! The report is keyed by the crawl id, so one cached report serves everyone who audits the
//! same domain within 24 hours. Only `quick` crawls are ever served here.

use askama::Template;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{AppendHeaders, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use codoseo_checks::def;
use codoseo_core::check::{CheckId, Severity};
use codoseo_core::output::StopReason;
use codoseo_core::plan::PlanLimits;
use codoseo_store::accounts::Account;
use codoseo_store::crawls::CrawlStatus;
use codoseo_store::events::{self, EventKind};
use codoseo_store::quick::{
    self, Audit, ClaimOutcome, LimitWindow, Limits, StartOutcome, StartRequest, UnlockSlot,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::sites::{FIRST_CRAWL_PRIORITY, check_public_target, parse_start_url, schedule_for};
use crate::abuse::{self, ClientIp};
use crate::auth::magic::{self, AuditLink, LinkOutcome};
use crate::auth::{email, session};
use crate::config::Mode;
use crate::error::AppError;
use crate::fmt;
use crate::render::{Hx, html, hx_redirect};
use crate::state::AppState;
use crate::turnstile::{self, Verdict};

pub const CLAIM_COOKIE: &str = "codoseo_audit";
/// Unclaimed audits live 7 days (spec section 6), and so does the cookie.
const CLAIM_TTL_SECS: i64 = 7 * 24 * 3600;
/// A visitor can audit several sites before unlocking one, so the cookie holds the claim tokens
/// of their last few audits, joined with `.` (tokens are URL-safe base64, which has no dot).
const MAX_CLAIM_TOKENS: usize = 5;
/// How many issues the preview shows; the rest are counted and locked.
const PREVIEW_ISSUES: usize = 5;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/audit", post(start))
        .route("/audit/{id}", get(report))
        .route("/audit/{id}/live", get(live))
        .route("/audit/{id}/unlock", post(unlock))
}

/// The no-signup audit, landing page, bot page and robots.txt exist on the cloud only.
pub fn require_cloud(state: &AppState) -> Result<(), AppError> {
    if state.config.mode == Mode::Cloud {
        Ok(())
    } else {
        Err(AppError::NotFound)
    }
}

/// The claim tokens in the visitor's cookie, oldest first.
fn claim_tokens(headers: &HeaderMap) -> Vec<String> {
    session::cookie(headers, CLAIM_COOKIE)
        .map(|v| {
            v.split('.')
                .filter(|t| !t.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The cookie value after adding `new`: the last [`MAX_CLAIM_TOKENS`] tokens.
fn with_claim_token(mut tokens: Vec<String>, new: &str) -> String {
    tokens.push(new.to_owned());
    let skip = tokens.len().saturating_sub(MAX_CLAIM_TOKENS);
    tokens[skip..].join(".")
}

pub fn clear_claim_cookie(state: &AppState) -> HeaderValue {
    session::set_cookie(CLAIM_COOKIE, "", 0, state.config.secure_cookies())
}

#[derive(Deserialize)]
pub struct StartForm {
    url: String,
    /// Added to the form by Cloudflare's Turnstile script.
    #[serde(rename = "cf-turnstile-response", default)]
    turnstile: Option<String>,
}

async fn start(
    State(state): State<AppState>,
    hx: Hx,
    headers: HeaderMap,
    ClientIp(ip): ClientIp,
    Form(form): Form<StartForm>,
) -> Result<Response, AppError> {
    require_cloud(&state)?;
    let target = parse_start_url(&form.url).and_then(|u| check_public_target(&u).map(|()| u));
    let url = match target {
        Ok(u) => u,
        Err(message) => {
            return super::landing::refuse(&state, &form.url, StatusCode::BAD_REQUEST, message);
        }
    };
    match turnstile::verify(&state, form.turnstile.as_deref(), ip).await {
        Verdict::Passed => {}
        Verdict::Failed => {
            return super::landing::refuse(
                &state,
                &form.url,
                StatusCode::FORBIDDEN,
                "We couldn't confirm you're a human. Reload the page and try again.".to_owned(),
            );
        }
        Verdict::Unavailable => {
            return super::landing::refuse(
                &state,
                &form.url,
                StatusCode::SERVICE_UNAVAILABLE,
                "Verification is unavailable right now. Please try again in a minute.".to_owned(),
            );
        }
    }
    let domain = url.host_str().unwrap_or_default().to_ascii_lowercase();
    // Today's hash is stored; yesterday's is also counted, since a limit window can span
    // midnight and the salt changes then.
    let today = time::OffsetDateTime::now_utc().date();
    let hash_for = |day: time::Date| ip.map(|ip| abuse::ip_hash(&state.config.secret_key, ip, day));
    let ip_hash = hash_for(today);
    let previous_ip_hash = today.previous_day().and_then(hash_for);

    let claim_token = session::random_token();
    let outcome = quick::start(
        &state.pool,
        &StartRequest {
            domain: &domain,
            start_url: url.as_str(),
            claim_hash: &session::hash(&claim_token),
            ip_hash: ip_hash.as_deref(),
            previous_ip_hash: previous_ip_hash.as_deref(),
            limits: Limits::DEFAULT,
        },
    )
    .await?;
    let (crawl_id, how, fresh) = match outcome {
        StartOutcome::Started { crawl_id } => (crawl_id, "started", true),
        StartOutcome::Cached { crawl_id } => (crawl_id, "cached", false),
        StartOutcome::Joined { crawl_id } => (crawl_id, "joined", false),
        StartOutcome::Limited {
            window,
            retry_after_secs,
        } => {
            let (limit, per) = match window {
                LimitWindow::Hour => (Limits::DEFAULT.per_hour, "an hour"),
                LimitWindow::Day => (Limits::DEFAULT.per_day, "a day"),
            };
            let message = format!(
                "The limit is {limit} audits {per} for each visitor, and you've used them. \
                 Try again in {}, or sign in to monitor your own site.",
                abuse::wait_text(retry_after_secs)
            );
            let mut res =
                super::landing::refuse(&state, &form.url, StatusCode::TOO_MANY_REQUESTS, message)?;
            res.headers_mut().insert(
                header::RETRY_AFTER,
                HeaderValue::from(retry_after_secs.max(1)),
            );
            return Ok(res);
        }
    };
    events::record(
        &state.pool,
        EventKind::AuditStarted,
        None,
        None,
        Some(json!({ "crawl_id": crawl_id, "domain": domain, "outcome": how })),
    )
    .await?;

    let to = format!("/audit/{crawl_id}");
    // Only the visitor whose submit created the audit holds its claim token.
    let cookies: Vec<(HeaderName, HeaderValue)> = if fresh {
        vec![(
            header::SET_COOKIE,
            session::set_cookie(
                CLAIM_COOKIE,
                &with_claim_token(claim_tokens(&headers), &claim_token),
                CLAIM_TTL_SECS,
                state.config.secure_cookies(),
            ),
        )]
    } else {
        Vec::new()
    };
    Ok(if hx.request {
        (AppendHeaders(cookies), [hx_redirect(&to)]).into_response()
    } else {
        (AppendHeaders(cookies), Redirect::to(&to)).into_response()
    })
}

/// The audit for `raw_id`, or the 404 every unservable id gets.
async fn load(state: &AppState, raw_id: &str) -> Result<Audit, AppError> {
    require_cloud(state)?;
    let id = Uuid::parse_str(raw_id).map_err(|_| AppError::NotFound)?;
    quick::get(&state.pool, id).await?.ok_or(AppError::NotFound)
}

/// A line of the preview's issue list.
pub struct IssueLine {
    pub severity: &'static str,
    pub label: &'static str,
    pub title: &'static str,
    /// `12 pages · 4.8%`
    pub detail: String,
}

pub struct DoneView {
    pub score: i16,
    /// `c-ok`, `c-warn` or `c-err`.
    pub tone: &'static str,
    pub checks_chip: String,
    /// `1,284 pages crawled`
    pub pages_crawled: String,
    pub stop_chip: Option<String>,
    pub issues: Vec<IssueLine>,
    /// `3 more issues`, when there are more than the preview shows.
    pub locked: Option<String>,
    pub all_clear: bool,
    /// The counted link to RankOrg.
    pub rankorg: String,
}

/// A report that can't show a score, and why.
pub struct Notice {
    pub title: &'static str,
    pub message: String,
}

/// What `#audit-main` shows.
pub struct MainView {
    pub id: Uuid,
    pub domain: String,
    /// The page the audit started from, when it isn't the site's homepage: reports are shared
    /// per domain for 24 hours, so a visitor can see another start page's audit.
    pub start_url: Option<String>,
    /// Waiting or running: the block polls itself every 2 s.
    pub polling: bool,
    /// Waiting vs running, for the headline.
    pub running: bool,
    pub pages_done: u32,
    /// `You're #3 in line.`, while waiting behind other audits.
    pub line: Option<String>,
    pub done: Option<DoneView>,
    pub notice: Option<Notice>,
}

/// The unlock card's state.
pub struct UnlockCard {
    pub id: Uuid,
    pub email: String,
    pub error: Option<String>,
    pub sent: bool,
}

impl UnlockCard {
    pub fn new(id: Uuid) -> UnlockCard {
        UnlockCard {
            id,
            email: String::new(),
            error: None,
            sent: false,
        }
    }
}

#[derive(Template)]
#[template(path = "quick/page.html")]
pub struct ReportPage {
    pub main: MainView,
    pub unlock: UnlockCard,
}

#[derive(Template)]
#[template(path = "quick/main.html")]
pub struct MainPartial {
    pub main: MainView,
    pub unlock: UnlockCard,
}

#[derive(Template)]
#[template(path = "quick/unlock.html")]
pub struct UnlockPartial {
    pub unlock: UnlockCard,
}

/// The reader's place in the queue as a sentence. Position 1 is next.
fn line_text(position: Option<i64>) -> Option<String> {
    match position? {
        n if n <= 1 => Some("You're next in line.".to_owned()),
        n => Some(format!("You're #{n} in line.")),
    }
}

async fn main_view(state: &AppState, audit: &Audit) -> Result<MainView, AppError> {
    let mut view = build_main(audit);
    if view.polling && !view.running {
        view.line = line_text(quick::queue_position(&state.pool, audit.crawl.id).await?);
    }
    Ok(view)
}

fn build_main(audit: &Audit) -> MainView {
    let crawl = &audit.crawl;
    let mut view = MainView {
        id: crawl.id,
        domain: audit.domain.clone(),
        start_url: start_page(&audit.start_url, &audit.domain),
        polling: false,
        running: false,
        pages_done: 0,
        line: None,
        done: None,
        notice: None,
    };
    match crawl.status {
        CrawlStatus::Queued | CrawlStatus::Running => {
            view.polling = true;
            view.running = crawl.status == CrawlStatus::Running;
            view.pages_done = crawl.progress().map_or(0, |p| p.pages_done);
        }
        CrawlStatus::Failed => {
            view.notice = Some(failure_notice(crawl.failure_reason.as_deref()));
        }
        CrawlStatus::Done => match (crawl.summary(), crawl.health_score) {
            (Some(summary), Some(score)) if summary.report_summary.pages > 0 => {
                view.done = Some(done_view(
                    crawl.id,
                    score,
                    crawl.checks_passed.unwrap_or(0),
                    crawl.checks_total.unwrap_or(0),
                    &summary,
                ));
            }
            (Some(summary), _) if matches!(summary.stop_reason, StopReason::RobotsBlocked) => {
                view.notice = Some(Notice {
                    title: "This site's robots.txt blocks crawlers",
                    message: "Its robots.txt forbids crawling the whole site, so CodoSEObot \
                              stayed out, as it always does. If you own the site and want an \
                              audit, allow CodoSEObot in robots.txt and try again."
                        .to_owned(),
                });
            }
            _ => {
                view.notice = Some(Notice {
                    title: "We found nothing to audit",
                    message: "The crawl finished without finding any pages. Check the address \
                              and try again."
                        .to_owned(),
                });
            }
        },
    }
    view
}

/// The start URL when it is more than `https://{domain}/`.
fn start_page(start_url: &str, domain: &str) -> Option<String> {
    (start_url != format!("https://{domain}/")).then(|| start_url.to_owned())
}

fn failure_notice(reason: Option<&str>) -> Notice {
    let reason = reason.unwrap_or_default();
    if reason.starts_with("site blocked our crawler") {
        Notice {
            title: "This site blocked our crawler",
            message: "It answered our requests with errors or a challenge page, so we couldn't \
                      audit it. If you own the site, allow CodoSEObot (see the bot page) and \
                      try again."
                .to_owned(),
        }
    } else if reason.starts_with("site unreachable") {
        Notice {
            title: "We couldn't reach this site",
            message: "It didn't answer, or the address doesn't exist. Check the address and \
                      that the site is online, then try again."
                .to_owned(),
        }
    } else if reason.contains("address not allowed") {
        Notice {
            title: "That address can't be audited",
            message: "It is private or internal, so we can't audit it.".to_owned(),
        }
    } else {
        Notice {
            title: "Something went wrong",
            message: "An unexpected error happened on our side. Try again in a moment.".to_owned(),
        }
    }
}

fn done_view(
    crawl_id: Uuid,
    score: i16,
    passed: i16,
    total: i16,
    summary: &codoseo_store::crawls::StoredSummary,
) -> DoneView {
    let mut failing: Vec<(CheckId, u32)> = summary
        .counts
        .iter()
        .filter_map(|(slug, n)| Some((CheckId::from_slug(slug)?, *n)))
        .collect();
    failing.sort_by_key(|&(id, n)| (def(id).severity, std::cmp::Reverse(n), id));
    let pages = u64::from(summary.report_summary.pages).max(1);
    let issues = failing
        .iter()
        .take(PREVIEW_ISSUES)
        .map(|&(id, n)| {
            let d = def(id);
            let (severity, label) = match d.severity {
                Severity::Critical => ("critical", "Critical"),
                Severity::Warning => ("warning", "Warning"),
                Severity::Notice => ("notice", "Notice"),
            };
            IssueLine {
                severity,
                label,
                title: d.title,
                detail: format!(
                    "{} · {}",
                    fmt::pages(n),
                    fmt::percent(i64::from(n), pages as i64)
                ),
            }
        })
        .collect();
    let more = failing.len().saturating_sub(PREVIEW_ISSUES);
    let limits = PlanLimits::quick_audit();
    let stop_chip = match &summary.stop_reason {
        StopReason::PageLimit => Some(format!(
            "Stopped at {} pages",
            fmt::thousands(limits.max_pages.unwrap_or(100))
        )),
        StopReason::TimeLimit => Some(format!(
            "Stopped at the {} minute limit",
            limits.max_duration.map_or(2, |d| d.as_secs() / 60)
        )),
        _ => None,
    };
    DoneView {
        score,
        tone: match score {
            s if s >= 90 => "c-ok",
            s if s >= 70 => "c-warn",
            _ => "c-err",
        },
        checks_chip: format!("{passed} of {total} checks passed"),
        pages_crawled: format!("{} crawled", fmt::pages(summary.report_summary.pages)),
        stop_chip,
        issues,
        locked: (more > 0)
            .then(|| format!("{more} more issue{}", if more == 1 { "" } else { "s" })),
        all_clear: failing.is_empty(),
        rankorg: format!("/go/rankorg?src=audit&audit={crawl_id}"),
    }
}

/// Headers the public report always carries: it is for one visitor, not for search engines.
fn report_headers() -> [(HeaderName, &'static str); 2] {
    [
        (header::CACHE_CONTROL, "no-store"),
        (HeaderName::from_static("x-robots-tag"), "noindex, nofollow"),
    ]
}

async fn report(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let audit = load(&state, &id).await?;
    let page = ReportPage {
        main: main_view(&state, &audit).await?,
        unlock: UnlockCard::new(audit.crawl.id),
    };
    Ok((report_headers(), html(&page)?).into_response())
}

async fn live(State(state): State<AppState>, Path(id): Path<String>) -> Result<Response, AppError> {
    let audit = load(&state, &id).await?;
    let part = MainPartial {
        main: main_view(&state, &audit).await?,
        unlock: UnlockCard::new(audit.crawl.id),
    };
    Ok((report_headers(), html(&part)?).into_response())
}

#[derive(Deserialize)]
pub struct UnlockForm {
    email: String,
}

async fn unlock(
    State(state): State<AppState>,
    hx: Hx,
    Path(id): Path<String>,
    Form(form): Form<UnlockForm>,
) -> Result<Response, AppError> {
    let audit = load(&state, &id).await?;
    let mut card = UnlockCard::new(audit.crawl.id);
    card.email = form.email.trim().to_owned();

    let mut status = StatusCode::OK;
    match email::parse(&form.email) {
        None => card.error = Some("That doesn't look like an email address.".to_owned()),
        Some(address) if abuse::is_disposable(address) => {
            card.error = Some(
                "Please use a permanent email address. Throwaway inboxes can't keep your \
                 report or your alerts."
                    .to_owned(),
            );
        }
        Some(address) => {
            let address = address.to_owned();
            let outcome = magic::issue_link(
                &state,
                &address,
                "/",
                Some(AuditLink {
                    crawl_id: audit.crawl.id,
                    domain: &audit.domain,
                }),
            )
            .await?;
            match outcome {
                LinkOutcome::Sent => {
                    events::record(
                        &state.pool,
                        EventKind::EmailGiven,
                        None,
                        Some(audit.site_id),
                        Some(json!({ "crawl_id": audit.crawl.id })),
                    )
                    .await?;
                    card.sent = true;
                }
                LinkOutcome::Throttled(slot) => {
                    status = StatusCode::TOO_MANY_REQUESTS;
                    card.error = Some(
                        match slot {
                            UnlockSlot::AddressCapReached => {
                                "We've already sent several emails to that address recently. \
                                 Check your inbox and spam folder, or try again in an hour."
                            }
                            _ => {
                                "We already sent several links for this audit. Check your \
                                 inbox and spam folder, or try again in an hour."
                            }
                        }
                        .to_owned(),
                    );
                }
            }
            card.email = address;
        }
    }

    if hx.request {
        return Ok((status, html(&UnlockPartial { unlock: card })?).into_response());
    }
    let page = ReportPage {
        main: main_view(&state, &audit).await?,
        unlock: card,
    };
    Ok((status, report_headers(), html(&page)?).into_response())
}

/// After a sign-in link from an audit is used: gives the account the audited site (see
/// [`quick::claim`]) and returns where to send them.
pub async fn attach_after_login(
    state: &AppState,
    account: &Account,
    audit_id: Uuid,
    headers: &HeaderMap,
) -> Result<String, AppError> {
    let claim_hashes: Vec<Vec<u8>> = claim_tokens(headers)
        .iter()
        .map(|t| session::hash(t))
        .collect();
    let limits = PlanLimits::for_plan(account.plan);
    let outcome = quick::claim(
        &state.pool,
        account.id,
        audit_id,
        &claim_hashes,
        limits.max_sites.map(i64::from),
        FIRST_CRAWL_PRIORITY,
        schedule_for(account.plan),
    )
    .await?;
    let (how, site) = match outcome {
        ClaimOutcome::Attached(s) => ("attached", Some(s)),
        ClaimOutcome::Created(s) => ("created", Some(s)),
        ClaimOutcome::Existing(s) => ("existing", Some(s)),
        ClaimOutcome::LimitReached(s) => ("limit", s),
        ClaimOutcome::NotFound => ("expired", None),
    };
    events::record(
        &state.pool,
        EventKind::LinkClicked,
        Some(account.id),
        site.as_ref().map(|s| s.id),
        Some(json!({ "crawl_id": audit_id, "outcome": how })),
    )
    .await?;
    Ok(match site {
        Some(s) => format!("/s/{}/audit", s.id),
        None => "/".to_owned(),
    })
}
