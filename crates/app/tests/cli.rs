//! End-to-end tests: the real `codoseo` binary against generated sites.
//!
//! Each site runs on its own background thread with a runtime that lives as long as the
//! test process, so the binary (a separate process) can crawl it.

use std::ffi::OsStr;
use std::fs;
use std::sync::mpsc;

use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin_cmd;
use codoseo_core::audit::Audit;
use codoseo_core::change::Change;
use codoseo_testkit::{Page, SiteBuilder, html_page};
use predicates::prelude::*;
use url::Url;

/// Starts `site` and returns the address of its home page.
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
        // Keeps the runtime, and so the server, alive until the process exits.
        loop {
            std::thread::park();
        }
    });
    rx.recv().expect("the site started")
}

/// Starts `site` and returns the address of its home page and a way to read how many
/// requests it has received so far.
fn serve_counted(site: SiteBuilder) -> (Url, impl Fn() -> usize) {
    let (tx, rx) = mpsc::channel();
    let (ask_tx, ask_rx) = mpsc::channel::<mpsc::Sender<usize>>();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a runtime");
        let started = rt.block_on(site.start());
        tx.send(started.url("/")).expect("the test is waiting");
        while let Ok(reply) = ask_rx.recv() {
            let _ = reply.send(started.hits());
        }
        loop {
            std::thread::park();
        }
    });
    let url = rx.recv().expect("the site started");
    let hits = move || {
        let (reply_tx, reply_rx) = mpsc::channel();
        ask_tx.send(reply_tx).expect("the site is running");
        reply_rx.recv().expect("a hit count")
    };
    (url, hits)
}

fn closed_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
    listener.local_addr().expect("an address").port()
}

fn at(base: &Url, path: &str) -> String {
    base.join(path).expect("a path").to_string()
}

fn codoseo() -> Command {
    cargo_bin_cmd!("codoseo")
}

