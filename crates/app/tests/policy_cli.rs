//! `codoseo policy init|validate` and `crawl --policy`: the real binary, with the site policy
//! file in a temporary working directory.

use std::fs;
use std::sync::mpsc;

use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin_cmd;
use codoseo_core::audit::Audit;
use codoseo_testkit::SiteBuilder;
use url::Url;

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

fn codoseo(dir: &std::path::Path) -> Command {
    let mut cmd = cargo_bin_cmd!("codoseo");
    cmd.current_dir(dir);
    cmd
}

fn site() -> SiteBuilder {
    SiteBuilder::new()
        .html("/", "Home page title for tests here", &["/a"])
        .html("/a", "About page title for tests here", &["/"])
        .robots(200, "User-agent: *\nAllow: /\n")
}

const TRAINING_BLOCKED: &str = "[policy]\n[policy.ai]\npurposes = { training = \"block\" }\n";

#[test]
fn init_writes_the_commented_defaults() {
    let dir = tempfile::tempdir().unwrap();
    codoseo(dir.path())
        .args(["policy", "init"])
        .assert()
        .success();
    let text = fs::read_to_string(dir.path().join("codoseo.toml")).unwrap();
    insta::assert_snapshot!("policy_init", text);
    // It parses to the defaults.
    let parsed = codoseo_policy::parse_file(&text).unwrap();
    assert_eq!(parsed.policy, codoseo_policy::SitePolicy::default());

    // Never over a file that is there, unless asked.
    let out = codoseo(dir.path())
        .args(["policy", "init"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--force"));
    codoseo(dir.path())
        .args(["policy", "init", "--force", "-o", "other.toml"])
        .assert()
        .success();
    assert!(dir.path().join("other.toml").exists());
}

#[test]
fn init_from_a_url_reads_the_address_form() {
    let url = serve(site());
    let dir = tempfile::tempdir().unwrap();
    codoseo(dir.path())
        .args(["policy", "init", "--from", url.as_str()])
        .assert()
        .success();
    let text = fs::read_to_string(dir.path().join("codoseo.toml")).unwrap();
    let p = codoseo_policy::parse_file(&text).unwrap().policy;
    let a = p.seo.address.expect("the address is switched on");
    // 127.0.0.1 is not a www host, and the home page says nothing about slashes.
    assert_eq!(a.host, codoseo_policy::HostPref::Apex);
    assert_eq!(a.trailing_slash, codoseo_policy::Slash::Without);
}

#[test]
fn validate_says_what_is_wrong_and_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("codoseo.toml"),
        "[policy]\nmystery = 1\n[policy.ai]\npurposes = { training = \"block\" }\n",
    )
    .unwrap();
    let out = codoseo(dir.path())
        .args(["policy", "validate"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("codoseo.toml: valid policy (sha256:"),
        "{stdout}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown key policy.mystery (ignored)"),
        "{stderr}"
    );

    fs::write(
        dir.path().join("bad.toml"),
        "[policy]\nversion = 2\n[policy.seo]\nmust_work = [\"pricing\"]\n",
    )
    .unwrap();
    let out = codoseo(dir.path())
        .args(["policy", "validate", "bad.toml"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("bad.toml is not a valid policy:"),
        "{stderr}"
    );
    assert!(
        stderr.contains("version: version 2 is not supported"),
        "{stderr}"
    );
    assert!(stderr.contains("seo.must_work[0]"), "{stderr}");

    let out = codoseo(dir.path())
        .args(["policy", "validate", "missing.toml"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
}

fn audit_of(out: &std::process::Output) -> Audit {
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    Audit::from_json(&out.stdout).expect("an audit")
}

fn kinds(audit: &Audit) -> Vec<String> {
    audit.ai_access.as_ref().unwrap()["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["kind"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn crawl_judges_ai_access_under_the_policy_file() {
    let url = serve(site());
    let dir = tempfile::tempdir().unwrap();
    let crawl = |extra: &[&str]| {
        let mut cmd = codoseo(dir.path());
        cmd.args(["crawl", url.as_str(), "--rps", "50", "--format", "json"]);
        cmd.args(extra);
        cmd.output().unwrap()
    };

    // No file: the default intent, and no policy section.
    let plain = audit_of(&crawl(&[]));
    assert!(plain.policy.is_none());
    assert!(!kinds(&plain).contains(&"bots_not_blocked".to_owned()));

    // ./codoseo.toml is read: training crawlers that still get in are a finding.
    fs::write(dir.path().join("codoseo.toml"), TRAINING_BLOCKED).unwrap();
    let judged = audit_of(&crawl(&[]));
    assert!(kinds(&judged).contains(&"bots_not_blocked".to_owned()));
    let section = judged.policy.as_ref().expect("the policy section");
    assert_eq!(section["file"], "codoseo.toml");
    assert!(section["hash"].as_str().unwrap().starts_with("sha256:"));
    assert_eq!(section["policy"]["ai"]["purposes"]["training"], "block");
    let training = section["report"]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "ai_training")
        .unwrap();
    assert_eq!(training["status"], "gaps");
    assert!(section["report"]["ai_gaps"].as_u64().unwrap() > 0);
    assert_eq!(judged.format_version, 1, "the format version stays");

    // --no-policy ignores it; --policy FILE names another.
    let ignored = audit_of(&crawl(&["--no-policy"]));
    assert!(ignored.policy.is_none());
    fs::write(dir.path().join("codoseo.toml"), "[policy]\n").unwrap();
    fs::write(dir.path().join("strict.toml"), TRAINING_BLOCKED).unwrap();
    let named = audit_of(&crawl(&["--policy", "strict.toml"]));
    assert_eq!(named.policy.as_ref().unwrap()["file"], "strict.toml");
    assert!(kinds(&named).contains(&"bots_not_blocked".to_owned()));

    // An invalid file stops the run before the crawl.
    fs::write(dir.path().join("broken.toml"), "[policy]\nversion = 7\n").unwrap();
    let out = crawl(&["--policy", "broken.toml"]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("broken.toml is not a valid policy: version: version 7"),
        "{stderr}"
    );
}

#[test]
fn check_validates_the_policy_file_too() {
    let url = serve(site());
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("codoseo.toml"), "[policy]\nversion = 3\n").unwrap();
    let out = codoseo(dir.path())
        .args(["check", url.as_str()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    codoseo(dir.path())
        .args(["check", url.as_str(), "--no-policy"])
        .assert()
        .success();
}
