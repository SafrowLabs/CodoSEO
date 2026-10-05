//! M8 T4: the no-key tier of the cloud MCP server at `/mcp`, driven in-process over HTTP
//! JSON-RPC through the real app: the four tools, quick audits started and read back (a fixture
//! finishes the crawl the way the worker would), the domain reuse, the agent daily budget (also
//! under concurrency), the per-IP limits shared with the website, the address guard, and the
//! rule that only quick audits are readable by id.

mod support;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use codoseo_core::output::StopReason;
use codoseo_core::page::Indexability;
use codoseo_core::plan::Plan;
use serde_json::{Value, json};
use support::{TestApp, cloud_config, cloud_config_with, page};
use uuid::Uuid;

use support::mcp::{Client, DIRECT_UA, HOST, PROTOCOL, SHARED_UA, jsonrpc};

async fn cloud() -> TestApp {
    TestApp::with_config(cloud_config()).await
}

const SHORT: Duration = Duration::from_millis(300);

/// A site with a spread of defects, so the audit has several failing checks.
fn messy_pages(domain: &str) -> Vec<codoseo_core::page::PageRecord> {
    let mut pages = vec![page(domain, "/")];
    for i in 0..4 {
        let mut p = page(domain, &format!("/messy-{i}"));
        p.fields.title = None;
        p.fields.title_count = 0;
        p.fields.meta_description = None;
        p.fields.h1.clear();
        pages.push(p);
    }
    let mut gone = page(domain, "/gone");
    gone.status = 404;
    gone.indexability = Indexability::ClientError;
    pages.push(gone);
    pages
}

async fn finish(app: &TestApp, crawl: Uuid, domain: &str) {
    app.finalize_crawl(
        crawl,
        messy_pages(domain),
        Vec::new(),
        StopReason::Completed,
    )
    .await;
}

async fn count(app: &TestApp, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(app.pool()).await.unwrap()
}

// ---- who gets what ----

#[tokio::test]
async fn without_a_key_the_tool_list_is_exactly_the_four_no_key_tools() {
    let app = cloud().await;
    let client = Client::new(&app, SHORT, Some(SHARED_UA));
    let init = client
        .result(
            "initialize",
            json!({"protocolVersion": PROTOCOL, "capabilities": {},
                   "clientInfo": {"name": "test", "version": "0"}}),
        )
        .await;
    assert_eq!(init["serverInfo"]["name"], "codoseo");

    let tools = client.tools().await;
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort();
    assert_eq!(
        names,
        [
            "get_audit",
            "get_issue_urls",
            "quick_audit",
            "start_monitoring"
        ]
    );
    for t in &tools {
        let name = t["name"].as_str().unwrap();
        assert!(
            t["description"].as_str().is_some_and(|d| d.len() > 60),
            "{name}"
        );
        assert_eq!(t["inputSchema"]["type"], "object", "{name}");
        let hints = &t["annotations"];
        assert!(hints["title"].is_string(), "{name}: {hints}");
        assert_eq!(hints["destructiveHint"], false, "{name}");
        let reads = matches!(name, "get_audit" | "get_issue_urls");
        assert_eq!(hints["readOnlyHint"], reads, "{name}");
        // Starting an audit is repeatable (the same domain returns the same audit); sending an
        // email is not. Both reach outside CodoSEO, reading does not.
        assert_eq!(
            hints["idempotentHint"],
            name != "start_monitoring",
            "{name}"
        );
        assert_eq!(hints["openWorldHint"], !reads, "{name}");
    }
    let by_name = |n: &str| tools.iter().find(|t| t["name"] == n).unwrap();
    assert_eq!(
        by_name("quick_audit")["inputSchema"]["required"],
        json!(["url"])
    );
    assert_eq!(
        by_name("start_monitoring")["inputSchema"]["required"],
        json!(["url", "email"])
    );
    assert_eq!(
        by_name("get_issue_urls")["inputSchema"]["required"],
        json!(["audit_id", "check"])
    );
    for tool in &tools {
        for (arg, schema) in tool["inputSchema"]["properties"].as_object().unwrap() {
            assert!(schema["description"].is_string(), "{}.{arg}", tool["name"]);
        }
    }

    // The keyed tools are not there for a caller without a key.
    for keyed in [
        "list_sites",
        "get_site_health",
        "get_page",
        "get_changes",
        "run_crawl",
    ] {
        let res = client
            .post(jsonrpc(
                "tools/call",
                json!({"name": keyed, "arguments": {"site_id": "x"}}),
            ))
            .await;
        assert!(res.body.contains("tool not found"), "{keyed}: {}", res.body);
    }
}

