//! The `ai_recheck` job: runs one recheck of an AI access incident ([`run_recheck`]).
//!
//! A recheck fetches again only what its incident is about ([`codoseo_geo::recheck::plan`]), as
//! CodoSEObot and as politely as a crawl, from the origin the stored report's crawl read (after
//! the start URL's redirects, [`effective_site`]): robots.txt first (always, for the crawl delay and so a
//! page robots.txt keeps CodoSEObot off is never requested), then the planned pages, the planned
//! probes (the B1 probe executor, restricted to the incident's bots and pages, under the plan's
//! probe budget and request cap and only when the site and the install allow probes) or
//! `/llms.txt` without link checks. What it saw is laid over the newest stored report
//! ([`codoseo_geo::recheck::overlay`]) and applied to the incident
//! ([`codoseo_store::geo::apply_recheck`]).
//!
//! The whole run has a hard limit ([`RECHECK_TIMEOUT`]). Whatever stops it before the result is
//! applied (an error, a panic, the limit) marks the recheck `failed`, so it never stays
//! `running`, and the job itself still succeeds: it has one attempt, and a recheck is asked for
//! again rather than retried. A recheck whose site is being crawled is `superseded`: the crawl
//! will judge the incident.

use std::time::Duration;

use codoseo_core::crawl::{AddressPolicy, Politeness, RobotsFile};
use codoseo_core::output::{LlmsSkip, LlmsTxtFetch};
use codoseo_core::plan::PlanLimits;
use codoseo_crawler::probe::{MAX_OVERRUN, ProbeConfig, run_probes};
use codoseo_crawler::recheck::{RecheckFetch, RecheckFetched, recheck_fetch};
use codoseo_crawler::robots::{ROBOTS_AGENT, RobotsRules};
use codoseo_geo::findings::{Evidence, FindingKind};
use codoseo_geo::llms::state_of;
use codoseo_geo::probe::{ProbePlan, ProbeReport, candidates};
use codoseo_geo::recheck::{Observations, ProbeScope, RecheckPlan, overlay, plan};
use codoseo_geo::report::{AccessReport, agent_rules};
use codoseo_geo::robots::RobotsTxt;
use codoseo_store::geo::{self, Recheck, RecheckOutcome};
use codoseo_web::metrics;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::time::Instant;
use url::Url;

use crate::worker::ProbeSettings;
use crate::worker::run::{limits_for, resolve_plan};

/// The hard limit on one recheck, fetching and applying included. A recheck fetches robots.txt,
/// at most 20 pages, one file or a few probed pages, so two minutes is ample even at a 10 s
/// crawl delay; past it the recheck fails and the job ends.
pub const RECHECK_TIMEOUT: Duration = Duration::from_secs(120);
/// The part of [`RECHECK_TIMEOUT`] kept for applying the result: the fetches stop this long
/// before it.
const APPLY_MARGIN: Duration = Duration::from_secs(10);
/// Per-request time limit for robots.txt, pages and `/llms.txt`.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Longest text kept in the observations.
const MAX_TEXT: usize = 200;
/// Most probe results kept in the observations.
const MAX_PROBE_RESULTS: usize = 50;

/// How rechecks run.
#[derive(Debug, Clone)]
pub struct RecheckSettings {
    /// Addresses the fetches may reach (the cloud refuses private ones).
    pub policy: AddressPolicy,
    /// `From`, gap and timeout of the probes, and `CODOSEO_PROBE_URLS`.
    pub probes: ProbeSettings,
    /// The pacing of robots.txt, pages and `/llms.txt` (a crawl's).
    pub politeness: Politeness,
    pub request_timeout: Duration,
    /// The hard limit on one recheck; [`RECHECK_TIMEOUT`] outside tests.
    pub timeout: Duration,
}

impl RecheckSettings {
    /// The defaults for `policy`, probing with the default settings.
    pub fn new(policy: AddressPolicy) -> RecheckSettings {
        RecheckSettings {
            policy,
            probes: ProbeSettings::default(),
            politeness: Politeness::default(),
            request_timeout: REQUEST_TIMEOUT,
            timeout: RECHECK_TIMEOUT,
        }
    }

