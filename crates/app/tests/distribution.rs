//! The agent distribution files at the repository root stay valid and in step with the release:
//! the MCP Registry `server.json` against the registry's published schema, the Claude Code plugin
//! and marketplace manifests, and the `codoseo-audit` Agent Skill against the Agent Skills rules.
//! Formats as documented on 2026-10-13 (sources in docs/agents.md).

use std::path::{Path, PathBuf};

use serde_json::Value;

/// The MCP Registry's server.json schema, version 2025-12-11, from
/// https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json. Vendored so the
/// test runs offline.
const SERVER_SCHEMA: &str = include_str!("data/mcp-server.schema.2025-12-11.json");

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_json(path: &str) -> Value {
    let text = std::fs::read_to_string(root().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[test]
fn server_json_matches_the_registry_schema_and_the_release() {
    let server = read_json("server.json");
    let schema: Value = serde_json::from_str(SERVER_SCHEMA).expect("schema");
    let validator = jsonschema::validator_for(&schema).expect("a valid schema");
    let errors: Vec<String> = validator
        .iter_errors(&server)
        .map(|e| format!("{e} at {}", e.instance_path()))
        .collect();
    assert!(errors.is_empty(), "server.json: {errors:#?}");
    assert_eq!(
        server["$schema"],
        "https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json"
    );

    // The registry's own rules beyond the schema: a description of at most 100 characters, an
    // exact package version, and the npm package naming the server back in `mcpName`.
    assert!(server["description"].as_str().unwrap().chars().count() <= 100);
    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(server["version"], version);
    let npm = read_json("npm/package.json");
    let package = &server["packages"][0];
    assert_eq!(package["registryType"], "npm");
    assert_eq!(package["identifier"], npm["name"]);
    assert_eq!(package["version"], npm["version"]);
    assert_eq!(npm["version"], version);
    assert_eq!(npm["mcpName"], server["name"]);
    assert_eq!(package["packageArguments"][0]["value"], "mcp");
    assert_eq!(server["remotes"][0]["url"], "https://codoseo.com/mcp");
}

#[test]
fn the_claude_code_plugin_and_marketplace_are_in_step() {
    let plugin = read_json(".claude-plugin/plugin.json");
    let market = read_json(".claude-plugin/marketplace.json");
    assert_eq!(plugin["name"], "codoseo");
    assert_eq!(plugin["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(plugin["license"], "AGPL-3.0-only");
    let server = &plugin["mcpServers"]["codoseo"];
    assert_eq!(server["command"], "npx");
    assert_eq!(server["args"], serde_json::json!(["-y", "codoseo", "mcp"]));
    let entry = &market["plugins"][0];
    assert_eq!(entry["name"], plugin["name"]);
    // The repository root is the plugin: its skills/ directory is the one it ships.
    assert_eq!(entry["source"], ".");
    assert!(root().join("skills/codoseo-audit/SKILL.md").is_file());
}

/// The frontmatter of a SKILL.md as `key: value` pairs.
fn frontmatter(text: &str) -> Vec<(String, String)> {
    let body = text
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .map(|(front, _)| front)
        .expect("SKILL.md starts with a --- frontmatter block");
    body.lines()
        .map(|l| {
            let (k, v) = l
                .split_once(':')
                .unwrap_or_else(|| panic!("not key: value: {l}"));
            (k.trim().to_owned(), v.trim().to_owned())
        })
        .collect()
}

#[test]
fn every_skill_follows_the_agent_skills_rules() {
    let dir = root().join("skills");
    let mut seen = 0;
    for entry in std::fs::read_dir(&dir).expect("skills/") {
        let path = entry.expect("an entry").path();
        let dir_name = path.file_name().unwrap().to_str().unwrap().to_owned();
        let text = std::fs::read_to_string(path.join("SKILL.md")).expect("SKILL.md");
        let front = frontmatter(&text);
        let get = |k: &str| {
            front
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };
        // https://agentskills.io/specification: name is 1-64 of a-z, 0-9 and -, no leading,
        // trailing or doubled hyphen, and the same as its directory.
        let name = get("name").expect("a name");
        assert_eq!(name, dir_name);
        assert!((1..=64).contains(&name.len()));
        assert!(
            name.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        );
        assert!(!name.starts_with('-') && !name.ends_with('-') && !name.contains("--"));
        let description = get("description").expect("a description");
        assert!((1..=1024).contains(&description.chars().count()));
        if let Some(c) = get("compatibility") {
            assert!((1..=500).contains(&c.chars().count()));
        }
        // Only the specification's fields, so any agent that reads skills can load it.
        for (key, _) in &front {
            assert!(
                [
                    "name",
                    "description",
                    "license",
                    "compatibility",
                    "metadata",
                    "allowed-tools"
                ]
                .contains(&key.as_str()),
                "{dir_name}: {key} is not an Agent Skills field"
            );
        }
        assert!(text.lines().count() < 500, "keep SKILL.md under 500 lines");
        seen += 1;
    }
    assert!(seen > 0);
}
