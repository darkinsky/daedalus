pub mod cli_args;
pub(crate) mod commands;
mod completer;
pub(crate) mod cost;
mod output_format;
mod print_runner;
// The `render` module lives in `render/mod.rs` (standard directory layout).
// `#[path]` is used to disambiguate from the legacy `render.rs` file which
// is kept empty until the next cleanup pass removes it.
#[path = "render/mod.rs"]
mod render;
mod repl;

use std::process::ExitCode;

use anyhow::Result;

use crate::agent::AgentMode;

pub use cli_args::{CliArgs, CliPromptStyle, OutputFormat};

/// Run an interactive REPL loop in Claude Code style.
///
/// This is the main entry point for the default (no `--print`) mode.
pub async fn run_interactive(agent: &mut dyn AgentMode) -> Result<()> {
    repl::run(agent).await
}

/// Run a single prompt in non-interactive (print) mode.
///
/// This is the main entry point for `--print` / `-p` mode.
/// Returns an exit code: SUCCESS (0) or FAILURE (1).
pub async fn run_print(
    agent: &mut dyn AgentMode,
    prompt: &str,
    format: &OutputFormat,
) -> Result<ExitCode> {
    print_runner::run(agent, prompt, format).await
}

/// Read a prompt from stdin (for `-p -`).
pub fn read_stdin_prompt() -> Result<String> {
    print_runner::read_stdin_prompt()
}