/// Runs the binary, expects exit 0 and returns stdout.
fn run<I, S>(args: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let out = codoseo().args(args).output().expect("the binary runs");
    assert!(
        out.status.success(),
        "exit {:?}, stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf-8 output")
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

#[test]
fn fail_on_critical_exits_1_when_a_critical_check_fails() {
    let url = serve(
        SiteBuilder::new()
            .html("/", "Home page title for tests here", &["/gone"])
            .page("/gone", Page::status(404, "")),
    );
    crawl(&url).args(["--fail-on", "critical"]).assert().code(1);
}

#[test]
fn fail_on_critical_exits_0_on_a_clean_site() {
    let url = serve(clean_site());
    crawl(&url).args(["--fail-on", "critical"]).assert().code(0);
}

#[test]
fn fail_on_warning_counts_warnings() {
    let url = serve(
        SiteBuilder::new()
            .html("/", "The same title on both pages", &["/a"])
            .html("/a", "The same title on both pages", &["/"])
            .sitemap(&["/", "/a"]),
    );
    crawl(&url).args(["--fail-on", "critical"]).assert().code(0);
    crawl(&url).args(["--fail-on", "warning"]).assert().code(1);
}

#[test]
fn json_stdout_is_a_parseable_audit() {
    let url = serve(clean_site());
    let out = crawl(&url)
        .args(["--format", "json"])
        .output()
        .expect("the binary runs");
    assert!(out.status.success());
    let audit = Audit::from_json(&out.stdout).expect("stdout is an audit");
    assert_eq!(audit.format_version, 1);
    assert_eq!(audit.tool_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(audit.snapshot.pages.len(), 3);
    // Not a terminal: no progress line, no escape codes, nothing on stderr.
    assert!(!out.stdout.contains(&0x1b));
    assert!(!out.stdout.contains(&b'\r'));
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A chain, so the crawl order (and the examples listed) never varies.
fn chain_site() -> SiteBuilder {
    SiteBuilder::new()
        .html("/", "Home page title for tests here", &["/a"])
        .html("/a", "About page title for tests here", &["/b"])
        .html("/b", "Blog page title for tests here", &["/gone"])
        .page("/gone", Page::status(404, ""))
        .sitemap(&["/", "/a", "/b"])
}

#[test]
fn markdown_report_snapshot() {
    let url = serve(chain_site());
    let out = run(["crawl", url.as_str(), "--rps", "50", "--format", "md"]);
    insta::with_settings!({ filters => vec![
        (r"127\.0\.0\.1:\d+", "[host]"),
        (r"\d+ ms", "[ms]"),
        (r"\d+\.\d+ s", "[s]"),
    ] }, {
        insta::assert_snapshot!(out);
    });
}

#[test]
fn table_report_lists_failing_checks_with_examples() {
    let url = serve(chain_site());
    let out = run(["crawl", url.as_str(), "--rps", "50"]);
    assert!(out.contains("Health score"), "{out}");
    assert!(out.contains("Page returns a 4xx error"), "{out}");
    assert!(out.contains("http_4xx"), "{out}");
    assert!(out.contains(&at(&url, "/gone")), "{out}");
    assert!(out.contains("Critical"), "{out}");
    assert!(!out.contains('\r') && !out.contains('\u{1b}'));
}

#[test]
fn csv_has_a_row_per_page_and_escapes_commas() {
    let url = serve(
        SiteBuilder::new()
            .html("/", "Tents, tarps and \"poles\" for every camper", &["/a"])
            .html("/a", "Second page title for the csv test", &["/"])
            .sitemap(&["/"]),
    );
    let out = run(["crawl", url.as_str(), "--rps", "50", "--format", "csv"]);
    let mut reader = csv::Reader::from_reader(out.as_bytes());
    let headers: Vec<String> = reader
        .headers()
        .expect("a header row")
        .iter()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        headers.join(","),
        "url,status,indexability,title,meta_description,h1,word_count,depth,inlinks,\
         response_ms,in_sitemap,issues"
    );
    let rows: Vec<csv::StringRecord> = reader.records().map(|r| r.expect("a row")).collect();
    assert_eq!(rows.len(), 2);
    let home = &rows[0];
    assert_eq!(&home[0], url.as_str());
    assert_eq!(&home[1], "200");
    assert_eq!(&home[2], "indexable");
    assert_eq!(&home[3], "Tents, tarps and \"poles\" for every camper");
    assert_eq!(&home[7], "0");
    assert_eq!(&home[10], "true");
    let other = &rows[1];
    assert_eq!(&other[10], "false");
    assert!(
        other[11].split(';').any(|s| s == "not_in_sitemap"),
        "{:?}",
        &other[11]
    );
}

/// Home page and `/b` answer differently the second time they are crawled, so two crawls of
/// one server (one origin) differ in a title and a status.
fn changing_site() -> SiteBuilder {
    SiteBuilder::new()
        .page(
            "/",
            Page::sequence(vec![
                Page::html(&html_page("Home page title before the change", &["/b"])),
                Page::html(&html_page("Home page title after the change!", &["/b"])),
            ]),
        )
        .page(
            "/b",
            Page::sequence(vec![
                Page::html(&html_page("Blog page title for the diff test", &[])),
                Page::status(404, ""),
            ]),
        )
}

#[test]
fn diff_of_two_saved_audits() {
    let url = serve(changing_site());
    let dir = tempfile::tempdir().expect("a temp dir");
    let a = dir.path().join("a.json");
    let b = dir.path().join("b.json");
    for file in [&a, &b] {
        crawl(&url)
            .args(["--format", "json", "-o"])
            .arg(file)
            .assert()
            .success();
    }
    let text = run([OsStr::new("diff"), a.as_os_str(), b.as_os_str()]);
    assert!(text.contains("title_changed"), "{text}");
    assert!(text.contains("status_changed"), "{text}");
    assert!(text.contains(&at(&url, "/b")), "{text}");

    codoseo()
        .arg("diff")
        .args([&a, &b])
        .args(["--fail-on", "warning"])
        .assert()
        .code(1);

    let json = run([
        OsStr::new("diff"),
        a.as_os_str(),
        b.as_os_str(),
        OsStr::new("--format"),
        OsStr::new("json"),
    ]);
    let changes: Vec<Change> = serde_json::from_str(&json).expect("an array of changes");
    assert!(changes.len() >= 2);

    let md = run([
        OsStr::new("diff"),
        a.as_os_str(),
        b.as_os_str(),
        OsStr::new("--format"),
        OsStr::new("md"),
    ]);
    assert!(
        md.contains("| Severity |") && md.contains("title_changed"),
        "{md}"
    );

    // The same audit twice has nothing to report and passes any threshold.
    codoseo()
        .arg("diff")
        .args([&a, &a])
        .args(["--fail-on", "warning"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains("No changes"));
}

#[test]
fn diff_rejects_a_newer_format() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let a = dir.path().join("a.json");
    fs::write(&a, r#"{"format_version": 2}"#).expect("a file");
    codoseo()
        .arg("diff")
        .args([&a, &a])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("version 2"));
}

#[test]
fn diff_rejects_a_file_that_is_not_an_audit() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let a = dir.path().join("a.json");
    fs::write(&a, "not json").expect("a file");
    codoseo()
        .arg("diff")
        .args([&a, &a])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("a.json"));
    codoseo()
        .args(["diff", "/no/such/file.json", "/no/such/file.json"])
        .assert()
        .code(2);
}

