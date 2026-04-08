mod agent;
mod cli;
mod config;
mod llm;
pub mod logging;
mod mcp;
mod memory;
mod prompt;
mod session;

use agent::AgentMode;
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging — hold the guard to keep file writer alive
    let log_config = logging::LogConfig::from_env();
    let _log_guard = logging::init(&log_config)?;

    tracing::info!("Daedalus Agent starting...");

    // Load configuration
    let config = config::AgentConfig::from_env()?;
    tracing::info!("Using model: {}", config.model());
    if let Some(base_url) = config.api_base() {
        tracing::info!("Using API base URL: {}", base_url);
    }

    // Load MCP configuration and connect to servers
    let mcp_config = mcp::McpConfig::load()?;
    let mcp_manager = if !mcp_config.servers.is_empty() {
        tracing::info!(
            servers = mcp_config.servers.len(),
            "Connecting to MCP servers..."
        );
        let manager = mcp::McpManager::from_config(&mcp_config).await;
        tracing::info!(
            tools = manager.tool_count(),
            "MCP initialization complete"
        );
        Some(manager)
    } else {
        None
    };

    // Create the LLM provider (GenAI supports both plain chat and tool calling)
    let provider = llm::create_provider(config.llm.clone())?;

    // Build the chat agent and optionally attach MCP
    let mut agent = agent::ChatAgent::new(provider, &config);
    if let Some(manager) = mcp_manager {
        agent.attach_mcp(manager);
    }

    // Run the interactive CLI
    cli::run_interactive(&mut agent).await?;

    Ok(())
}