    /// [`RecheckSettings::new`] with the probe settings of the environment (`CODOSEO_PROBE_FROM`,
    /// `CODOSEO_PROBE_URLS`), as the crawl worker reads them.
    pub fn from_env(policy: AddressPolicy) -> RecheckSettings {
        RecheckSettings {
            probes: ProbeSettings::from_env(),
            ..RecheckSettings::new(policy)
        }
    }
}

/// Runs the recheck `id`. Only a queued recheck runs; any other (superseded, already run) is left
/// alone, so a job run twice does nothing the second time. Errors are database errors before
/// the recheck started: anything after marks it `failed` and returns `Ok`.
pub async fn run_recheck(pool: &PgPool, settings: &RecheckSettings, id: i64) -> Result<(), String> {
    let Some(recheck) = geo::start_recheck(pool, id)
        .await
        .map_err(|e| format!("could not start recheck {id}: {e}"))?
    else {
        tracing::info!(
            recheck = id,
            "the recheck is no longer queued; nothing to do"
        );
        return Ok(());
    };
    let span = tracing::info_span!(
        "recheck",
        recheck = id,
        site_id = %recheck.site_id,
        incident = recheck.incident_id,
    );
    let task = {
        let pool = pool.clone();
        let settings = settings.clone();
        tokio::spawn(tracing::Instrument::instrument(
            async move { execute(&pool, &settings, &recheck).await },
            span.clone(),
        ))
    };
    let abort = task.abort_handle();
    let result = match tokio::time::timeout(settings.timeout, task).await {
        Ok(Ok(result)) => result,
        Ok(Err(join)) if join.is_panic() => Err("the recheck panicked".to_owned()),
        Ok(Err(_)) => Err("the recheck was cancelled".to_owned()),
        Err(_) => {
            abort.abort();
            Err(format!(
                "the recheck did not finish within {} s",
                settings.timeout.as_secs()
            ))
        }
    };
    let outcome = match result {
        Ok(Ran::Applied(Some(outcome))) => outcome.slug(),
        // Finished meanwhile, or made moot (resolved, superseded by a crawl).
        Ok(Ran::Applied(None)) | Ok(Ran::Superseded) => "superseded",
        Err(error) => {
            tracing::warn!(parent: &span, %error, "recheck failed");
            if let Err(e) = geo::fail_recheck(pool, id).await {
                // The orphan sweep fails it later.
                tracing::error!(parent: &span, error = %e, "could not mark the recheck failed");
            }
            "failed"
        }
    };
    metrics::recheck_finished(outcome);
    tracing::info!(parent: &span, outcome, "recheck finished");
    Ok(())
}

enum Ran {
    /// `apply_recheck`'s answer.
    Applied(Option<RecheckOutcome>),
    /// A crawl of the site is under way.
    Superseded,
}