#[test]
fn check_robots_and_redirects_commands() {
    let url = serve(
        SiteBuilder::new()
            .html("/", "Home page title for tests here", &["/r1"])
            .robots(200, "User-agent: *\nDisallow: /private\nCrawl-delay: 2\nSitemap: https://example.com/sitemap.xml\n")
            .page("/r1", Page::redirect(301, "/r2"))
            .page("/r2", Page::redirect(302, "/"))
            .page("/loop1", Page::redirect(301, "/loop2"))
            .page("/loop2", Page::redirect(301, "/loop1")),
    );

    let check = run(["check", url.as_str()]);
    for label in [
        "Title",
        "Meta description",
        "H1",
        "Canonical",
        "Status",
        "Indexability",
        "Word count",
        "Response time",
    ] {
        assert!(check.contains(label), "missing {label}: {check}");
    }
    assert!(check.contains("Home page title for tests here"), "{check}");
    assert!(check.contains("Page has under 200 words"), "{check}");

    let record: serde_json::Value =
        serde_json::from_str(&run(["check", url.as_str(), "--format", "json"])).expect("json");
    assert_eq!(record["status"], 200);

    let robots = run(["robots", url.as_str(), "--path", "/private"]);
    assert!(robots.contains("blocked"), "{robots}");
    assert!(robots.contains("2 s"), "{robots}");
    assert!(
        robots.contains("https://example.com/sitemap.xml"),
        "{robots}"
    );
    assert!(run(["robots", url.as_str(), "--path", "/public"]).contains("allowed"));
    let robots_json: serde_json::Value = serde_json::from_str(&run([
        "robots",
        url.as_str(),
        "--path",
        "/private",
        "--format",
        "json",
    ]))
    .expect("json");
    assert_eq!(robots_json["status"], 200);
    assert_eq!(robots_json["path"], "/private");
    assert_eq!(robots_json["allowed"], false);
    assert_eq!(robots_json["crawl_delay_secs"], 2.0);

    let redirects = run(["redirects", &at(&url, "/r1")]);
    assert!(redirects.contains("301"), "{redirects}");
    assert!(redirects.contains("302"), "{redirects}");
    assert!(redirects.contains("200"), "{redirects}");
    let hops: Vec<&str> = redirects.lines().collect();
    assert!(
        hops[0].starts_with("301") && hops[0].ends_with("/r1"),
        "{redirects}"
    );
    let json: serde_json::Value =
        serde_json::from_str(&run(["redirects", &at(&url, "/r1"), "--format", "json"]))
            .expect("json");
    assert_eq!(json["hops"].as_array().map(Vec::len), Some(2));
    assert_eq!(json["final_status"], 200);

    // A loop is a finding, not a failure of the command.
    let looped = run(["redirects", &at(&url, "/loop1")]);
    assert!(looped.contains("loop"), "{looped}");
    assert!(looped.contains("301"), "{looped}");
}

