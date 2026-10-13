//! CI mode end to end: `crawl --fail-on` gates, `--baseline`, SARIF and GitHub annotations, with
//! the real `codoseo` binary against generated sites.
//!
//! The exit criterion of the CI kit lives here: a deploy whose robots.txt starts blocking an AI
//! search bot fails the build against the audit of the deploy before it, with a SARIF result.

use std::fs;
use std::sync::mpsc;

use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin_cmd;
use codoseo_testkit::{Page, SiteBuilder};
use predicates::prelude::*;
use url::Url;

/// The SARIF 2.1.0 schema as json.schemastore.org publishes it (draft-07; the OASIS original at
/// https://docs.oasis-open.org/sarif/sarif/v2.1.0/errata01/os/schemas/sarif-schema-2.1.0.json is
/// the same schema in draft-04). Vendored so the test runs offline; fetched 2026-10-13.
const SARIF_SCHEMA: &str = include_str!("data/sarif-schema-2.1.0.json");

/// Starts `site` on its own runtime, alive until the process exits, and returns its home page.
fn serve(site: SiteBuilder) -> Url {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a runtime");
        let started = rt.block_on(site.start());
        tx.send(started.url("/")).expect("the test is waiting");
        loop {
            std::thread::park();
        }
    });
    rx.recv().expect("the site started")
}

fn codoseo() -> Command {
    cargo_bin_cmd!("codoseo")
}

/// `crawl` with the speed turned up so tests stay fast.
fn crawl(url: &Url) -> Command {
    let mut cmd = codoseo();
    cmd.args(["crawl", url.as_str(), "--rps", "50"]);
    cmd
}

fn clean_site() -> SiteBuilder {
    SiteBuilder::new()
        .html("/", "Home page title for tests here", &["/a", "/b"])
        .html("/a", "About page title for tests here", &["/", "/b"])
        .html("/b", "Blog page title for tests here", &["/", "/a"])
        .sitemap(&["/", "/a", "/b"])
}

fn assert_valid_sarif(text: &str) -> serde_json::Value {
    let schema: serde_json::Value = serde_json::from_str(SARIF_SCHEMA).expect("schema");
    let validator = jsonschema::validator_for(&schema).expect("a valid schema");
    let doc: serde_json::Value = serde_json::from_str(text).expect("SARIF is JSON");
    let errors: Vec<String> = validator
        .iter_errors(&doc)
        .map(|e| format!("{e} at {}", e.instance_path()))
        .collect();
    assert!(errors.is_empty(), "invalid SARIF: {errors:#?}");
    doc
}

fn results<'a>(doc: &'a serde_json::Value, rule: &str) -> Vec<&'a serde_json::Value> {
    doc["runs"][0]["results"]
        .as_array()
        .expect("results")
        .iter()
        .filter(|r| r["ruleId"] == rule)
        .collect()
}

fn artifact(r: &serde_json::Value) -> &serde_json::Value {
    &r["locations"][0]["physicalLocation"]["artifactLocation"]
}