async fn execute(
    pool: &PgPool,
    settings: &RecheckSettings,
    recheck: &Recheck,
) -> Result<Ran, String> {
    let started = Instant::now();
    let fetch_until = started + settings.timeout.saturating_sub(APPLY_MARGIN);
    let db = |what: &'static str| move |e: sqlx::Error| format!("could not load {what}: {e}");

    let crawling: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM crawls \
         WHERE site_id = $1 AND status IN ('queued', 'running'))",
    )
    .bind(recheck.site_id)
    .fetch_one(pool)
    .await
    .map_err(db("the site's crawls"))?;
    if crawling {
        geo::supersede_recheck(pool, recheck.id)
            .await
            .map_err(|e| format!("could not supersede the recheck: {e}"))?;
        tracing::info!("a crawl of the site is under way; it will judge the incident");
        return Ok(Ran::Superseded);
    }

    let start_url: String = sqlx::query_scalar("SELECT start_url FROM sites WHERE id = $1")
        .bind(recheck.site_id)
        .fetch_one(pool)
        .await
        .map_err(db("the site"))?;
    let start = Url::parse(&start_url).map_err(|e| format!("bad start URL: {e}"))?;
    let incident = geo::incident(pool, recheck.site_id, recheck.incident_id)
        .await
        .map_err(db("the incident"))?
        .ok_or("the incident is gone")?;
    if incident.resolved_at.is_some() {
        // Resolved meanwhile: nothing to look at. `apply_recheck` would say the same.
        geo::supersede_recheck(pool, recheck.id)
            .await
            .map_err(|e| format!("could not supersede the recheck: {e}"))?;
        return Ok(Ran::Superseded);
    }
    let stored = geo::latest_report(pool, recheck.site_id)
        .await
        .map_err(db("the stored report"))?
        .ok_or("the site has no stored AI access report")?
        .report;
    let site = effective_site(&stored, start);
    let evidence: Evidence = serde_json::from_value(incident.evidence.clone())
        .map_err(|e| format!("the incident's evidence does not read: {e}"))?;
    let plan_name = resolve_plan(pool, recheck.site_id).await?;
    let limits = limits_for(plan_name, &json!({}));
    let probe_cap = settings.probes.probe_urls(plan_name, &limits);

    let mut plan = plan(
        incident.kind,
        &incident.subject,
        &evidence,
        &stored,
        probe_cap,
    );
    let mut notes: Vec<String> = Vec::new();
    if incident.kind == FindingKind::FetchBlocked {
        if probe_cap == 0 {
            notes.push("fetch checks are turned off on this install".to_owned());
        } else if !geo::get_ai_probes(pool, recheck.site_id)
            .await
            .map_err(db("the probe setting"))?
        {
            notes.push("fetch checks are turned off for this site".to_owned());
            plan.probe = None;
        } else if plan.probe.is_none() {
            notes.push("no probed page to check again".to_owned());
        }
    }

    // Only pages on the origin the crawl read; any other host has its own robots.txt.
    let pages: Vec<Url> = plan
        .pages
        .iter()
        .filter_map(|u| Url::parse(u).ok())
        .filter(|u| u.origin() == site.origin())
        .collect();
    let fetched = recheck_fetch(RecheckFetch {
        site: site.clone(),
        pages,
        llms: plan.llms,
        address_policy: settings.policy,
        politeness: settings.politeness.clone(),
        request_timeout: settings.request_timeout,
        deadline: fetch_until,
    })
    .await
    .map_err(|e| format!("could not fetch from the site: {e}"))?;
    metrics::recheck_requests(u64::from(fetched.requests));

    let probes = match &plan.probe {
        Some(scope) => match probe_config(
            &stored,
            scope,
            &fetched.robots,
            &limits,
            &settings.probes,
            settings.policy,
            fetch_until.saturating_duration_since(Instant::now()),
        ) {
            Ok(cfg) => {
                let report = run_probes(cfg).await;
                metrics::probe_requests(u64::from(report.requests));
                Some(report)
            }
            Err(why) => {
                notes.push(why.to_owned());
                None
            }
        },
        None => None,
    };

    let llms = match &fetched.llms {
        Some(Ok(f)) => Some(f.clone()),
        Some(Err(skip)) => {
            notes.push(llms_skip_note(*skip).to_owned());
            None
        }
        None => None,
    };
    let obs = Observations {
        robots: Some(fetched.robots.clone()),
        pages: fetched.pages.clone(),
        probes,
        llms,
    };
    let (report, kinds) = overlay(&stored, &obs, &plan);
    let observed = observations(&plan, &fetched, &obs, &report, &evidence, &kinds, &notes);
    let outcome = geo::apply_recheck(pool, recheck.id, &report, &kinds, Some(&observed))
        .await
        .map_err(|e| format!("could not apply the recheck: {e}"))?;
    Ok(Ran::Applied(outcome))
}

/// Where the crawl behind `stored` read the site: the origin of its first important page (the
/// home page, after the start URL's redirects, say from the apex to `www`), so robots.txt,
/// `/llms.txt` and the pages are read from the same host the report judged. The start URL when
/// the report names no page.
fn effective_site(stored: &AccessReport, start: Url) -> Url {
    stored
        .important
        .first()
        .and_then(|i| Url::parse(&i.url).ok())
        .filter(|u| matches!(u.scheme(), "http" | "https") && u.host().is_some())
        .and_then(|u| u.join("/").ok())
        .unwrap_or(start)
}