#[test]
fn unreachable_site_exits_2() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
        listener.local_addr().expect("an address").port()
    };
    let out = codoseo()
        .args(["crawl", &format!("http://127.0.0.1:{port}/"), "--rps", "50"])
        .output()
        .expect("the binary runs");
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unreachable"));
    // The report is still printed first.
    assert!(String::from_utf8_lossy(&out.stdout).contains("Health score"));
}

#[test]
fn a_site_that_blocks_crawlers_in_robots_txt_exits_2_even_with_fail_on() {
    let url = serve(
        SiteBuilder::new()
            .html("/", "Home page title for tests here", &[])
            .robots(200, "User-agent: *\nDisallow: /\n"),
    );
    crawl(&url)
        .args(["--fail-on", "critical"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("robots"));
}

#[test]
fn a_site_that_refuses_the_crawler_exits_2() {
    let url = serve(SiteBuilder::new().every_path(Page::status(403, "forbidden")));
    crawl(&url)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("blocked"));
}

#[test]
fn output_flag_writes_the_report_to_a_file() {
    let url = serve(clean_site());
    let dir = tempfile::tempdir().expect("a temp dir");
    let file = dir.path().join("report.md");
    crawl(&url)
        .args(["--format", "md", "-o"])
        .arg(&file)
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
    let text = fs::read_to_string(&file).expect("the report");
    assert!(text.contains("checks passed"), "{text}");
}

#[test]
fn bad_arguments_exit_2() {
    codoseo().args(["crawl"]).assert().code(2);
    codoseo().args(["crawl", "not a url"]).assert().code(2);
    codoseo()
        .args(["crawl", "http://127.0.0.1:1/", "--format", "xml"])
        .assert()
        .code(2);
    codoseo()
        .args(["crawl", "http://127.0.0.1:1/", "--fail-on", "notice"])
        .assert()
        .code(2);
    codoseo().args(["bogus"]).assert().code(2);
}

/// A page whose text fields are hostile: spreadsheet formulas and terminal escapes.
fn hostile_page(title: &str, description: &str, h1: &str) -> Page {
    Page::html(&format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title>\
         <meta name=\"description\" content=\"{description}\"></head>\
         <body><h1>{h1}</h1></body></html>"
    ))
}

#[test]
fn csv_neutralises_spreadsheet_formulas_in_text_columns_only() {
    let url = serve(SiteBuilder::new().page(
        "/",
        hostile_page(
            "=HYPERLINK(\"http://x\",\"y\")",
            "-2+3 for the price",
            "@SUM(A1)",
        ),
    ));
    let out = run(["crawl", url.as_str(), "--rps", "50", "--format", "csv"]);
    let mut reader = csv::Reader::from_reader(out.as_bytes());
    let row = reader
        .records()
        .next()
        .expect("a row")
        .expect("a valid row");
    assert_eq!(&row[3], "'=HYPERLINK(\"http://x\",\"y\")");
    assert_eq!(&row[4], "'-2+3 for the price");
    assert_eq!(&row[5], "'@SUM(A1)");
    // Other columns are untouched, including a URL and a negative-looking number column.
    assert_eq!(&row[0], url.as_str());
    assert_eq!(&row[1], "200");
}

