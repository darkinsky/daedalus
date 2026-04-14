mod agent;
mod cli;
mod config;
mod embedding;
mod llm;
mod mcp;
mod memory;
mod prompt;
mod skill;
mod tools;
mod workspace;

use agent::AgentMode;
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // Resolve workspace (zero-config: auto-creates ~/.daedalus/)
    let workspace = workspace::Workspace::resolve()?;

    // Initialize logging — use workspace config file
    let log_config = config::LogConfig::from_workspace(&workspace);
    let _log_guard = config::init_logging(&log_config)?;

    tracing::info!("Daedalus Agent starting...");
    tracing::info!(
        workspace = %workspace.root().display(),
        kind = %workspace.kind(),
        "Workspace resolved"
    );

    // Load configuration from workspace YAML config file
    let config = config::AgentConfig::from_workspace(&workspace)?;
    tracing::info!("Using model: {}", config.model());
    if let Some(base_url) = config.api_base() {
        tracing::info!("Using API base URL: {}", base_url);
    }

    // Load MCP configuration (workspace-aware search chain)
    let mcp_config = mcp::McpConfig::load_with_workspace(&workspace)?;
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

    // Create the LLM provider
    let provider = llm::create_provider(config.llm.clone())?;

    // Build the chat agent with workspace-aware memory persistence
    let mut agent = agent::ChatAgent::new_with_workspace(provider, &config, workspace.clone());
    if let Some(manager) = mcp_manager {
        agent.attach_mcp(manager);
    }

    // Load skills from workspace skills directory
    let ws_skills_dir = workspace.skills_dir();
    load_skills(&mut agent, &ws_skills_dir);

    // Also load skills from cwd/skills/ if different from workspace
    let cwd_skills_dir = std::env::current_dir()
        .unwrap_or_default()
        .join("skills");
    if cwd_skills_dir != ws_skills_dir {
        load_skills(&mut agent, &cwd_skills_dir);
    }

    // Run the interactive CLI (shutdown is called inside on exit)
    cli::run_interactive(&mut agent).await?;

    Ok(())
}

/// Load skills from a directory, logging the result.
fn load_skills(agent: &mut agent::ChatAgent, dir: &std::path::Path) {
    match agent.load_skills(dir) {
        Ok(count) if count > 0 => {
            tracing::info!(
                skills = count,
                path = %dir.display(),
                "Skills loaded successfully"
            );
        }
        Ok(_) => {
            tracing::debug!(
                path = %dir.display(),
                "No skills found in directory"
            );
        }
        Err(e) => {
            tracing::warn!(
                path = %dir.display(),
                error = %e,
                "Failed to load skills, continuing without skills"
            );
        }
    }
}
