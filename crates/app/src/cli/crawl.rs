//! `codoseo crawl`: crawl, check, report.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::Args;
use codoseo_core::audit::{AUDIT_FORMAT_VERSION, Audit};
use codoseo_core::crawl::{AddressPolicy, CrawlConfig, CrawlLimits, Politeness, USER_AGENT};
use codoseo_core::output::CrawlOutput;
use codoseo_core::output::{Progress, StopReason};
use codoseo_core::snapshot::Snapshot;
use codoseo_crawler::crawl::crawl;
use codoseo_crawler::probe::{ProbeConfig, probe_from_env, run_probes};
use codoseo_geo::probe::plan_probes;
use codoseo_geo::report::{AccessReport, build_report, important_urls};
use codoseo_geo::robots::RobotsTxt;
use codoseo_policy::SitePolicy;
use url::Url;

use super::gate::{Gate, judge, parse_gate, summary_lines};
use super::output::write_crawl;
use super::{CliError, CrawlFormat, EXIT_FAIL_ON, EXIT_OK, EXIT_RUNTIME, Outcome, open_output};
const MIN_RPS: u32 = 1;
const MAX_RPS: u32 = 50;
/// Pages `--probes` checks, and its budget and request cap: the self-hosted defaults.
const PROBE_URLS: usize = 5;
const PROBE_BUDGET: Duration = Duration::from_secs(180);
const PROBE_MAX_REQUESTS: u16 = 160;

#[derive(Debug, Args)]
pub struct CrawlArgs {
    /// The address to start from
    url: Url,
    /// Most pages to crawl
    #[arg(long, default_value_t = 500, value_parser = clap::value_parser!(u32).range(1..))]
    max_pages: u32,
    /// Longest crawl, in seconds
    #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u64).range(1..))]
    max_time: u64,
    /// Requests per second (1 to 50)
    #[arg(long, default_value_t = 5)]
    rps: u32,
    #[arg(long, value_enum, default_value_t = CrawlFormat::Table)]
    format: CrawlFormat,
    /// Write the report to this file instead of stdout
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Exit with 1 when one of these fails (comma-separated or repeated): critical or warning (SEO
    /// checks at that severity or worse); geo (robots.txt failing, wanted bots blocked, nosnippet
    /// or noarchive, empty app shells); geo:critical|warning|notice; or an AI access finding kind
    /// such as bots_blocked or answers_restricted:nosnippet
    #[arg(long, value_delimiter = ',', value_parser = parse_gate)]
    fail_on: Vec<Gate>,
    /// A saved audit (`--format json`) from an earlier run: --fail-on then counts only what is
    /// new or worse since it, and SARIF marks each result's baselineState
    #[arg(long, value_name = "AUDIT.json")]
    baseline: Option<PathBuf>,
    /// Also write the report to FILE, in the format its extension names (.json, .sarif, .md,
    /// .csv, .txt); repeatable
    #[arg(long, value_name = "FILE")]
    save: Vec<PathBuf>,
    /// After the crawl, fetch up to 5 important pages as each AI search and user-triggered bot
    /// and compare with CodoSEObot (a synthetic check; off by default)
    #[arg(long)]
    probes: bool,
    /// Judge AI access under this site policy file's AI rules (default: ./codoseo.toml when
    /// there is one); the audit JSON keeps the file's hash and the policy report
    #[arg(long, value_name = "FILE", conflicts_with = "no_policy")]
    policy: Option<PathBuf>,
    /// Ignore ./codoseo.toml: judge AI access under the default intent
    #[arg(long)]
    no_policy: bool,
}

impl CrawlArgs {
    fn config(&self) -> CrawlConfig {
        CrawlConfig {
            start_url: self.url.clone(),
            limits: CrawlLimits {
                max_pages: self.max_pages,
                max_duration: Duration::from_secs(self.max_time),
                ..CrawlLimits::default()
            },
            politeness: Politeness {
                requests_per_sec: self.rps.clamp(MIN_RPS, MAX_RPS) as f32,
                ..Politeness::default()
            },
            address_policy: AddressPolicy::AllowPrivate,
            user_agent: USER_AGENT.to_owned(),
            site_signals: true,
            extra_seeds: Vec::new(),
        }
    }
}

/// The `Crawled 120 pages · depth 3 · 4.1 s` line, drawn on stderr only when it is a
/// terminal. Output on stdout never gets progress.
struct ProgressLine {
    enabled: bool,
}

impl ProgressLine {
    fn update(&self, p: Progress) {
        if self.enabled {
            let mut err = std::io::stderr().lock();
            let _ = write!(
                err,
                "\rCrawled {} pages · depth {} · {:.1} s\x1b[K",
                p.pages_done,
                p.depth,
                p.elapsed_ms as f64 / 1000.0
            );
            let _ = err.flush();
        }
    }