#[test]
fn terminal_escapes_in_crawled_text_never_reach_stdout() {
    let url = serve(SiteBuilder::new().page(
        "/",
        hostile_page(
            "Evil\u{1b}[31mRed",
            "Desc\u{7}with\u{9b}bell",
            "Head\u{1b}]0;x\u{7}ing",
        ),
    ));
    let has_escape = |bytes: &[u8]| bytes.iter().any(|b| *b == 0x1b || *b == 0x07);

    let check = run(["check", url.as_str()]);
    assert!(!has_escape(check.as_bytes()), "{check:?}");
    assert!(!check.contains('\u{9b}'), "{check:?}");
    assert!(check.contains("Evil[31mRed"), "{check:?}");

    for format in ["md", "table", "csv"] {
        let out = run(["crawl", url.as_str(), "--rps", "50", "--format", format]);
        assert!(!has_escape(out.as_bytes()), "{format}: {out:?}");
        assert!(!out.contains('\u{9b}'), "{format}: {out:?}");
    }
    let csv = run(["crawl", url.as_str(), "--rps", "50", "--format", "csv"]);
    assert!(csv.contains("Evil[31mRed"), "{csv:?}");

    // JSON stays raw: serde escapes the control character and the value round-trips.
    let json = run(["check", url.as_str(), "--format", "json"]);
    assert!(json.contains("\\u001b"), "{json}");
    let record: serde_json::Value = serde_json::from_str(&json).expect("json");
    assert_eq!(record["fields"]["title"], "Evil\u{1b}[31mRed");
}

#[test]
fn terminal_escapes_in_diffed_values_never_reach_stdout() {
    let url = serve(SiteBuilder::new().page(
        "/",
        Page::sequence(vec![
            hostile_page("Calm title for the first crawl", "d", "h"),
            hostile_page("Evil\u{1b}[31mRed title after", "d", "h"),
        ]),
    ));
    let dir = tempfile::tempdir().expect("a temp dir");
    let a = dir.path().join("a.json");
    let b = dir.path().join("b.json");
    for file in [&a, &b] {
        crawl(&url)
            .args(["--format", "json", "-o"])
            .arg(file)
            .assert()
            .success();
    }
    for format in ["table", "md"] {
        let out = run([
            OsStr::new("diff"),
            a.as_os_str(),
            b.as_os_str(),
            OsStr::new("--format"),
            OsStr::new(format),
        ]);
        assert!(out.contains("title_changed"), "{out}");
        assert!(out.contains("Evil[31mRed"), "{format}: {out:?}");
        assert!(!out.contains('\u{1b}'), "{format}: {out:?}");
    }
    // JSON keeps the raw value, escaped.
    let json = run([
        OsStr::new("diff"),
        a.as_os_str(),
        b.as_os_str(),
        OsStr::new("--format"),
        OsStr::new("json"),
    ]);
    assert!(json.contains("\\u001b"), "{json}");
}

#[test]
fn an_unwritable_output_path_fails_before_crawling() {
    let (url, hits) = serve_counted(clean_site());
    let dir = tempfile::tempdir().expect("a temp dir");
    let file = dir.path().join("no-such-dir").join("report.json");
    let started = std::time::Instant::now();
    crawl(&url)
        .args(["--format", "json", "-o"])
        .arg(&file)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot write"));
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(hits(), 0, "the crawl must not start");
}

