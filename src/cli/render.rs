use crossterm::style::{Attribute, Color, Stylize};
use indicatif::{ProgressBar, ProgressStyle};

use crate::agent::{AgentMetadata, ToolEvent};
use crate::llm::TokenUsage;
use super::commands::SLASH_COMMANDS;
use super::cost::SessionCost;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Maximum number of lines to display for tool output before truncating.
pub(super) const TOOL_OUTPUT_MAX_LINES: usize = 10;
/// Number of lines to show from the beginning when truncating.
pub(super) const TOOL_OUTPUT_HEAD_LINES: usize = 5;
/// Number of lines to show from the end when truncating.
pub(super) const TOOL_OUTPUT_TAIL_LINES: usize = 3;

// ── Shared text utilities ──

/// Truncate a string to at most `max_chars` characters, appending "…" if truncated.
///
/// Uses `char_indices` for UTF-8 safe truncation — never panics on
/// multi-byte characters (e.g. Chinese, emoji).
pub(super) fn truncate_chars(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        Some((byte_pos, _)) => format!("{}…", &s[..byte_pos]),
        None => s.to_string(),
    }
}

/// Format tool output lines with smart truncation (head + tail).
///
/// Returns a `Vec<String>` of formatted lines, ready to be printed.
/// Each line is prefixed with a dim vertical bar (`│`).
///
/// Mimics Claude Code's approach: if the output exceeds `TOOL_OUTPUT_MAX_LINES`,
/// show the first `TOOL_OUTPUT_HEAD_LINES` lines, a "... (N lines hidden) ..."
/// indicator, and the last `TOOL_OUTPUT_TAIL_LINES` lines.
pub(super) fn format_truncated_output(lines: &[&str]) -> Vec<String> {
    let line_count = lines.len();
    let mut result = Vec::new();

    if line_count == 0 {
        return result;
    }

    if line_count <= TOOL_OUTPUT_MAX_LINES {
        for line in lines {
            result.push(line.to_string());
        }
    } else {
        for line in &lines[..TOOL_OUTPUT_HEAD_LINES] {
            result.push(line.to_string());
        }
        let hidden = line_count - TOOL_OUTPUT_HEAD_LINES - TOOL_OUTPUT_TAIL_LINES;
        result.push(format!("... ({} lines hidden) ...", hidden));
        for line in &lines[line_count - TOOL_OUTPUT_TAIL_LINES..] {
            result.push(line.to_string());
        }
    }

    result
}

// ── Primitive helpers ──

/// Print a dim/muted line to stdout (for secondary information).
fn print_dim(text: &str) {
    println!("{}", text.with(Color::DarkGrey));
}

/// Print a key-value line with the key in dim and the value in the given color.
fn print_key_value(key: &str, value: &str, value_color: Color) {
    println!(
        "    {}  {}",
        key.with(Color::DarkGrey),
        value.with(value_color),
    );
}

// ── Banner ──

/// Print the startup banner in Claude Code style.
pub fn banner(agent: &dyn AgentMetadata) {
    println!();
    println!(
        "{}  {}",
        "🏛️ Daedalus".with(Color::Cyan).attribute(Attribute::Bold),
        format!("v{}", VERSION).with(Color::DarkGrey),
    );
    println!();
    print_dim(&format!(
        "  Model:    {}  ({})",
        agent.model_name(),
        agent.provider_name(),
    ));
    print_dim(&format!("  Mode:     {}", agent.mode_name()));
    if agent.has_tools() {
        print_dim(&format!("  Tools:    {} available", agent.tool_count()));
    }
    if agent.skill_count() > 0 {
        print_dim(&format!("  Skills:   {} available", agent.skill_count()));
    }
    if agent.subagent_count() > 0 {
        print_dim(&format!("  Agents:   {} available", agent.subagent_count()));
    }
    print_dim(&format!(
        "  Session:  {} ({})",
        agent.session().title,
        agent.session().short_id(),
    ));
    println!();
    print_dim("  Type /help for available commands.");
    println!();
}

// ── Help ──

/// Print the `/help` output.
pub fn help() {
    println!();
    println!(
        "{}",
        "  Available commands:"
            .with(Color::White)
            .attribute(Attribute::Bold)
    );
    println!();
    for (cmd, desc) in SLASH_COMMANDS {
        println!(
            "    {}  {}",
            format!("{:<12}", cmd).with(Color::Cyan),
            desc.with(Color::DarkGrey),
        );
    }
    println!();
    print_dim("  Or just type a message to chat with the assistant.");
    println!();
}

// ── Cost ──