    fn clear(&self) {
        if self.enabled {
            let mut err = std::io::stderr().lock();
            let _ = write!(err, "\r\x1b[K");
            let _ = err.flush();
        }
    }
}

pub async fn run(args: CrawlArgs) -> Outcome {
    let cfg = args.config();
    // Read and opened first, so a bad path fails at once instead of after a long crawl.
    let baseline = args
        .baseline
        .as_deref()
        .map(super::diff::load)
        .transpose()?;
    let policy = super::policy::resolve(args.policy.as_deref(), args.no_policy)?;
    if policy.is_none()
        && let Some(g) = args.fail_on.iter().find(|g| g.needs_policy())
    {
        return Err(CliError::msg(format!(
            "--fail-on {g} needs a policy file: add ./codoseo.toml (codoseo policy init) or pass --policy FILE"
        )));
    }
    let mut saves = Vec::new();
    for path in &args.save {
        let format = CrawlFormat::from_extension(path).ok_or_else(|| {
            CliError::msg(format!(
                "--save {}: name the format with the extension .json, .sarif, .md, .csv or .txt",
                path.display()
            ))
        })?;
        saves.push((format, open_output(Some(path))?));
    }
    let mut w = open_output(args.output.as_ref())?;
    let progress = ProgressLine {
        enabled: std::io::stderr().is_terminal(),
    };
    // The policy's must-work pages are fetched even when no link leads there.
    let mut run_cfg = cfg.clone();
    if let Some(p) = &policy {
        run_cfg.extra_seeds = p
            .policy
            .seo
            .must_work
            .iter()
            .filter_map(|path| cfg.start_url.join(path).ok())
            .collect();
    }
    let result = crawl(run_cfg, |p| progress.update(p)).await;
    progress.clear();
    let mut out = result?;

    // Under the policy file: its thresholds, its SEO rules' gaps as checks like any other and
    // its exceptions left out of the health score. Without one, the defaults.
    let default = SitePolicy::default();
    let judged = policy.as_ref().map_or(&default, |p| &p.policy);
    let report = codoseo_policy::run_checks_with(judged, &mut out, &Default::default(), &[]);
    let duration_ms = out.duration_ms;
    // GEO stays out of the health score: it is a section of its own, judged under the policy
    // file's AI rules, or the default intent without one. `--fail-on` may gate on it.
    let intent = policy
        .as_ref()
        .map(|p| p.policy.ai.intent.clone())
        .unwrap_or_default();
    let important = important_urls(&out.pages, &out.origin, &Default::default());
    let mut access = build_report(&out, &important);
    if args.probes && stop_message(&out.stop).is_none() {
        probe(&out, &mut access).await;
    }
    let ai_access = codoseo_geo::section_with(out.origin.as_str(), &access, &intent);
    let policy_section = policy.as_ref().map(|p| {
        let snapshot = Snapshot::from_output(&out);
        p.audit_section(&super::policy::report_for(
            &p.policy,
            &snapshot,
            &report,
            Some(&access),
        ))
    });
    let audit = Audit {
        format_version: AUDIT_FORMAT_VERSION,
        tool_version: env!("CARGO_PKG_VERSION").to_owned(),
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        duration_ms,
        start_url: cfg.start_url,
        report,
        snapshot: Snapshot::from_output_owned(out),
        ai_access: Some(ai_access),
        policy: policy_section,
    };

    let verdict = judge(&audit, baseline.as_ref(), &args.fail_on);
    write_crawl(&mut w, args.format, &audit, &verdict)?;
    w.flush()?;
    for (format, mut file) in saves {
        write_crawl(&mut file, format, &audit, &verdict)?;
        file.flush()?;
    }

    // A crawl that could not run says so after its report, and that wins over --fail-on, except
    // for a failing robots.txt a gate asked about: finding that is the gate's job.
    if let Some(message) = stop_message(&audit.snapshot.stop) {
        eprintln!("crawl stopped: {}", super::clean(&message));
        if !verdict.robots_tripped() {
            return Ok(EXIT_RUNTIME);
        }
    }
    for line in summary_lines(&verdict) {
        eprintln!("{line}");
    }
    Ok(if verdict.failed() {
        EXIT_FAIL_ON
    } else {
        EXIT_OK
    })
}

/// The synthetic fetch probes of up to [`PROBE_URLS`] important pages, kept in `access.probes`
/// and judged in the findings. A site robots.txt gives no verdicts for, or with no HTML 200 page
/// to probe, keeps the report without probes.
async fn probe(out: &CrawlOutput, access: &mut AccessReport) {
    let robots = out
        .robots
        .as_ref()
        .and_then(|r| RobotsTxt::from_response(Some(r.status), r.body.as_bytes()));
    if let Some(plan) = plan_probes(access, robots.as_ref(), PROBE_URLS) {
        // Like the crawl, a self-run command may probe private and internal addresses.
        let mut cfg = ProbeConfig::new(
            access,
            plan,
            AddressPolicy::AllowPrivate,
            PROBE_BUDGET,
            PROBE_MAX_REQUESTS,
        );
        cfg.from = probe_from_env();
        access.probes = Some(run_probes(cfg).await);
    }
}

/// Why the crawl never got going, for the stop reasons that mean it could not run.
fn stop_message(stop: &StopReason) -> Option<String> {
    match stop {
        StopReason::Unreachable(why) => Some(format!("site unreachable ({why})")),
        StopReason::Blocked(why) => Some(format!("crawler blocked ({why})")),
        StopReason::RobotsBlocked => Some("robots.txt blocks the whole site".to_owned()),
        StopReason::Completed | StopReason::PageLimit | StopReason::TimeLimit => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn config(args: &[&str]) -> CrawlConfig {
        #[derive(Parser)]
        struct Wrap {
            #[command(flatten)]
            args: CrawlArgs,
        }
        let mut argv = vec!["codoseo", "http://example.com/"];
        argv.extend_from_slice(args);
        Wrap::parse_from(argv).args.config()
    }

    #[test]
    fn rps_is_clamped_to_1_through_50() {
        assert_eq!(config(&["--rps", "0"]).politeness.requests_per_sec, 1.0);
        assert_eq!(config(&["--rps", "500"]).politeness.requests_per_sec, 50.0);
        assert_eq!(config(&["--rps", "7"]).politeness.requests_per_sec, 7.0);
        assert_eq!(config(&[]).politeness.requests_per_sec, 5.0);
    }

    #[test]
    fn limits_come_from_the_flags() {
        let cfg = config(&["--max-pages", "12", "--max-time", "30"]);
        assert_eq!(cfg.limits.max_pages, 12);
        assert_eq!(cfg.limits.max_duration, Duration::from_secs(30));
        assert_eq!(cfg.address_policy, AddressPolicy::AllowPrivate);
        assert_eq!(cfg.user_agent, USER_AGENT);
    }
}