#[test]
fn a_closed_stdout_pipe_is_a_quiet_success() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    // Enough pages that the CSV is larger than a pipe buffer, so the binary is still
    // writing when the reader goes away after the first line.
    // 40 rows of about 6 KB each (the long URL appears in more than one column).
    let paths: Vec<String> = (0..40)
        .map(|i| format!("/page-{i:02}-{}", "x".repeat(2_000)))
        .collect();
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let url = serve(
        SiteBuilder::new()
            .html("/", "Home page title for tests here", &refs)
            .every_path(Page::html(&html_page(
                "A page title for the pipe test",
                &[],
            ))),
    );
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_codoseo"))
        .args(["crawl", url.as_str(), "--rps", "50", "--format", "csv"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary runs");
    let mut first = String::new();
    BufReader::new(child.stdout.take().expect("stdout"))
        .read_line(&mut first)
        .expect("a first line");
    assert!(first.starts_with("url,"), "{first}");
    // The reader is dropped here: the pipe is closed.
    let out = child.wait_with_output().expect("the binary exits");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stderr.is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn check_exits_2_when_the_page_cannot_be_fetched() {
    let out = codoseo()
        .args(["check", &format!("http://127.0.0.1:{}/", closed_port())])
        .output()
        .expect("the binary runs");
    assert_eq!(out.status.code(), Some(2));
    // The table is still printed first.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Status") && stdout.contains("Error"),
        "{stdout}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("could not be fetched"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn diff_of_incomplete_crawls_says_so() {
    let url = serve(clean_site());
    let dir = tempfile::tempdir().expect("a temp dir");
    let a = dir.path().join("a.json");
    let b = dir.path().join("b.json");
    for file in [&a, &b] {
        crawl(&url)
            .args(["--max-pages", "1", "--format", "json", "-o"])
            .arg(file)
            .assert()
            .success();
    }
    let note =
        "Note: both crawls stopped early (page limit); new and removed URLs were not compared.";
    let out = codoseo()
        .arg("diff")
        .args([&a, &b])
        .output()
        .expect("the binary runs");
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stderr.trim_end(), note);
    assert!(stdout.starts_with(note), "{stdout}");

    let md = run([
        OsStr::new("diff"),
        a.as_os_str(),
        b.as_os_str(),
        OsStr::new("--format"),
        OsStr::new("md"),
    ]);
    assert!(md.starts_with(note), "{md}");

    // JSON stays a pure array; the note goes to stderr only.
    let out = codoseo()
        .arg("diff")
        .args([&a, &b])
        .args(["--format", "json"])
        .output()
        .expect("the binary runs");
    assert_eq!(out.status.code(), Some(0));
    let changes: Vec<Change> = serde_json::from_slice(&out.stdout).expect("an array");
    assert!(changes.is_empty());
    assert_eq!(String::from_utf8_lossy(&out.stderr).trim_end(), note);

    // Complete crawls: no note anywhere.
    let full = dir.path().join("full.json");
    crawl(&url)
        .args(["--format", "json", "-o"])
        .arg(&full)
        .assert()
        .success();
    let out = codoseo()
        .arg("diff")
        .args([&full, &full])
        .output()
        .expect("the binary runs");
    assert!(out.stderr.is_empty());
    assert!(!String::from_utf8_lossy(&out.stdout).contains("Note"));

    // Only the newer crawl incomplete: only removed URLs go unchecked.
    let out = codoseo()
        .arg("diff")
        .args([&full, &b])
        .output()
        .expect("the binary runs");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim_end(),
        "Note: the newer crawl stopped early (page limit); removed URLs were not compared."
    );
}

#[test]
fn a_robots_txt_that_starts_blocking_the_crawl_is_a_critical_change() {
    let url = serve(
        SiteBuilder::new()
            .html("/", "Home page title for tests here", &[])
            .page(
                "/robots.txt",
                Page::sequence(vec![
                    Page::status(200, "User-agent: *\nDisallow: /private\n")
                        .header("content-type", "text/plain"),
                    Page::status(200, "User-agent: *\nDisallow: /\n")
                        .header("content-type", "text/plain"),
                ]),
            ),
    );
    let dir = tempfile::tempdir().expect("a temp dir");
    let a = dir.path().join("a.json");
    let b = dir.path().join("b.json");
    crawl(&url)
        .args(["--format", "json", "-o"])
        .arg(&a)
        .assert()
        .success();
    // The blocked crawl exits 2 but still saves its audit.
    crawl(&url)
        .args(["--format", "json", "-o"])
        .arg(&b)
        .assert()
        .code(2);

    let out = codoseo()
        .arg("diff")
        .args([&a, &b])
        .args(["--fail-on", "critical"])
        .output()
        .expect("the binary runs");
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout
            .lines()
            .any(|l| l.starts_with("critical") && l.contains("robots_txt_changed")),
        "{stdout}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("blocked by robots.txt"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn bots_lists_the_registry_and_json_is_the_raw_file() {
    let table = run(["bots"]);
    assert!(table.contains("OAI-SearchBot"), "{table}");
    assert!(table.contains("OpenAI"), "{table}");
    let json = run(["bots", "--format", "json"]);
    assert_eq!(json.trim_end(), codoseo_geo::registry_json().trim_end());
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("json");
    assert_eq!(parsed["license"], "CC0-1.0");
}

