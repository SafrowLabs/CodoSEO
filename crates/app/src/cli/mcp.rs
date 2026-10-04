//! `codoseo mcp`: the local MCP server over stdio. `claude mcp add codoseo -- codoseo mcp`
//! registers it in Claude Code.

use clap::Args;
use codoseo_mcp::cache::AuditCache;
use codoseo_mcp::{CodoseoMcp, LocalBackend};
use rmcp::ServiceExt;

use super::{CliError, EXIT_OK, Outcome};

#[derive(Debug, Args)]
pub struct McpArgs {}

fn cache_dir() -> Result<std::path::PathBuf, CliError> {
    Ok(dirs::cache_dir()
        .ok_or_else(|| CliError::msg("could not find a user cache directory"))?
        .join("codoseo")
        .join("audits"))
}

pub async fn run(_args: McpArgs) -> Outcome {
    let backend = LocalBackend::new(AuditCache::new(cache_dir()?));
    let server = CodoseoMcp::new(backend);
    let running = server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| CliError::msg(format!("could not start the MCP server: {e}")))?;
    running
        .waiting()
        .await
        .map_err(|e| CliError::msg(format!("MCP server error: {e}")))?;
    Ok(EXIT_OK)
}
