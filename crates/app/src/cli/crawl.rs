//! `codoseo crawl`: crawl, check, report.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::Args;
use codoseo_checks::{def, run_checks};
use codoseo_core::audit::{AUDIT_FORMAT_VERSION, Audit};
use codoseo_core::crawl::{AddressPolicy, CrawlConfig, CrawlLimits, Politeness, USER_AGENT};
use codoseo_core::output::{Progress, StopReason};
use codoseo_core::snapshot::Snapshot;
use codoseo_crawler::crawl::crawl;
use codoseo_geo::Intent;
use codoseo_geo::findings::findings;
use codoseo_geo::report::{build_report, important_urls};
use url::Url;

use super::output::write_crawl;
use super::{CrawlFormat, EXIT_FAIL_ON, EXIT_OK, EXIT_RUNTIME, FailOn, Outcome, open_output};
const MIN_RPS: u32 = 1;
const MAX_RPS: u32 = 50;

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
    /// Exit with 1 when a check at this severity or worse fails
    #[arg(long, value_enum)]
    fail_on: Option<FailOn>,
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
    // Opened first, so a bad path fails at once instead of after a long crawl.
    let mut w = open_output(args.output.as_ref())?;
    let progress = ProgressLine {
        enabled: std::io::stderr().is_terminal(),
    };
    let result = crawl(cfg.clone(), |p| progress.update(p)).await;
    progress.clear();
    let mut out = result?;

    let report = run_checks(&mut out);
    let duration_ms = out.duration_ms;
    // GEO stays out of the health score and `--fail-on`: it is a section of its own, judged under
    // the default intent because the CLI has nowhere to keep an owner's.
    let ai_access = {
        let important = important_urls(&out.pages, &out.origin, &Default::default());
        let access = build_report(&out, &important);
        let found = findings(&access, &Intent::default());
        serde_json::json!({ "report": access, "findings": found })
    };
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
    };

    write_crawl(&mut w, args.format, &audit)?;
    w.flush()?;

    // A crawl that could not run says so after its report, and that wins over --fail-on.
    if let Some(message) = stop_message(&audit.snapshot.stop) {
        eprintln!("crawl stopped: {}", super::clean(&message));
        return Ok(EXIT_RUNTIME);
    }
    let failed = args.fail_on.is_some_and(|threshold| {
        audit
            .report
            .counts
            .iter()
            .any(|(id, _)| threshold.reached_by(def(*id).severity))
    });
    Ok(if failed { EXIT_FAIL_ON } else { EXIT_OK })
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