/// The probe run of a recheck: the scope's bots on the scope's pages, as the crawl's probes
/// would send them, under the plan's probe budget and request cap and within `left`. robots.txt
/// as just fetched decides: a page CodoSEObot may not fetch is not probed (the baseline is a
/// CodoSEObot request), and a bot is not sent where its token is disallowed. `Err` says why
/// nothing is probed.
fn probe_config(
    stored: &AccessReport,
    scope: &ProbeScope,
    robots: &RobotsFile,
    limits: &PlanLimits,
    settings: &ProbeSettings,
    policy: AddressPolicy,
    left: Duration,
) -> Result<ProbeConfig, &'static str> {
    let Some(txt) = RobotsTxt::from_response(Some(robots.status), robots.body.as_bytes()) else {
        return Err("robots.txt is unavailable, so no bot was sent");
    };
    let ours = RobotsRules::from_response(robots.status, robots.body.as_bytes(), ROBOTS_AGENT);
    let mut origin_urls: Vec<(u16, Url)> = Vec::new();
    for u in &scope.urls {
        let Some(i) = stored
            .important
            .iter()
            .position(|x| &x.url == u)
            .and_then(|i| u16::try_from(i).ok())
        else {
            continue;
        };
        let Ok(url) = Url::parse(u) else { continue };
        if ours.allowed(url.as_str()) {
            origin_urls.push((i, url));
        }
    }
    if origin_urls.is_empty() {
        return Err("robots.txt keeps CodoSEObot off the probed pages, so none was probed");
    }
    let (all, _) = candidates();
    let bots: Vec<_> = all
        .into_iter()
        .filter(|b| {
            scope
                .tokens
                .iter()
                .any(|t| t.eq_ignore_ascii_case(&b.token))
        })
        .collect();
    if bots.is_empty() {
        return Err("none of the incident's bots is probed any more");
    }
    let mut disallowed = Vec::new();
    for (i, url) in &origin_urls {
        let path = match url.query() {
            Some(q) => format!("{}?{q}", url.path()),
            None => url.path().to_owned(),
        };
        for b in &bots {
            if !txt.allowed_for(&agent_rules(&txt, &b.token), &path) {
                disallowed.push((*i, b.token.clone()));
            }
        }
    }
    let crawl_delay = std::iter::once(ROBOTS_AGENT)
        .chain(bots.iter().map(|b| b.token.as_str()))
        .filter_map(|t| agent_rules(&txt, t).crawl_delay())
        .max_by(f64::total_cmp);
    let budget = limits.probe_budget.min(left);
    if budget.is_zero() {
        return Err("the time ran out before the probes");
    }
    Ok(ProbeConfig {
        plan: ProbePlan {
            urls: origin_urls.iter().map(|(i, _)| *i).collect(),
            bots,
            skipped_bots: Vec::new(),
            crawl_delay,
        },
        origin_urls,
        disallowed,
        from: settings.from.clone(),
        address_policy: policy,
        budget,
        deadline: (budget + MAX_OVERRUN).min(left),
        max_requests: limits.probe_max_requests,
        min_gap: settings.min_gap,
        request_timeout: settings.request_timeout,
    })
}

fn llms_skip_note(skip: LlmsSkip) -> &'static str {
    match skip {
        LlmsSkip::RobotsDisallowed => {
            "robots.txt keeps CodoSEObot off /llms.txt, so it was not fetched"
        }
        LlmsSkip::Refused => "/llms.txt was not fetched: the address is refused",
        LlmsSkip::Deadline => "/llms.txt was not fetched: the time ran out",
    }
}

