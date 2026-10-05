//! Logs and metrics: the log format picks, a JSON line parses, every required metric shows up
//! on the internal listener with low-cardinality labels, and the public app has no /metrics.

use std::io::Write;
use std::sync::{Arc, Mutex};

use codoseo::telemetry::{self, LogFormat};
use codoseo_web::metrics as m;
use codoseo_web::metrics::{Surface, Tier};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

/// A writer that keeps what a subscriber prints.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Capture {
        self.clone()
    }
}

impl Capture {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

#[test]
fn the_log_format_is_json_in_the_cloud_and_text_elsewhere_unless_overridden() {
    assert_eq!(telemetry::log_format(None, None), Ok(LogFormat::Text));
    assert_eq!(
        telemetry::log_format(Some("selfhost"), None),
        Ok(LogFormat::Text)
    );
    assert_eq!(
        telemetry::log_format(Some("cloud"), None),
        Ok(LogFormat::Json)
    );
    assert_eq!(
        telemetry::log_format(Some("cloud"), Some("text")),
        Ok(LogFormat::Text)
    );
    assert_eq!(
        telemetry::log_format(Some("selfhost"), Some("JSON")),
        Ok(LogFormat::Json)
    );
    assert_eq!(
        telemetry::log_format(None, Some(" ")),
        Ok(LogFormat::Text),
        "a blank override is no override"
    );
    assert!(telemetry::log_format(None, Some("xml")).is_err());
}

#[test]
fn a_json_log_line_parses_and_carries_the_span_fields() {
    let out = Capture::default();
    let subscriber = telemetry::subscriber(LogFormat::Json, EnvFilter::new("info"), out.clone());
    tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!("crawl", crawl_id = "c-1", site_id = "s-1");
        let _in = span.enter();
        tracing::info!(pages = 3, "crawl finished");
    });
    let text = out.text();
    let line = text.lines().next().expect("one line");
    let v: serde_json::Value = serde_json::from_str(line).expect("the line is JSON");
    assert_eq!(v["level"], "INFO");
    assert_eq!(v["fields"]["message"], "crawl finished");
    assert_eq!(v["fields"]["pages"], 3);
    assert_eq!(v["span"]["crawl_id"], "c-1");
    assert_eq!(v["span"]["site_id"], "s-1");
}

#[test]
fn a_text_log_line_is_plain_and_the_default_filter_quiets_sqlx() {
    let out = Capture::default();
    let subscriber =
        telemetry::subscriber(LogFormat::Text, telemetry::default_filter(), out.clone());
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(target: "sqlx::query", "noisy statement");
        tracing::info!("worker started");
    });
    let text = out.text();
    assert!(text.contains("worker started"), "{text}");
    assert!(!text.contains("noisy statement"), "{text}");
    assert!(
        !text.contains('\u{1b}'),
        "no colour codes in a pipe: {text}"
    );
}

/// Compose passes `RUST_LOG: ${RUST_LOG:-}`, so an unset host variable arrives as an empty
/// string; that must mean the default filter, not "no directives" (errors only).
#[test]
fn a_blank_rust_log_means_the_default_filter() {
    for value in [None, Some(""), Some("   ")] {
        let out = Capture::default();
        let subscriber =
            telemetry::subscriber(LogFormat::Text, telemetry::env_filter(value), out.clone());
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "sqlx::query", "noisy statement");
            tracing::info!("worker started");
        });
        let text = out.text();
        assert!(text.contains("worker started"), "{value:?}: {text}");
        assert!(!text.contains("noisy statement"), "{value:?}: {text}");
    }
    let out = Capture::default();
    let subscriber = telemetry::subscriber(
        LogFormat::Text,
        telemetry::env_filter(Some("warn")),
        out.clone(),
    );
    tracing::subscriber::with_default(subscriber, || tracing::info!("hidden"));
    assert!(!out.text().contains("hidden"));
}

/// Records every metric once through the helpers, into a recorder of its own.
fn render_all() -> String {
    let recorder = telemetry::build_recorder();
    let handle = recorder.handle();
    ::metrics::with_local_recorder(&recorder, || {
        m::queue_wait(2, 12.5);
        {
            let _running = m::crawl_started(1536 * 500);
        }
        m::crawl_finished(true);
        m::crawl_finished(false);
        m::pages_crawled(42);
        m::worker_memory_budget(1_000_000_000);
        m::db_pool(3, 2);
        m::alert_delivery("slack", true);
        m::alert_delivery("webhook", false);
        m::api_request(Surface::Rest, Tier::Key, "ok");
        m::api_request(Surface::Mcp, Tier::Anon, "quota_exceeded");
        m::scheduler_tick(1_700_000_000.0);
        m::job_failed("send_email");
    });
    handle.render()
}