#[tokio::test]
async fn self_hosted_without_a_key_is_a_401_and_the_cloud_app_router_serves_the_tier() {
    let app = TestApp::new().await;
    let res = Client::new(&app, SHORT, None)
        .post(jsonrpc("tools/list", json!({})))
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.body);
    assert_eq!(res.header("www-authenticate"), Some("Bearer"));

    // And the real app router (not a custom one) answers no-key calls in the cloud.
    let cloud = cloud().await;
    let res = cloud
        .send(
            Request::builder()
                .method(Method::POST)
                .uri("/mcp")
                .header(header::HOST, HOST)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json, text/event-stream")
                .header("mcp-protocol-version", PROTOCOL)
                .body(Body::from(jsonrpc("tools/list", json!({})).to_string()))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("quick_audit"));
}

// ---- quick_audit, get_audit ----

#[tokio::test]
async fn quick_audit_starts_an_agent_crawl_then_get_audit_returns_the_summary() {
    let app = cloud().await;
    let client = Client::new(&app, SHORT, Some(SHARED_UA));

    // Nothing finishes the crawl during the wait, so the answer is "running" with the id.
    let started = std::time::Instant::now();
    let state = client
        .call_ok("quick_audit", json!({"url": "Example.com"}))
        .await;
    assert!(started.elapsed() >= SHORT, "it waited for the audit");
    assert_eq!(state["status"], "running");
    assert!(state["message"].as_str().unwrap().contains("get_audit"));
    let id = Uuid::parse_str(state["audit_id"].as_str().unwrap()).unwrap();

    let (trigger, source, priority, ip, status, domain): (
        String,
        Option<String>,
        i16,
        Option<Vec<u8>>,
        String,
        String,
    ) = sqlx::query_as(
        "SELECT trigger::text, source, priority, requester_ip_hash, status::text, domain \
         FROM crawls WHERE id = $1",
    )
    .bind(id)
    .fetch_one(app.pool())
    .await
    .unwrap();
    assert_eq!(
        (
            trigger.as_str(),
            source.as_deref(),
            priority,
            status.as_str(),
            domain.as_str()
        ),
        ("quick", Some("agent"), 0, "queued", "example.com")
    );
    assert_eq!(ip, None, "a shared connector's address is not kept");
    assert_eq!(
        count(&app, "SELECT count(*) FROM events WHERE kind = 'audit_started' AND payload->>'source' = 'agent'").await,
        1
    );

    // get_audit still says running, then the fixture finishes the crawl.
    let again = client
        .call_ok("get_audit", json!({"audit_id": id.to_string()}))
        .await;
    assert_eq!(again["status"], "running");
    finish(&app, id, "example.com").await;

    let (is_error, text) = client
        .call("get_audit", json!({"audit_id": id.to_string()}))
        .await;
    assert!(!is_error, "{text}");
    assert!(text.len() < 4096, "{} bytes", text.len());
    let done: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(done["status"], "done");
    assert_eq!(done["audit_id"], id.to_string());
    assert_eq!(done["domain"], "example.com");
    assert_eq!(done["pages_crawled"], 6);
    assert_eq!(done["stop_code"], "completed");
    assert!(done["health_score"].as_u64().unwrap() <= 100);
    assert!(done["checks_total"].as_u64().unwrap() > 20);
    assert_eq!(
        done["report_url"],
        format!("https://codoseo.com/audit/{id}")
    );
    assert!(done["note"].as_str().unwrap().contains("start_monitoring"));
    let failing = done["failing_checks"].as_array().unwrap();
    assert!(failing.len() >= 3, "{done}");
    for check in failing {
        assert!(check["count"].as_u64().unwrap() >= 1);
        // Site-wide checks (no sitemap) have no page to point at.
        assert!(
            check["example_urls"].as_array().unwrap().len() <= 3,
            "{check}"
        );
    }
    assert!(
        failing
            .iter()
            .any(|c| !c["example_urls"].as_array().unwrap().is_empty()),
        "{done}"
    );
    // Most severe first.
    let severities: Vec<&str> = failing
        .iter()
        .map(|c| c["severity"].as_str().unwrap())
        .collect();
    let rank = |s: &str| {
        ["critical", "warning", "notice"]
            .iter()
            .position(|x| *x == s)
            .unwrap()
    };
    assert!(
        severities.windows(2).all(|w| rank(w[0]) <= rank(w[1])),
        "{severities:?}"
    );

    // The linked report is the public one.
    let report = app.get(&format!("/audit/{id}"), None).await;
    assert_eq!(report.status, StatusCode::OK);

    // Asking again for the same site returns the finished report at once: same audit, no new crawl.
    let state = client
        .call_ok("quick_audit", json!({"url": "https://example.com/"}))
        .await;
    assert_eq!(state["status"], "done");
    assert_eq!(state["audit_id"], id.to_string());
    assert_eq!(count(&app, "SELECT count(*) FROM crawls").await, 1);
}