/// Print the `/cost` output.
pub fn cost(cost: &SessionCost) {
    println!();
    println!(
        "{}",
        "  Session token usage:"
            .with(Color::White)
            .attribute(Attribute::Bold)
    );
    println!();
    print_key_value("Requests:", &cost.requests().to_string(), Color::White);
    print_key_value("Prompt tokens:", &cost.prompt_tokens().to_string(), Color::White);
    print_key_value("Completion tokens:", &cost.completion_tokens().to_string(), Color::White);
    print_key_value("Total tokens:", &cost.total_tokens().to_string(), Color::Cyan);
    println!();
}

// ── Model info ──

/// Print the `/model` output.
pub fn model_info(agent: &dyn AgentMetadata) {
    println!();
    print_key_value("Provider:", agent.provider_name(), Color::White);
    print_key_value("Model:", agent.model_name(), Color::Cyan);
    print_key_value("Mode:", agent.mode_name(), Color::White);
    if agent.has_tools() {
        print_key_value("Tools:", &format!("{} available", agent.tool_count()), Color::Green);
    }
    println!();
}

// ── Spinner ──

/// Create a spinner for the "thinking" state.
pub fn spinner() -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
            .template("  {spinner} {msg}")
            .expect("invalid spinner template"),
    );
    pb.set_message("Thinking…");
    pb.enable_steady_tick(std::time::Duration::from_millis(80));
    pb
}

// ── Reasoning content ──

/// Render the reasoning/thinking process from a reasoning model.
///
/// Displayed in dim style with a vertical border to visually distinguish
/// it from the final response content.
pub fn reasoning_content(reasoning: &str) {
    println!();
    println!(
        "  {} {}",
        "💭".to_string(),
        "Reasoning:".with(Color::DarkGrey).attribute(Attribute::Italic),
    );
    for line in reasoning.lines() {
        println!(
            "  {}  {}",
            "┊".with(Color::DarkGrey),
            line.with(Color::DarkGrey),
        );
    }
}

// ── Response rendering ──

/// Render the assistant's response with terminal markdown support.
pub fn response(content: &str) {
    let skin = termimad::MadSkin::default();
    println!();
    let rendered = skin.term_text(content);
    for line in rendered.to_string().lines() {
        println!("  {}", line);
    }
}

/// Print a compact token-usage / elapsed-time line after each response.
pub fn response_footer(usage: Option<&TokenUsage>, elapsed: f64) {
    // Collect non-None parts, skipping unavailable token counts
    let parts: Vec<String> = [
        usage.and_then(|u| u.prompt_tokens).map(|t| format!("{}↑", t)),
        usage.and_then(|u| u.completion_tokens).map(|t| format!("{}↓", t)),
        Some(format!("{:.1}s", elapsed)),
    ]
    .into_iter()
    .flatten()
    .collect();

    if !parts.is_empty() {
        println!();
        println!("  {}", parts.join(" · ").with(Color::DarkGrey));
    }
}

// ── Session events ──

/// Print the "new session started" message.
pub fn new_session(agent: &dyn AgentMetadata) {
    println!();
    println!(
        "  {} New session started.",
        "✨".to_string().with(Color::Yellow),
    );
    print_dim(&format!(
        "  Session: {} ({})",
        agent.session().title,
        agent.session().short_id(),
    ));
    println!();
}

/// Print the "screen cleared" message.
pub fn screen_cleared(agent: &dyn AgentMetadata) {
    print_dim(&format!(
        "  Screen cleared. Session: {} ({})",
        agent.session().title,
        agent.session().short_id(),
    ));
    println!();
}

// ── Tools list ──

/// Print the `/tools` output — list all available MCP tools.
pub fn tools_list(agent: &dyn AgentMetadata) {
    println!();
    if !agent.has_tools() {
        print_dim("  No MCP tools available.");
        print_dim("  Configure MCP servers in mcp.json to enable tool calling.");
        println!();
        return;
    }

    println!(
        "{}",
        format!("  Available MCP tools ({}):", agent.tool_count())
            .with(Color::White)
            .attribute(Attribute::Bold)
    );
    println!();

    for tool in agent.tool_infos() {
        println!(
            "    {}  {}",
            tool.name.with(Color::Cyan),
            format!("({})", tool.source).with(Color::DarkGrey),
        );
        if !tool.description.is_empty() {
            print_dim(&format!("      {}", tool.description));
        }
    }

    println!();
    print_dim("    The LLM will automatically use these tools when needed.");
    println!();
}

// ── Skills list ──