#[test]
fn a_geo_regression_against_the_baseline_fails_the_build_with_a_sarif_result() {
    let before = serve(clean_site().robots(200, "User-agent: *\nAllow: /\n"));
    let after = serve(clean_site().robots(
        200,
        "User-agent: OAI-SearchBot\nDisallow: /\n\nUser-agent: *\nAllow: /\n",
    ));
    let dir = tempfile::tempdir().expect("a temp dir");
    let base = dir.path().join("baseline.json");
    crawl(&before)
        .args(["--format", "json", "-o"])
        .arg(&base)
        .assert()
        .success();

    // Nothing new on the same site: it passes, and SARIF has nothing for the gate.
    let out = crawl(&before)
        .args([
            "--fail-on",
            "geo,critical",
            "--format",
            "sarif",
            "--baseline",
        ])
        .arg(&base)
        .output()
        .expect("the binary runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("passed against the baseline"), "{stderr}");
    let doc = assert_valid_sarif(&String::from_utf8(out.stdout).expect("utf-8"));
    assert!(results(&doc, "geo/bots_blocked").is_empty());

    // robots.txt now blocks OAI-SearchBot: exit 1 and a SARIF result, new since the baseline.
    let sarif = dir.path().join("codoseo.sarif");
    let audit = dir.path().join("after.json");
    let out = crawl(&after)
        .args(["--fail-on", "geo", "--format", "github", "--baseline"])
        .arg(&base)
        .arg("--save")
        .arg(&sarif)
        .arg("--save")
        .arg(&audit)
        .output()
        .expect("the binary runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("bots_blocked:search"), "{stderr}");
    assert!(stderr.contains("new since the baseline"), "{stderr}");
    // What tripped the gate is the first annotation, as an error.
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let first = stdout.lines().next().unwrap_or_default();
    assert!(
        first.starts_with("::error title=CodoSEO AI access bots_blocked%3Asearch"),
        "{stdout}"
    );
    assert!(first.contains("[new since the baseline]"), "{stdout}");

    let doc = assert_valid_sarif(&fs::read_to_string(&sarif).expect("the SARIF file"));
    let blocked = results(&doc, "geo/bots_blocked");
    assert_eq!(blocked.len(), 1, "{doc:#}");
    let r = blocked[0];
    assert_eq!(r["baselineState"], "new");
    assert_eq!(r["level"], "error");
    assert_eq!(r["properties"]["evidenceGrade"], "A");
    assert_eq!(r["properties"]["gate"], "bots_blocked:search");
    assert!(
        r["message"]["text"]
            .as_str()
            .is_some_and(|t| t.contains("OAI-SearchBot")),
        "{r}"
    );
    assert_eq!(artifact(r)["uriBaseId"], "SITE");
    let run = &doc["runs"][0];
    assert_eq!(run["originalUriBaseIds"]["SITE"]["uri"], after.as_str());
    let rules = run["tool"]["driver"]["rules"].as_array().expect("rules");
    // 44 SEO checks and 7 AI & GEO checks, one rule each.
    assert_eq!(rules.len(), 51);
    let rule = &rules[r["ruleIndex"].as_u64().expect("an index") as usize];
    assert_eq!(rule["id"], "geo/bots_blocked");
    assert_eq!(rule["properties"]["evidenceGrade"], "A");
    assert!(rule["help"]["text"].as_str().is_some_and(|t| t.len() > 40));

    // The saved audit is the next baseline: against it the block is no longer new.
    crawl(&after)
        .args(["--fail-on", "geo", "--baseline"])
        .arg(&audit)
        .assert()
        .code(0);
    // Without a baseline every finding counts; gates pick by kind and subject; an SEO gate
    // ignores AI access.
    crawl(&after).args(["--fail-on", "geo"]).assert().code(1);
    crawl(&after)
        .args(["--fail-on", "bots_blocked:search"])
        .assert()
        .code(1);
    crawl(&after)
        .args(["--fail-on", "bots_blocked:training"])
        .assert()
        .code(0);
    crawl(&after)
        .args(["--fail-on", "critical"])
        .assert()
        .code(0);
}

#[test]
fn a_failing_robots_txt_is_exit_1_when_a_gate_asks_for_it() {
    let url = serve(clean_site().robots(503, "down"));
    // The crawl can't run on a 5xx robots.txt, which is exit 2 on its own...
    crawl(&url).assert().code(2);
    crawl(&url).args(["--fail-on", "critical"]).assert().code(2);
    // ...but finding it is what the GEO gate is for.
    let out = crawl(&url)
        .args(["--fail-on", "robots_unavailable", "--format", "sarif"])
        .output()
        .expect("the binary runs");
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc = assert_valid_sarif(&String::from_utf8(out.stdout).expect("utf-8"));
    let r = results(&doc, "geo/robots_unavailable");
    assert_eq!(r.len(), 1, "{doc:#}");
    assert_eq!(artifact(r[0])["uri"], "robots.txt");
    assert_eq!(
        doc["runs"][0]["invocations"][0]["executionSuccessful"],
        false
    );
}

#[test]
fn seo_results_in_sarif_are_one_per_page() {
    let url = serve(
        SiteBuilder::new()
            .html(
                "/",
                "Home page title for tests here",
                &["/gone", "/also-gone"],
            )
            .page("/gone", Page::status(404, ""))
            .page("/also-gone", Page::status(404, "")),
    );
    let out = crawl(&url)
        .args(["--format", "sarif", "--fail-on", "critical"])
        .output()
        .expect("the binary runs");
    assert_eq!(out.status.code(), Some(1));
    let doc = assert_valid_sarif(&String::from_utf8(out.stdout).expect("utf-8"));
    let broken = results(&doc, "seo/http_4xx");
    assert_eq!(broken.len(), 2, "{doc:#}");
    assert!(
        broken
            .iter()
            .all(|r| r["level"] == "error" && r.get("baselineState").is_none())
    );
    let mut uris: Vec<&str> = broken
        .iter()
        .filter_map(|r| artifact(r)["uri"].as_str())
        .collect();
    uris.sort_unstable();
    assert_eq!(uris, ["also-gone", "gone"]);
}

#[test]
fn fail_on_save_and_baseline_reject_what_they_do_not_know() {
    for bad in [
        "notice",
        "geo:bogus",
        "bots_blocked:nobody",
        "content_missing:x",
        "bogus",
    ] {
        codoseo()
            .args(["crawl", "http://127.0.0.1:1/", "--fail-on", bad])
            .assert()
            .code(2);
    }
    codoseo()
        .args(["crawl", "http://127.0.0.1:1/", "--save", "report.xml"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(".sarif"));
    codoseo()
        .args([
            "crawl",
            "http://127.0.0.1:1/",
            "--baseline",
            "/nonexistent/audit.json",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot read"));
}

/// The GitHub Action's run step (`action/run.sh`), as the runner would call it, with this build of
/// `codoseo` first on the PATH: the same regression fails the step, and the step leaves its
/// outputs, the SARIF file, the next baseline and the job summary behind.
#[cfg(unix)]
#[test]
fn the_action_run_step_fails_on_a_geo_regression_and_writes_its_outputs() {
    let before = serve(clean_site());
    let after = serve(clean_site().robots(200, "User-agent: OAI-SearchBot\nDisallow: /\n"));
    let dir = tempfile::tempdir().expect("a temp dir");
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../../action/run.sh");
    let bin_dir = assert_cmd::cargo::cargo_bin!("codoseo")
        .parent()
        .expect("the binary's directory")
        .to_owned();
    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let step = |url: &Url, baseline: &str| {
        let output = dir.path().join("output");
        let summary = dir.path().join("summary.md");
        let _ = fs::remove_file(&output);
        let out = std::process::Command::new("bash")
            .arg(script)
            .current_dir(dir.path())
            .env("PATH", &path)
            .env("RUNNER_TEMP", dir.path())
            .env("GITHUB_OUTPUT", &output)
            .env("GITHUB_STEP_SUMMARY", &summary)
            .env("CODOSEO_URL", url.as_str())
            .env("CODOSEO_FAIL_ON", "critical,geo")
            .env("CODOSEO_BASELINE", baseline)
            .env("CODOSEO_RPS", "50")
            .env("CODOSEO_ARGS", "--max-time 60")
            .output()
            .expect("bash runs");
        let outputs = fs::read_to_string(&output).unwrap_or_default();
        (out, outputs)
    };

    // First run on main: no baseline yet, a notice, a pass, and an audit to keep.
    let (out, outputs) = step(&before, "baseline.json");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert!(
        stdout.contains("::notice::No baseline at baseline.json"),
        "{stdout}"
    );
    assert!(outputs.contains("exit-code=0\n"), "{outputs}");
    fs::rename(
        dir.path().join("codoseo-audit.json"),
        dir.path().join("baseline.json"),
    )
    .expect("the audit was written");

    // The pull request's deploy blocks OAI-SearchBot.
    let (out, outputs) = step(&after, "baseline.json");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("::error title=CodoSEO AI access bots_blocked%3Asearch"),
        "{stdout}"
    );
    assert!(stdout.contains("::error::CodoSEO: a --fail-on gate (critical,geo) failed"));
    assert!(outputs.contains("exit-code=1\n"), "{outputs}");
    assert!(outputs.contains("sarif=codoseo.sarif\n"), "{outputs}");
    let doc = assert_valid_sarif(
        &fs::read_to_string(dir.path().join("codoseo.sarif")).expect("the SARIF file"),
    );
    assert_eq!(results(&doc, "geo/bots_blocked")[0]["baselineState"], "new");
    let summary = fs::read_to_string(dir.path().join("summary.md")).expect("the job summary");
    assert!(summary.contains("AI access"), "{summary}");
}