#[tokio::test]
async fn quick_audit_returns_the_summary_in_one_call_when_the_crawl_finishes_while_it_waits() {
    let app = cloud().await;
    let client = Client::new(&app, Duration::from_secs(20), Some(SHARED_UA));
    let call = client.call("quick_audit", json!({"url": "example.com"}));
    let finisher = async {
        // The worker picks the audit up and finishes it a moment after the call started.
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let id: Option<Uuid> =
                sqlx::query_scalar("SELECT id FROM crawls WHERE status = 'queued'")
                    .fetch_optional(app.pool())
                    .await
                    .unwrap();
            if let Some(id) = id {
                finish(&app, id, "example.com").await;
                return;
            }
        }
    };
    let ((is_error, text), ()) = tokio::join!(call, finisher);
    assert!(!is_error, "{text}");
    let done: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(done["status"], "done", "{text}");
    assert!(done["failing_checks"].as_array().unwrap().len() >= 3);
}

#[tokio::test]
async fn an_audit_that_cannot_produce_a_report_says_why() {
    let app = cloud().await;
    let client = Client::new(&app, Duration::ZERO, Some(SHARED_UA));
    let id = client.audit_id("blocked.example.com").await;
    sqlx::query(
        "UPDATE crawls SET status = 'failed', finished_at = now(), failure_reason = $2 \
         WHERE id = $1",
    )
    .bind(id)
    .bind(format!(
        "{} no answer",
        codoseo_core::output::UNREACHABLE_REASON_PREFIX
    ))
    .execute(app.pool())
    .await
    .unwrap();
    let state = client
        .call_ok("get_audit", json!({"audit_id": id.to_string()}))
        .await;
    assert_eq!(state["status"], "failed");
    assert!(
        state["reason"].as_str().unwrap().contains("couldn't reach"),
        "{state}"
    );
    // The pages of an audit with no report are not listed.
    let err = client
        .call_err(
            "get_issue_urls",
            json!({"audit_id": id.to_string(), "check": "title_missing"}),
        )
        .await;
    assert!(err.contains("without a report"), "{err}");
}

// ---- the limits ----

#[tokio::test]
async fn the_same_domain_within_24_hours_reuses_the_audit_and_spends_no_budget() {
    let app = TestApp::with_config(cloud_config_with(&[("MCP_ANON_DAILY_AUDITS", "1")])).await;
    let client = Client::new(&app, Duration::ZERO, Some(SHARED_UA));
    let first = client.audit_id("one.example.com").await;

    // Running: joined. Finished: cached. Neither is a new crawl nor spends the one slot.
    assert_eq!(client.audit_id("https://one.example.com/").await, first);
    finish(&app, first, "one.example.com").await;
    assert_eq!(client.audit_id("one.example.com").await, first);
    assert_eq!(count(&app, "SELECT count(*) FROM crawls").await, 1);

    // A different domain is a fresh audit, and the budget of 1 is spent.
    let err = client
        .call_err("quick_audit", json!({"url": "two.example.com"}))
        .await;
    assert!(err.contains("used up"), "{err}");
    assert_eq!(count(&app, "SELECT count(*) FROM crawls").await, 1);
    // Old audits are reusable still, and after 24 hours the budget comes back.
    assert_eq!(client.audit_id("one.example.com").await, first);
    sqlx::query("UPDATE crawls SET created_at = now() - interval '25 hours'")
        .execute(app.pool())
        .await
        .unwrap();
    assert_ne!(client.audit_id("two.example.com").await, first);
}