/// Print the `/skills` output — list all available skills.
pub fn skills_list(agent: &dyn AgentMetadata) {
    println!();
    let infos = agent.skill_infos();
    if infos.is_empty() {
        print_dim("  No skills available.");
        print_dim("  Place .md files in the ./skills/ directory to add skills.");
        println!();
        return;
    }

    println!(
        "{}",
        format!("  Available skills ({}):", infos.len())
            .with(Color::White)
            .attribute(Attribute::Bold)
    );
    println!();

    for skill in &infos {
        println!(
            "    {}",
            skill.name.as_str().with(Color::Cyan),
        );
        if !skill.description.is_empty() {
            print_dim(&format!("      {}", skill.description));
        }
    }

    println!();
    print_dim("    The LLM will automatically invoke skills via the use_skill tool when needed.");
    println!();
}

// ── Agents list ──

/// Print the `/agents` output — list all available subagents.
pub fn agents_list(agent: &dyn AgentMetadata) {
    println!();
    let infos = agent.subagent_infos();
    if infos.is_empty() {
        print_dim("  No subagents available.");
        print_dim("  Place .md files in the .daedalus/agents/ directory to add subagents.");
        println!();
        return;
    }

    println!(
        "{}",
        format!("  Available subagents ({}):", infos.len())
            .with(Color::White)
            .attribute(Attribute::Bold)
    );
    println!();

    for info in &infos {
        println!(
            "    {}  {}",
            info.name.as_str().with(Color::Cyan),
            format!("({})", info.source).with(Color::DarkGrey),
        );
        if !info.description.is_empty() {
            // Show first line of description only
            let first_line = info.description.lines().next().unwrap_or("").trim();
            print_dim(&format!("      {}", first_line));
        }
    }

    println!();
    print_dim("    The LLM will automatically spawn subagents via the spawn_subagent tool when needed.");
    println!();
}

// ── Tool execution events ──

/// Format a tool execution event into styled lines for terminal display.
///
/// Returns a `Vec<String>` of pre-formatted lines (with ANSI color codes).
/// Callers can output these to stdout (`println!`) or stderr (`eprintln!`)
/// depending on the output mode.
///
/// This eliminates the ~180 lines of duplicated ToolEvent rendering logic
/// that previously existed in both `render::tool_event()` and
/// `print_runner::build_text_stderr_callback()`.
pub(super) fn format_tool_event_lines(event: &ToolEvent) -> Vec<String> {
    let mut lines = Vec::new();
    match event {
        ToolEvent::RoundStart { round } => {
            lines.push(format!(
                "  🔧 {}",
                format!("Tool round {}", round)
                    .with(Color::Cyan)
                    .attribute(Attribute::Bold),
            ));
        }
        ToolEvent::ToolCallStart { tool_name, source, arguments } => {
            lines.push(format!(
                "  {}  {} {}",
                "▸".with(Color::Yellow),
                tool_name.as_str().with(Color::White).attribute(Attribute::Bold),
                format!("({})", source).with(Color::DarkGrey),
            ));
            if let Some(summary) = summarize_tool_args(tool_name, arguments) {
                lines.push(format!(
                    "      {}",
                    summary.with(Color::DarkGrey).attribute(Attribute::Italic),
                ));
            }
            for diff_line in edit_diff_preview(tool_name, arguments) {
                lines.push(format!(
                    "      {}",
                    diff_line,
                ));
            }
        }
        ToolEvent::ToolCallComplete { tool_name, success, result_content } => {
            let (icon, color) = if *success {
                ("✓", Color::Green)
            } else {
                ("✗", Color::Red)
            };
            if *success {
                let content_lines: Vec<&str> = result_content.lines().collect();
                let line_count = content_lines.len();
                lines.push(format!(
                    "    {} {}",
                    icon.with(color),
                    format!("{} ({} lines)", tool_name, line_count).with(Color::DarkGrey),
                ));
                // Render output with smart truncation
                for formatted_line in format_truncated_output(&content_lines) {
                    lines.push(format!(
                        "    {}  {}",
                        "│".with(Color::DarkGrey),
                        formatted_line.with(Color::DarkGrey),
                    ));
                }
            } else {
                let first_line = result_content.lines().next().unwrap_or("");
                lines.push(format!(
                    "    {} {}{}",
                    icon.with(color),
                    format!("{}: ", tool_name).with(color),
                    first_line.with(Color::DarkGrey),
                ));
            }
        }
        ToolEvent::RoundComplete { tool_count } => {
            lines.push(format!(
                "  {}",
                format!("  {} tool call(s) completed", tool_count).with(Color::DarkGrey),
            ));
            lines.push(String::new());
        }
        ToolEvent::SubagentStart { agent_name, task_preview } => {
            lines.push(String::new());
            lines.push(format!(
                "  {} {} {}",
                "\u{1F916}".to_string(),
                format!("Subagent '{}' started", agent_name)
                    .with(Color::Magenta)
                    .attribute(Attribute::Bold),
                "—".with(Color::DarkGrey),
            ));
            let preview = truncate_chars(task_preview, 100);
            lines.push(format!(
                "    {}",
                preview.with(Color::DarkGrey),
            ));
            lines.push(String::new());
        }
        ToolEvent::SubagentComplete { agent_name, success, tool_rounds, result_preview } => {
            let (icon, color) = if *success {
                ("✓", Color::Green)
            } else {
                ("✗", Color::Red)
            };
            lines.push(format!(
                "  {} {} {}",
                icon.with(color),
                format!("Subagent '{}' completed", agent_name).with(color),
                format!("({} tool rounds)", tool_rounds).with(Color::DarkGrey),
            ));
            let preview = truncate_chars(result_preview, 120);
            if !preview.is_empty() {
                lines.push(format!(
                    "    {}",
                    preview.with(Color::DarkGrey),
                ));
            }
            lines.push(String::new());
        }
    }
    lines
}