#[test]
fn every_required_metric_is_rendered_with_its_labels() {
    let body = render_all();
    for name in m::ALL {
        assert!(body.contains(name), "{name} is missing from:\n{body}");
    }
    for expected in [
        r#"codoseo_crawl_queue_wait_seconds_bucket{lane="2""#,
        r#"codoseo_crawls_finished_total{outcome="completed"} 1"#,
        r#"codoseo_crawls_finished_total{outcome="failed"} 1"#,
        "codoseo_pages_crawled_total 42",
        "codoseo_worker_memory_budget_bytes 1000000000",
        "codoseo_crawls_running 0",
        "codoseo_worker_memory_reserved_bytes 0",
        r#"codoseo_db_pool_connections{state="idle"} 3"#,
        r#"codoseo_db_pool_connections{state="active"} 2"#,
        r#"codoseo_alert_deliveries_total{channel="slack",result="ok"} 1"#,
        r#"codoseo_alert_deliveries_total{channel="webhook",result="error"} 1"#,
        r#"codoseo_api_requests_total{surface="rest",tier="key",result="ok"} 1"#,
        r#"codoseo_api_requests_total{surface="mcp",tier="anon",result="quota_exceeded"} 1"#,
        "codoseo_scheduler_last_tick_timestamp_seconds 1700000000",
        r#"codoseo_jobs_failed_total{kind="send_email"} 1"#,
    ] {
        assert!(
            body.contains(expected),
            "{expected} is missing from:\n{body}"
        );
    }
}

#[test]
fn a_fresh_process_already_lists_every_metric() {
    let recorder = telemetry::build_recorder();
    let handle = recorder.handle();
    ::metrics::with_local_recorder(&recorder, m::register);
    let body = handle.render();
    // These two exist only where they mean something: the budget in a process that runs
    // crawls, the tick once the scheduler has ticked. A 0 there would read as "stuck".
    let on_demand = [
        m::WORKER_MEMORY_BUDGET_BYTES,
        m::SCHEDULER_LAST_TICK_TIMESTAMP_SECONDS,
    ];
    for name in m::ALL.into_iter().filter(|n| !on_demand.contains(n)) {
        assert!(body.contains(name), "{name} is missing from:\n{body}");
    }
    for name in on_demand {
        assert!(
            !body.contains(&format!("\n{name} ")),
            "{name} should wait for a value"
        );
    }
}

#[test]
fn labels_hold_no_urls_or_emails() {
    let body = render_all();
    for line in body.lines().filter(|l| !l.starts_with('#')) {
        if let Some(labels) = line.split_once('{').map(|(_, rest)| rest) {
            assert!(!labels.contains('@'), "{line}");
            assert!(!labels.contains("://"), "{line}");
        }
    }
    // Every label value the helpers can produce is a short word.
    for value in m::LANES
        .iter()
        .chain(&m::CHANNELS)
        .chain(&m::JOB_KINDS)
        .chain(&m::API_RESULTS)
    {
        assert!(
            value.len() < 20 && !value.contains(['@', '/', ':']),
            "{value}"
        );
    }
}

#[test]
fn a_priority_outside_the_lanes_lands_in_the_nearest_one() {
    let recorder = telemetry::build_recorder();
    let handle = recorder.handle();
    ::metrics::with_local_recorder(&recorder, || {
        m::queue_wait(-1, 1.0);
        m::queue_wait(99, 1.0);
    });
    let body = handle.render();
    assert!(
        body.contains(r#"lane="0""#) && body.contains(r#"lane="5""#),
        "{body}"
    );
    assert!(!body.contains(r#"lane="99""#));
}

#[tokio::test]
async fn the_internal_listener_serves_metrics_and_nothing_else() {
    let recorder = telemetry::build_recorder();
    let handle = recorder.handle();
    ::metrics::with_local_recorder(&recorder, m::register);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, telemetry::metrics_router(handle))
            .await
            .unwrap()
    });
    let ok = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    let body = ok.text().await.unwrap();
    assert!(body.contains("codoseo_crawls_running"));
    let other = reqwest::get(format!("http://{addr}/")).await.unwrap();
    assert_eq!(other.status(), 404);
}

#[test]
fn the_metrics_bind_is_off_unless_set_and_must_be_an_address() {
    assert_eq!(telemetry::metrics_bind(None), Ok(None));
    assert_eq!(telemetry::metrics_bind(Some("  ")), Ok(None));
    assert_eq!(
        telemetry::metrics_bind(Some("0.0.0.0:9090")),
        Ok(Some("0.0.0.0:9090".parse().unwrap()))
    );
    assert!(telemetry::metrics_bind(Some("9090")).is_err());
}

#[test]
fn sentry_is_off_without_a_dsn_and_a_bad_dsn_is_reported_without_echoing_it() {
    assert!(telemetry::sentry_dsn(None).unwrap().is_none());
    assert!(telemetry::sentry_dsn(Some("  ")).unwrap().is_none());
    assert!(
        telemetry::sentry_dsn(Some("https://key@o1.ingest.sentry.io/42"))
            .unwrap()
            .is_some()
    );
    let err = telemetry::sentry_dsn(Some("https://secretkey@@nope")).unwrap_err();
    assert!(!err.contains("secretkey"), "{err}");
}
