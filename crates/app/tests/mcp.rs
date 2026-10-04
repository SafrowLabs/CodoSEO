//! `codoseo mcp` end to end: a raw JSON-RPC handshake against the real binary's
//! stdin/stdout, piped (not a duplex in-process transport - the local tool tests in
//! `codoseo-mcp` already cover tool behaviour in-process).

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};

use assert_cmd::cargo::cargo_bin;
use serde_json::{Value, json};

struct McpChild {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl McpChild {
    fn start() -> McpChild {
        let mut child = Command::new(cargo_bin("codoseo"))
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn `codoseo mcp`");
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        McpChild {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, message: &Value) {
        let line = serde_json::to_string(message).unwrap();
        writeln!(self.stdin, "{line}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// Reads lines until one parses as JSON with this exact `id`, skipping any
    /// notifications in between.
    fn response_for_id(&mut self, id: u64) -> Value {
        for _ in 0..50 {
            let mut line = String::new();
            self.stdout
                .read_line(&mut line)
                .expect("read a line before EOF");
            if line.trim().is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(&line).expect("a JSON-RPC line");
            if value.get("id") == Some(&Value::from(id)) {
                return value;
            }
        }
        panic!("no response for id {id} within 50 lines");
    }
}

impl Drop for McpChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn tools_list_shows_all_8_tools_over_real_stdio() {
    let mut mcp = McpChild::start();

    mcp.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "codoseo-cli-test", "version": "0.0.1" }
        }
    }));
    mcp.response_for_id(1);
    mcp.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));

    mcp.send(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    let response = mcp.response_for_id(2);

    let tools = response["result"]["tools"]
        .as_array()
        .expect("a tools array");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    for expected in [
        "audit_site",
        "get_audit",
        "get_issue_urls",
        "get_page",
        "check_page",
        "check_robots",
        "check_redirects",
        "compare_audits",
    ] {
        assert!(
            names.contains(&expected),
            "missing tool {expected} in {names:?}"
        );
    }
    assert_eq!(tools.len(), 8);
}
