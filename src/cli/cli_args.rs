//! CLI argument parsing using `clap` derive.
//!
//! Defines the command-line interface for Daedalus, supporting both
//! interactive (REPL) and non-interactive (print) modes.

use clap::Parser;

/// Daedalus — A blazing-fast terminal AI assistant built in Rust.
///
/// Run without arguments to start an interactive REPL session.
/// Use `-p` / `--print` to run a single prompt and exit.
#[derive(Parser, Debug)]
#[command(name = "daedalus", version, about, long_about = None)]
pub struct CliArgs {
    /// Run in non-interactive mode: execute a single prompt and exit.
    ///
    /// Pass "-" to read the prompt from stdin.
    /// When combined with `--output-format`, controls how the result is emitted.
    #[arg(short = 'p', long = "print", value_name = "PROMPT")]
    pub print: Option<String>,

    /// Output format (only effective in `--print` mode).
    ///
    /// - "text": plain text to stdout (default)
    /// - "json": single JSON object after completion
    /// - "stream-json": NDJSON event stream in real-time
    #[arg(long = "output-format", default_value = "text", value_name = "FORMAT")]
    pub output_format: OutputFormat,

    /// Maximum number of tool-calling rounds per prompt.
    ///
    /// Limits the agentic loop depth. 0 means use the internal default (200).
    #[arg(long = "max-turns", value_name = "N")]
    pub max_turns: Option<usize>,

    /// Override the system prompt entirely.
    #[arg(long = "system-prompt", value_name = "PROMPT")]
    pub system_prompt: Option<String>,

    /// Append text to the end of the system prompt.
    #[arg(long = "append-system-prompt", value_name = "TEXT")]
    pub append_system_prompt: Option<String>,

    /// Override the model identifier.
    #[arg(long = "model", value_name = "MODEL")]
    pub model: Option<String>,

    /// Allowed tools (comma-separated). Only these tools will be available.
    ///
    /// Example: --allowed-tools "read_file,write_file,bash"
    #[arg(long = "allowed-tools", value_name = "TOOLS")]
    pub allowed_tools: Option<String>,

    /// Disallowed tools (comma-separated). These tools will be blocked.
    ///
    /// Example: --disallowed-tools "bash,write_file"
    #[arg(long = "disallowed-tools", value_name = "TOOLS")]
    pub disallowed_tools: Option<String>,

    /// Continue the most recent session.
    #[arg(short = 'c', long = "continue")]
    pub continue_session: bool,

    /// Resume a specific session by ID.
    #[arg(short = 'r', long = "resume", value_name = "SESSION_ID")]
    pub resume_session: Option<String>,

    /// Enable verbose logging to stderr.
    #[arg(short = 'v', long = "verbose")]
    pub verbose: bool,

    /// Skip all permission checks (only use in sandboxed/CI environments).
    #[arg(long = "dangerously-skip-permissions")]
    pub skip_permissions: bool,

    /// Auto-approve all tool calls (alias for --dangerously-skip-permissions).
    ///
    /// Friendlier name for CI/CD pipelines. Equivalent to --dangerously-skip-permissions.
    #[arg(long = "auto-approve")]
    pub auto_approve: bool,

    /// Total timeout in seconds for non-interactive (print) mode.
    ///
    /// If the agent does not complete within this time, it is terminated
    /// and a timeout error is returned. Only effective in --print mode.
    #[arg(long = "timeout", value_name = "SECONDS")]
    pub timeout: Option<u64>,

    /// Bare mode: skip auto-discovery of skills, subagents, and MCP servers.
    ///
    /// Significantly reduces startup time for simple queries.
    #[arg(long = "bare")]
    pub bare: bool,

    /// Prompt assembly style: "default" or "coding".
    ///
    /// - "default": Original Daedalus prompt (generic AI assistant)
    /// - "coding": Coding-focused prompt (autonomous coding agent)
    #[arg(long = "prompt-style", value_name = "STYLE")]
    pub prompt_style: Option<CliPromptStyle>,
}

/// Prompt style for CLI argument parsing.
#[derive(Debug, Clone, clap::ValueEnum)]
pub enum CliPromptStyle {
    /// Original Daedalus prompt.
    Default,
    /// Coding-focused prompt.
    Coding,
}

/// Output format for non-interactive (print) mode.
#[derive(Debug, Clone, Default, clap::ValueEnum)]
pub enum OutputFormat {
    /// Plain text output to stdout (default).
    #[default]
    Text,
    /// Single JSON object after completion.
    Json,
    /// NDJSON event stream (one JSON object per line, real-time).
    StreamJson,
}