#[tokio::test]
async fn the_agent_budget_is_the_configured_daily_count_of_fresh_audits() {
    let app = TestApp::with_config(cloud_config_with(&[("MCP_ANON_DAILY_AUDITS", "3")])).await;
    let client = Client::new(&app, Duration::ZERO, Some(SHARED_UA));
    for n in 0..3 {
        client.audit_id(&format!("site{n}.example.com")).await;
    }
    let err = client
        .call_err("quick_audit", json!({"url": "site3.example.com"}))
        .await;
    assert!(err.contains("used up"), "{err}");
    // The website's own audits are not agents' and aren't refused by their budget.
    let res = app.post("/audit", "url=website.example.com", None).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
}

#[tokio::test]
async fn twenty_concurrent_audits_with_five_slots_left_start_exactly_five() {
    let app = TestApp::with_config(cloud_config_with(&[("MCP_ANON_DAILY_AUDITS", "200")])).await;
    for n in 0..195 {
        sqlx::query(
            "WITH s AS (INSERT INTO sites (domain, start_url) VALUES ($1, $2) RETURNING id) \
             INSERT INTO crawls (site_id, domain, trigger, priority, source, status, finished_at) \
             SELECT id, $1, 'quick', 0, 'agent', 'done', now() FROM s",
        )
        .bind(format!("old{n}.example.com"))
        .bind(format!("https://old{n}.example.com/"))
        .execute(app.pool())
        .await
        .unwrap();
    }
    let client = Client::new(&app, Duration::ZERO, Some(SHARED_UA));
    let domains: Vec<String> = (0..20).map(|n| format!("new{n}.example.com")).collect();
    let results = futures_util::future::join_all(
        domains
            .iter()
            .map(|d| client.call("quick_audit", json!({"url": d}))),
    )
    .await;
    let ok = results.iter().filter(|(is_error, _)| !is_error).count();
    assert_eq!(ok, 5, "{results:?}");
    assert_eq!(
        count(&app, "SELECT count(*) FROM crawls WHERE source = 'agent'").await,
        200
    );
}

#[tokio::test]
async fn a_direct_client_shares_the_per_ip_limits_with_the_website_and_a_connector_does_not() {
    let app = cloud().await;
    let ip = "203.0.113.7";

    // Three audits from the website, from this address.
    for n in 0..3 {
        let res = app
            .post_with_headers(
                "/audit",
                &format!("url=web{n}.example.com"),
                None,
                &[("cf-connecting-ip", ip)],
            )
            .await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    }
    // Claude Code from the same address is over the hourly limit...
    let direct = Client::new(&app, Duration::ZERO, Some(DIRECT_UA)).with_ip(ip);
    let err = direct
        .call_err("quick_audit", json!({"url": "agent.example.com"}))
        .await;
    assert!(err.contains("3 new audits an hour"), "{err}");
    // ...but a site audited recently is still handed back (it costs nothing)...
    assert!(Uuid::parse_str(&direct.audit_id("web0.example.com").await.to_string()).is_ok());
    // ...a different address has its own allowance...
    let other = Client::new(&app, Duration::ZERO, Some(DIRECT_UA)).with_ip("203.0.113.8");
    other.audit_id("agent.example.com").await;
    // ...and a hosted connector (shared servers) isn't limited per address at all.
    let shared = Client::new(&app, Duration::ZERO, Some(SHARED_UA)).with_ip(ip);
    for n in 0..5 {
        shared.audit_id(&format!("shared{n}.example.com")).await;
    }
    // The direct client's own audits are counted by its address hash, not stored in the clear.
    let hashed: i64 = count(
        &app,
        "SELECT count(*) FROM crawls WHERE source = 'agent' AND requester_ip_hash IS NOT NULL",
    )
    .await;
    assert_eq!(hashed, 1);
}