fn clip(s: &str) -> String {
    let mut chars = s.chars().filter(|c| !c.is_control());
    let mut out: String = chars.by_ref().take(MAX_TEXT).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

/// What the recheck saw, kept on its row: statuses, verdicts and the rules that matched, never a
/// body.
fn observations(
    plan: &RecheckPlan,
    fetched: &RecheckFetched,
    obs: &Observations,
    report: &AccessReport,
    evidence: &Evidence,
    kinds: &[FindingKind],
    notes: &[String],
) -> Value {
    let mut out = serde_json::Map::new();
    out.insert(
        "robots".into(),
        json!({ "status": fetched.robots.status, "bytes": fetched.robots.body.len() }),
    );
    if plan.robots {
        let bots: Vec<Value> = evidence
            .bots
            .iter()
            .filter_map(|b| report.bot(&b.token))
            .map(|b| {
                json!({
                    "token": clip(&b.token),
                    "home_allowed": b.home_allowed,
                    "urls_blocked": b.blocked.len(),
                    "home_rule": b.home_rule.as_ref().map(|r| json!({
                        "line": r.line,
                        "allow": r.allow,
                        "pattern": clip(&r.pattern),
                    })),
                })
            })
            .collect();
        out.insert("bots".into(), Value::Array(bots));
    }
    if !plan.pages.is_empty() {
        let pages: Vec<Value> = fetched
            .pages
            .iter()
            .map(|p| {
                json!({
                    "url": clip(p.url.as_str()),
                    "status": p.status,
                    "html_ok": p.is_html_ok(),
                    "error": p.error.as_ref().and_then(|e| serde_json::to_value(e).ok()),
                })
            })
            .collect();
        out.insert("pages".into(), Value::Array(pages));
        let skipped = |urls: &[Url]| -> Value {
            urls.iter()
                .map(|u| Value::String(clip(u.as_str())))
                .collect()
        };
        if !fetched.robots_skipped.is_empty() {
            out.insert("robots_skipped".into(), skipped(&fetched.robots_skipped));
        }
        if !fetched.not_reached.is_empty() {
            out.insert("not_reached".into(), skipped(&fetched.not_reached));
        }
    }
    if let Some(p) = &obs.probes {
        out.insert("probes".into(), probes_seen(p, report));
    }
    if let Some(f) = &obs.llms {
        out.insert("llms".into(), llms_seen(f));
    }
    out.insert("requests".into(), json!(fetched.requests));
    out.insert(
        "conclusive".into(),
        kinds
            .iter()
            .map(|k| Value::String(k.slug().to_owned()))
            .collect(),
    );
    if !notes.is_empty() {
        out.insert(
            "notes".into(),
            notes.iter().map(|n| Value::String(clip(n))).collect(),
        );
    }
    Value::Object(out)
}

fn probes_seen(p: &ProbeReport, report: &AccessReport) -> Value {
    let results: Vec<Value> = p
        .urls
        .iter()
        .flat_map(|u| {
            let url = report
                .important
                .get(usize::from(u.url))
                .map_or_else(String::new, |i| clip(&i.url));
            u.results.iter().map(move |r| {
                json!({
                    "url": url,
                    "token": clip(&r.token),
                    "status": r.fetch.as_ref().and_then(|f| f.status),
                    "verdict": serde_json::to_value(&r.verdict).unwrap_or(Value::Null),
                })
            })
        })
        .take(MAX_PROBE_RESULTS)
        .collect();
    let baselines: Vec<Value> = p
        .urls
        .iter()
        .map(|u| json!({ "url": u.url, "status": u.baseline.status }))
        .collect();
    json!({
        "requests": p.requests,
        "stop": p.stop,
        "baselines": baselines,
        "results": results,
    })
}

fn llms_seen(f: &LlmsTxtFetch) -> Value {
    json!({
        "status": f.status,
        "error": f.error.as_deref().map(clip),
        "content_type": f.content_type.as_deref().map(clip),
        "bytes": f.bytes,
        "state": serde_json::to_value(state_of(f)).unwrap_or(Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_clipped_on_a_character_boundary_without_control_characters() {
        assert_eq!(clip("a\nb"), "ab");
        let long = "é".repeat(MAX_TEXT + 5);
        let c = clip(&long);
        assert_eq!(c.chars().count(), MAX_TEXT + 1);
        assert!(c.ends_with('…'));
    }

    #[test]
    fn the_fetches_leave_room_to_apply_the_result() {
        let s = RecheckSettings::new(AddressPolicy::Public);
        assert_eq!(s.timeout, RECHECK_TIMEOUT);
        assert!(RECHECK_TIMEOUT > APPLY_MARGIN + REQUEST_TIMEOUT);
    }
}
