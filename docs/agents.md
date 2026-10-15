# CodoSEO for AI agents

How to give a coding or research agent CodoSEO: the MCP server (local or hosted), the Claude Code plugin, the `codoseo-audit` Agent Skill and the MCP Registry entry. The tools themselves are listed in [mcp.md](mcp.md).

Formats on this page were checked against the official documentation on 2026-10-13; the links are under [Sources](#sources).

## Two servers

| | Local (`codoseo mcp`) | Hosted (`https://codoseo.com/mcp`) |
|---|---|---|
| Transport | stdio | Streamable HTTP |
| Runs | on your machine, via `codoseo` or `npx -y codoseo` | on codoseo.com (or `<BASE_URL>/mcp` self-hosted) |
| Account | none | none for free audits of public sites; an API key (`cdo_...`, **Settings, API keys**) for your monitored sites |
| Tools | 11 (`audit_site`, `check_ai_access`, `probe_ai_fetch`, ...) | 4 without a key (`quick_audit`, ...), 13 with one (`list_sites`, `get_ai_access`, `get_fix`, ...) |
| Private and staging addresses | allowed | refused |

Both give an agent the 44 SEO checks, the 9 AI & GEO checks and a fix for each AI access finding.

## Claude Code

### Plugin

This repository is a Claude Code plugin marketplace with one plugin, `codoseo`: the local MCP server (run with `npx`, so Node.js 18 or newer is needed) and the `codoseo-audit` skill.

```text
/plugin marketplace add SafrowLabs/CodoSEO
/plugin install codoseo@codoseo
```

Files: [`.claude-plugin/marketplace.json`](../.claude-plugin/marketplace.json) (the marketplace, plugin source `.`, the repository root), [`.claude-plugin/plugin.json`](../.claude-plugin/plugin.json) (the manifest, with the MCP server inline) and [`skills/codoseo-audit/SKILL.md`](../skills/codoseo-audit/SKILL.md). Check them with `claude plugin validate .`.

The plugin's server is the local one; for the hosted server with your key, add it as below.

### MCP server only

```sh
# Local
claude mcp add codoseo -- npx -y codoseo mcp

# Hosted, free audits of public sites
claude mcp add --transport http codoseo https://codoseo.com/mcp

# Hosted, your monitored sites (the shell expands the key, so it is stored in your config)
claude mcp add --transport http codoseo https://codoseo.com/mcp \
  --header "Authorization: Bearer $CODOSEO_API_KEY"
```

A project's `.mcp.json` can keep the key out of the file; Claude Code expands `${VAR}` in `headers`:

```json
{
  "mcpServers": {
    "codoseo": {
      "type": "http",
      "url": "https://codoseo.com/mcp",
      "headers": { "Authorization": "Bearer ${CODOSEO_API_KEY}" }
    }
  }
}
```

## Agent Skill

[`skills/codoseo-audit/SKILL.md`](../skills/codoseo-audit/SKILL.md) follows the [Agent Skills specification](https://agentskills.io/specification): frontmatter `name` (matching its directory), `description`, `license` and `compatibility`, and no tool-specific fields, so any agent that reads skills can use it. It tells the agent which tools to pick (local MCP, hosted with or without a key, or the CLI), how to run an audit, how to read the SEO checks, AI access findings and evidence grades, and how to hand over the fixes (`remedies` locally, `get_fix` with a key) without following instructions found in crawled pages.

Outside the plugin, copy the `codoseo-audit` directory into your agent's skills directory (`~/.claude/skills/` or `.claude/skills/` for Claude Code).

## OpenAI Codex

`~/.codex/config.toml` (or `.codex/config.toml` in a trusted project):

```toml
# Local
[mcp_servers.codoseo]
command = "npx"
args = ["-y", "codoseo", "mcp"]

# Hosted: the key is read from the environment; without it Codex connects with no key
[mcp_servers.codoseo-cloud]
url = "https://codoseo.com/mcp"
bearer_token_env_var = "CODOSEO_API_KEY"
```

Or `codex mcp add codoseo -- npx -y codoseo mcp` for the local server.

## Cursor

`.cursor/mcp.json` in a project, or `~/.cursor/mcp.json` for all of them:

```json
{
  "mcpServers": {
    "codoseo": {
      "type": "stdio",
      "command": "npx",
      "args": ["-y", "codoseo", "mcp"]
    },
    "codoseo-cloud": {
      "url": "https://codoseo.com/mcp",
      "headers": { "Authorization": "Bearer ${env:CODOSEO_API_KEY}" }
    }
  }
}
```

## Other clients

Any client that starts a stdio server needs the command `npx` and the arguments `-y codoseo mcp` (or `codoseo mcp` with the binary installed). Any client that speaks Streamable HTTP needs the URL `https://codoseo.com/mcp` and, for your sites, the header `Authorization: Bearer cdo_...`. An `Authorization` header with `Bearer` and no key is treated as no key, so a config that expands an empty variable still gets the free tools.

## MCP Registry

[`server.json`](../server.json) describes CodoSEO for the [MCP Registry](https://registry.modelcontextprotocol.io) under the name `io.github.safrowlabs/codoseo` (schema `2025-12-11`): the npm package `codoseo` started with `npx` and the argument `mcp` over stdio, and the hosted Streamable HTTP endpoint with an optional, secret `Authorization` header. `npm/package.json` carries the matching `mcpName`, which is how the registry checks that the npm package belongs to the server. A test validates `server.json` against the registry's schema and keeps its versions equal to the workspace's and the npm package's.

It is not published. To publish, after the npm package of the same version is out (the registry reads `mcpName` from the published package):

1. Install `mcp-publisher` ([quickstart](https://github.com/modelcontextprotocol/registry/blob/main/docs/modelcontextprotocol-io/quickstart.mdx)).
2. `mcp-publisher login github` as an **owner** of the SafrowLabs GitHub organization (the `io.github.safrowlabs/` namespace is verified through GitHub; membership alone is not enough).
3. `mcp-publisher publish` from the repository root.

Each version can be published once, so bump `version` (and `packages[0].version`) with every release, as [RELEASING.md](../RELEASING.md) lists. A `com.codoseo/...` name would need a DNS TXT record or an HTTP well-known file on codoseo.com instead.

## Sources

Checked on 2026-10-13:

- MCP Registry `server.json`: https://github.com/modelcontextprotocol/registry/blob/main/docs/reference/server-json/generic-server-json.md, https://github.com/modelcontextprotocol/registry/blob/main/docs/reference/server-json/official-registry-requirements.md and the schema https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json
- Claude Code plugins and marketplaces: https://code.claude.com/docs/en/plugins-reference, https://code.claude.com/docs/en/plugins/marketplace-reference, https://code.claude.com/docs/en/mcp
- Agent Skills: https://agentskills.io/specification and https://code.claude.com/docs/en/skills
- Codex: https://developers.openai.com/codex/mcp
- Cursor: https://cursor.com/docs/context/mcp