#[tokio::test]
async fn private_and_malformed_targets_are_refused() {
    let app = cloud().await;
    let client = Client::new(&app, Duration::ZERO, Some(SHARED_UA));
    for bad in [
        "http://localhost/",
        "http://127.0.0.1/",
        "http://10.1.2.3/",
        "http://192.168.0.1/",
        "http://169.254.169.254/latest/meta-data",
        "http://[::1]/",
        "http://nas.local/",
        "http://db.internal/",
        "ftp://example.com",
        "not a url",
        "",
    ] {
        let err = client.call_err("quick_audit", json!({"url": bad})).await;
        assert!(err.len() > 20, "{bad}: {err}");
        let again = client
            .call_err(
                "start_monitoring",
                json!({"url": bad, "email": "a@example.org"}),
            )
            .await;
        assert_eq!(err, again, "{bad}");
    }
    assert_eq!(count(&app, "SELECT count(*) FROM crawls").await, 0);
    assert_eq!(count(&app, "SELECT count(*) FROM login_tokens").await, 0);
    assert!(app.mail.lock().unwrap().is_empty());
}

// ---- get_issue_urls ----

#[tokio::test]
async fn get_issue_urls_pages_through_a_finished_quick_audit() {
    let app = cloud().await;
    let client = Client::new(&app, Duration::ZERO, Some(SHARED_UA));
    let id = client.audit_id("example.com").await;
    let args = |extra: Value| {
        let mut base = json!({"audit_id": id.to_string(), "check": "title_missing"});
        base.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        base
    };

    // Not finished yet.
    let err = client.call_err("get_issue_urls", args(json!({}))).await;
    assert!(err.contains("not finished"), "{err}");

    finish(&app, id, "example.com").await;
    let page = client
        .call_ok("get_issue_urls", args(json!({"limit": 3})))
        .await;
    assert_eq!(page["audit_id"], id.to_string());
    assert_eq!(page["check"], "title_missing");
    assert_eq!(page["total"], 4);
    assert_eq!(page["limit"], 3);
    assert_eq!(page["urls"].as_array().unwrap().len(), 3);
    assert_eq!(page["next_offset"], 3);
    let rest = client
        .call_ok("get_issue_urls", args(json!({"limit": 3, "offset": 3})))
        .await;
    assert_eq!(rest["urls"].as_array().unwrap().len(), 1);
    assert_eq!(rest["next_offset"], Value::Null);
    assert!(
        rest["urls"][0]["url"]
            .as_str()
            .unwrap()
            .starts_with("https://example.com/messy-")
    );

    // An unknown check lists the valid ones.
    let err = client
        .call_err(
            "get_issue_urls",
            json!({"audit_id": id.to_string(), "check": "nope"}),
        )
        .await;
    assert!(
        err.contains("title_missing") && err.contains("nope"),
        "{err}"
    );
}

#[tokio::test]
async fn only_quick_audits_can_be_read_by_id() {
    let app = cloud().await;
    let (account, _) = app
        .login_with_plan("owner@example.com", Some(Plan::Pro))
        .await;
    let site = app.site(&account, "private.example.com").await;
    let keyed_crawl = app
        .finished_crawl(&site, messy_pages("private.example.com"), Vec::new())
        .await;
    let client = Client::new(&app, Duration::ZERO, Some(SHARED_UA));

    let unknown = Uuid::new_v4().to_string();
    let mut messages = Vec::new();
    for id in [
        keyed_crawl.to_string(),
        unknown,
        "garbage".to_owned(),
        site.id.to_string(),
    ] {
        messages.push(client.call_err("get_audit", json!({"audit_id": id})).await);
        messages.push(
            client
                .call_err(
                    "get_issue_urls",
                    json!({"audit_id": id, "check": "title_missing"}),
                )
                .await,
        );
    }
    // A site's crawl, a site's id, an unknown id and nonsense all read the same.
    assert!(messages.iter().all(|m| m == &messages[0]), "{messages:?}");
    assert!(messages[0].contains("No such audit"));
    // And nothing from the site leaked.
    assert!(!messages.iter().any(|m| m.contains("private.example.com")));
}
