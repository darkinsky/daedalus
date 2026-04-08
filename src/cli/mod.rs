mod commands;
mod cost;
mod render;
mod repl;

use anyhow::Result;

use crate::agent::AgentMode;

/// Run an interactive REPL loop in Claude Code style.
///
/// This is the main entry point for the CLI module.
pub async fn run_interactive(agent: &mut dyn AgentMode) -> Result<()> {
    repl::run(agent).await
}
