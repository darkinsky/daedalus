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
        ToolEvent::ToolCallStart { tool_name, source } => {
            lines.push(format!(
                "  {}  {} {}",
                "▸".with(Color::Yellow),
                tool_name.as_str().with(Color::White).attribute(Attribute::Bold),
                format!("({})", source).with(Color::DarkGrey),
            ));
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
