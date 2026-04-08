mod agent;
mod cli;
mod config;
mod llm;
pub mod logging;
mod memory;
mod session;

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

    // Create the LLM provider (LlmConfig is embedded in AgentConfig)
    let provider = llm::create_provider(config.llm.clone())?;

    // Build the chat agent and run the interactive CLI
    let mut agent = agent::ChatAgent::new(provider, &config);
    cli::run_interactive(&mut agent).await?;

    Ok(())
}