impl CliArgs {
    /// Return true if the user requested non-interactive (print) mode.
    pub fn is_print_mode(&self) -> bool {
        self.print.is_some()
    }

    /// Return true if permissions should be skipped.
    ///
    /// Combines `--dangerously-skip-permissions` and `--auto-approve` flags.
    pub fn should_skip_permissions(&self) -> bool {
        self.skip_permissions || self.auto_approve
    }

    /// Parse the allowed tools list into a Vec of tool names.
    pub fn allowed_tools_list(&self) -> Option<Vec<String>> {
        self.allowed_tools.as_ref().map(|s| {
            s.split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect()
        })
    }

    /// Parse the disallowed tools list into a Vec of tool names.
    pub fn disallowed_tools_list(&self) -> Option<Vec<String>> {
        self.disallowed_tools.as_ref().map(|s| {
            s.split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allowed_tools_list_parsing() {
        let args = CliArgs {
            print: None,
            output_format: OutputFormat::Text,
            max_turns: None,
            system_prompt: None,
            append_system_prompt: None,
            model: None,
            allowed_tools: Some("read_file, write_file, bash".to_string()),
            disallowed_tools: None,
            continue_session: false,
            resume_session: None,
            verbose: false,
            skip_permissions: false,
            auto_approve: false,
            timeout: None,
            bare: false,
            prompt_style: None,
        };
        let tools = args.allowed_tools_list().unwrap();
        assert_eq!(tools, vec!["read_file", "write_file", "bash"]);
    }

    #[test]
    fn test_disallowed_tools_list_parsing() {
        let args = CliArgs {
            print: None,
            output_format: OutputFormat::Text,
            max_turns: None,
            system_prompt: None,
            append_system_prompt: None,
            model: None,
            allowed_tools: None,
            disallowed_tools: Some("bash,write_file".to_string()),
            continue_session: false,
            resume_session: None,
            verbose: false,
            skip_permissions: false,
            auto_approve: false,
            timeout: None,
            bare: false,
            prompt_style: None,
        };
        let tools = args.disallowed_tools_list().unwrap();
        assert_eq!(tools, vec!["bash", "write_file"]);
    }

    #[test]
    fn test_tools_list_none_when_not_set() {
        let args = CliArgs {
            print: None,
            output_format: OutputFormat::Text,
            max_turns: None,
            system_prompt: None,
            append_system_prompt: None,
            model: None,
            allowed_tools: None,
            disallowed_tools: None,
            continue_session: false,
            resume_session: None,
            verbose: false,
            skip_permissions: false,
            auto_approve: false,
            timeout: None,
            bare: false,
            prompt_style: None,
        };
        assert!(args.allowed_tools_list().is_none());
        assert!(args.disallowed_tools_list().is_none());
    }

    #[test]
    fn test_is_print_mode() {
        let mut args = CliArgs {
            print: None,
            output_format: OutputFormat::Text,
            max_turns: None,
            system_prompt: None,
            append_system_prompt: None,
            model: None,
            allowed_tools: None,
            disallowed_tools: None,
            continue_session: false,
            resume_session: None,
            verbose: false,
            skip_permissions: false,
            auto_approve: false,
            timeout: None,
            bare: false,
            prompt_style: None,
        };
        assert!(!args.is_print_mode());

        args.print = Some("Hello".to_string());
        assert!(args.is_print_mode());
    }

    #[test]
    fn test_empty_tools_list_filtered() {
        let args = CliArgs {
            print: None,
            output_format: OutputFormat::Text,
            max_turns: None,
            system_prompt: None,
            append_system_prompt: None,
            model: None,
            allowed_tools: Some(",,,".to_string()),
            disallowed_tools: None,
            continue_session: false,
            resume_session: None,
            verbose: false,
            skip_permissions: false,
            auto_approve: false,
            timeout: None,
            bare: false,
            prompt_style: None,
        };
        let tools = args.allowed_tools_list().unwrap();
        assert!(tools.is_empty());
    }

    #[test]
    fn test_should_skip_permissions() {
        let mut args = CliArgs {
            print: None,
            output_format: OutputFormat::Text,
            max_turns: None,
            system_prompt: None,
            append_system_prompt: None,
            model: None,
            allowed_tools: None,
            disallowed_tools: None,
            continue_session: false,
            resume_session: None,
            verbose: false,
            skip_permissions: false,
            auto_approve: false,
            timeout: None,
            bare: false,
            prompt_style: None,
        };
        assert!(!args.should_skip_permissions());

        args.auto_approve = true;
        assert!(args.should_skip_permissions());

        args.auto_approve = false;
        args.skip_permissions = true;
        assert!(args.should_skip_permissions());
    }
}