/// Render a tool execution event to stdout (interactive REPL mode).
///
/// Called in real-time during the tool-calling loop to show progress.
pub fn tool_event(event: &ToolEvent) {
    for line in format_tool_event_lines(event) {
        println!("{}", line);
    }
}

// ── Tool argument summarization ──

/// Build a one-line human-readable summary of a tool call's arguments.
///
/// Returns `None` when nothing useful can be extracted (e.g. the arguments
/// are empty or the tool is unknown and carries no obvious fields).
///
/// The goal is to show the user **what** a tool is about to do, e.g.:
/// - `read_file`            → `src/foo.rs:100-200`
/// - `edit_file`            → `src/foo.rs  (replace_all)`
/// - `multi_edit`           → `src/foo.rs  (5 edits)`
/// - `write_file`           → `README.md  (1.2 KB)`
/// - `grep_search`          → `"DEFAULT_MAX_TOOL_ROUNDS" in src/  (regex)`
/// - `search_files`         → `*.rs in src/cli/`
/// - `list_directory`       → `src/subagent/`
/// - `bash`                 → `$ cargo build --release  [cwd: /tmp]`
/// - `use_skill`            → `skill-creator`
/// - `get_file_info`        → `src/foo.rs`
/// - MCP / unknown          → truncated single-line JSON preview
pub(super) fn summarize_tool_args(
    tool_name: &str,
    args: &serde_json::Value,
) -> Option<String> {
    // Helper closures
    let s = |key: &str| -> Option<String> {
        args.get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };
    let u = |key: &str| -> Option<u64> {
        args.get(key).and_then(|v| v.as_u64())
    };
    let b = |key: &str| -> Option<bool> {
        args.get(key).and_then(|v| v.as_bool())
    };

    match tool_name {
        "read_file" => {
            let path = s("path")?;
            match (u("offset"), u("limit")) {
                (Some(off), Some(lim)) => Some(format!("{}:{}-{}", path, off, off + lim)),
                (Some(off), None) => Some(format!("{} (from line {})", path, off)),
                (None, Some(lim)) => Some(format!("{} (first {} lines)", path, lim)),
                (None, None) => Some(path),
            }
        }
        "edit_file" => {
            let path = s("path")?;
            if b("replace_all").unwrap_or(false) {
                Some(format!("{}  (replace_all)", path))
            } else {
                Some(path)
            }
        }
        "multi_edit" => {
            let path = s("path")?;
            let n = args
                .get("edits")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            Some(format!("{}  ({} edit{})", path, n, if n == 1 { "" } else { "s" }))
        }
        "write_file" => {
            let path = s("path")?;
            let bytes = args
                .get("content")
                .and_then(|v| v.as_str())
                .map(|c| c.len())
                .unwrap_or(0);
            Some(format!("{}  ({})", path, human_bytes(bytes)))
        }
        "grep_search" => {
            let pattern = s("pattern")?;
            let scope = s("path").unwrap_or_else(|| ".".to_string());
            let mut tags: Vec<&str> = Vec::new();
            if !b("use_regex").unwrap_or(true) {
                tags.push("literal");
            }
            if !b("case_sensitive").unwrap_or(true) {
                tags.push("icase");
            }
            let suffix = if tags.is_empty() {
                String::new()
            } else {
                format!("  ({})", tags.join(", "))
            };
            Some(format!(
                "{:?} in {}{}",
                truncate_chars(&pattern, 60),
                scope,
                suffix
            ))
        }
        "search_files" => {
            let pattern = s("pattern")?;
            let scope = s("path").unwrap_or_else(|| ".".to_string());
            Some(format!("{} in {}", pattern, scope))
        }
        "list_directory" | "get_file_info" => s("path"),
        "bash" => {
            let cmd = s("command")?;
            let cwd = s("working_directory");
            let timeout = u("timeout_secs").or_else(|| u("timeout"));
            let first_line = cmd.lines().next().unwrap_or("").to_string();
            let preview = truncate_chars(&first_line, 100);
            let mut out = format!("$ {}", preview);
            let mut extras: Vec<String> = Vec::new();
            if let Some(dir) = cwd {
                extras.push(format!("cwd: {}", dir));
            }
            if let Some(t) = timeout {
                extras.push(format!("timeout: {}s", t));
            }
            if !extras.is_empty() {
                out.push_str(&format!("  [{}]", extras.join(", ")));
            }
            Some(out)
        }
        "use_skill" => s("name").or_else(|| s("skill_name")),
        _ => {
            // Generic fallback: compact single-line JSON preview.
            let compact = serde_json::to_string(args).ok()?;
            if compact == "{}" || compact == "null" {
                None
            } else {
                Some(truncate_chars(&compact, 120))
            }
        }
    }
}