#[test]
fn robots_shows_what_it_says_to_ai_bots() {
    let url = serve(
        SiteBuilder::new()
            .html("/", "Home page title for tests here", &[])
            .robots(
                200,
                "User-agent: GPTBot\nDisallow: /\n\nUser-agent: *\nAllow: /\nContent-Signal: ai-train=no, search=yes\n",
            ),
    );
    let table = run(["robots", url.as_str()]);
    let gpt = table
        .lines()
        .find(|l| l.trim_start().starts_with("GPTBot"))
        .expect("a GPTBot row");
    assert!(gpt.contains("Blocked") && gpt.contains("line 2"), "{gpt}");
    let search = table
        .lines()
        .find(|l| l.trim_start().starts_with("OAI-SearchBot"))
        .expect("an OAI-SearchBot row");
    assert!(search.contains("Allowed"), "{search}");
    assert!(table.contains("Content-Signal (line 6)"), "{table}");
    assert!(table.contains("ai-train=no"), "{table}");

    let json: serde_json::Value =
        serde_json::from_str(&run(["robots", url.as_str(), "--format", "json"])).expect("json");
    assert_eq!(json["allowed"], true);
    let bots = json["bots"].as_array().expect("bots");
    let gptbot = bots
        .iter()
        .find(|b| b["token"] == "GPTBot")
        .expect("GPTBot");
    assert_eq!(gptbot["allowed"], false);
    assert_eq!(gptbot["line"], 2);
    assert_eq!(json["declared"]["content_signals"][0]["line"], 6);
}

#[test]
fn crawl_reports_ai_access_in_every_format_and_old_audits_still_diff() {
    let url = serve(clean_site().robots(200, "User-agent: OAI-SearchBot\nDisallow: /\n"));
    let table = run(["crawl", url.as_str(), "--rps", "50"]);
    assert!(table.contains("\nAI access\n"), "{table}");
    assert!(table.contains("OAI-SearchBot"), "{table}");
    let md = run(["crawl", url.as_str(), "--rps", "50", "--format", "md"]);
    assert!(md.contains("## AI access"), "{md}");

    let dir = tempfile::tempdir().expect("a temp dir");
    let new = dir.path().join("new.json");
    crawl(&url)
        .args(["--format", "json", "-o"])
        .arg(&new)
        .assert()
        .success();
    let text = fs::read_to_string(&new).expect("file");
    let mut value: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert!(value["ai_access"]["report"]["bots"].is_array());
    assert!(
        value["ai_access"]["findings"]
            .as_array()
            .is_some_and(|f| !f.is_empty())
    );
    assert_eq!(value["format_version"], 1);

    // An audit saved before the field existed loads, and diffs against one that has it.
    value.as_object_mut().expect("object").remove("ai_access");
    let old = dir.path().join("old.json");
    fs::write(&old, serde_json::to_vec(&value).expect("json")).expect("write");
    assert!(
        Audit::from_json(&fs::read(&old).expect("read"))
            .expect("loads")
            .ai_access
            .is_none()
    );
    run([OsStr::new("diff"), old.as_os_str(), new.as_os_str()]);
    run([OsStr::new("diff"), new.as_os_str(), old.as_os_str()]);
}
