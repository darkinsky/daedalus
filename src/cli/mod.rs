pub mod cli_args;
pub(crate) mod commands;
mod completer;
pub(crate) mod context_analysis;
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
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;

use crate::agent::AgentMode;

pub use cli_args::{CliArgs, CliPromptStyle, OutputFormat};

// ── Verbose output mode ──

/// Global flag controlling CLI output verbosity.
///
/// When `true`, tool events are rendered in verbose mode (multi-line with
/// full argument details, expanded thinking, diff previews). When `false`
/// (default), output uses compact inline-refresh mode.
///
/// Set once during bootstrap via `set_verbose()`. Also respects the
/// `DAEDALUS_VERBOSE` environment variable.
static VERBOSE_OUTPUT: AtomicBool = AtomicBool::new(false);

/// Enable or disable verbose CLI output.
///
/// Called during bootstrap based on `--verbose` flag or `DAEDALUS_VERBOSE` env.
pub fn set_verbose(verbose: bool) {
    VERBOSE_OUTPUT.store(verbose, Ordering::Relaxed);
}

/// Check if verbose CLI output is enabled.
pub(crate) fn is_verbose() -> bool {
    VERBOSE_OUTPUT.load(Ordering::Relaxed)
}

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
///
/// If `timeout_secs` is provided, the agent execution is bounded by that
/// duration. On timeout, an error result is emitted and FAILURE is returned.
pub async fn run_print(
    agent: &mut dyn AgentMode,
    prompt: &str,
    format: &OutputFormat,
    timeout_secs: Option<u64>,
) -> Result<ExitCode> {
    print_runner::run(agent, prompt, format, timeout_secs).await
}

/// Read a prompt from stdin (for `-p -`).
pub fn read_stdin_prompt() -> Result<String> {
    print_runner::read_stdin_prompt()
}