/// Build a tiny inline diff preview for editing tools.
///
/// For `edit_file` / `multi_edit`, shows the first `old_string` → `new_string`
/// replacement as two colored lines:
///
/// ```text
///       - old text here
///       + new text here
/// ```
///
/// Each string is trimmed to a single line and truncated to keep the UI tidy.
/// For `multi_edit`, only the first edit is previewed, with a trailing
/// "(+N more)" hint when there are additional edits.
pub(super) fn edit_diff_preview(
    tool_name: &str,
    args: &serde_json::Value,
) -> Vec<String> {
    const MAX_CHARS: usize = 100;
    let mut out: Vec<String> = Vec::new();

    let format_pair = |old: &str, new: &str, extra: Option<String>| -> Vec<String> {
        let old_preview = truncate_chars(
            old.lines().next().unwrap_or("").trim_end(),
            MAX_CHARS,
        );
        let new_preview = truncate_chars(
            new.lines().next().unwrap_or("").trim_end(),
            MAX_CHARS,
        );
        let mut lines = vec![
            format!("{} {}", "-".with(Color::Red), old_preview.with(Color::Red)),
            format!("{} {}", "+".with(Color::Green), new_preview.with(Color::Green)),
        ];
        if let Some(tail) = extra {
            lines.push(tail.with(Color::DarkGrey).to_string());
        }
        lines
    };

    match tool_name {
        "edit_file" => {
            let old_s = args.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
            let new_s = args.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
            if !old_s.is_empty() || !new_s.is_empty() {
                out.extend(format_pair(old_s, new_s, None));
            }
        }
        "multi_edit" => {
            if let Some(edits) = args.get("edits").and_then(|v| v.as_array()) {
                if let Some(first) = edits.first() {
                    let old_s = first.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
                    let new_s = first.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
                    let extra = if edits.len() > 1 {
                        Some(format!("(+{} more edit{})",
                            edits.len() - 1,
                            if edits.len() - 1 == 1 { "" } else { "s" }))
                    } else {
                        None
                    };
                    out.extend(format_pair(old_s, new_s, extra));
                }
            }
        }
        _ => {}
    }

    out
}

/// Human-readable byte size, e.g. `1.2 KB`, `345 B`, `7.8 MB`.
fn human_bytes(n: usize) -> String {
    const KB: usize = 1024;
    const MB: usize = 1024 * 1024;
    if n >= MB {
        format!("{:.1} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1} KB", n as f64 / KB as f64)
    } else {
        format!("{} B", n)
    }
}

// ── Error / exit ──

/// Print an unknown-command warning.
pub fn unknown_command(input: &str) {
    println!();
    println!(
        "  {} Unknown command: {}. Type {} for help.",
        "⚠".with(Color::Yellow),
        input.with(Color::White),
        "/help".with(Color::Cyan),
    );
    println!();
}

/// Print the goodbye message.
pub fn goodbye() {
    println!();
    println!("  {}", "Goodbye! 👋".with(Color::DarkGrey));
    println!();
}

/// Print an error message.
pub fn error(err: &anyhow::Error) {
    println!();
    println!(
        "  {} {}",
        "✗".with(Color::Red).attribute(Attribute::Bold),
        format!("Error: {}", err).with(Color::Red),
    );
    println!();
}
