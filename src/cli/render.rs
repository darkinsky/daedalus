use crossterm::style::{Attribute, Color, Stylize};
use indicatif::{ProgressBar, ProgressStyle};

use crate::agent::{AgentMode, ToolEvent};
use crate::llm::TokenUsage;
use super::commands::SLASH_COMMANDS;
use super::cost::SessionCost;

const VERSION: &str = env!("CARGO_PKG_VERSION");

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
pub fn banner(agent: &dyn AgentMode) {
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
pub fn model_info(agent: &dyn AgentMode) {
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
pub fn new_session(agent: &dyn AgentMode) {
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
pub fn screen_cleared(agent: &dyn AgentMode) {
    print_dim(&format!(
        "  Screen cleared. Session: {} ({})",
        agent.session().title,
        agent.session().short_id(),
    ));
    println!();
}

// ── Tools list ──

/// Print the `/tools` output — list all available MCP tools.
pub fn tools_list(agent: &dyn AgentMode) {
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
pub fn skills_list(agent: &dyn AgentMode) {
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

// ── Tool execution events ──

/// Render a tool execution event to the terminal.
///
/// Called in real-time during the tool-calling loop to show progress.
pub fn tool_event(event: &ToolEvent) {
    match event {
        ToolEvent::RoundStart { round } => {
            println!(
                "  🔧 {}",
                format!("Tool round {}", round)
                    .with(Color::Cyan)
                    .attribute(Attribute::Bold),
            );
        }
        ToolEvent::ToolCallStart { tool_name, source } => {
            println!(
                "  {}  {} {}",
                "▸".with(Color::Yellow),
                tool_name.as_str().with(Color::White).attribute(Attribute::Bold),
                format!("({})", source).with(Color::DarkGrey),
            );
        }
        ToolEvent::ToolCallComplete { tool_name, success, result_preview } => {
            let (icon, color) = if *success {
                ("✓", Color::Green)
            } else {
                ("✗", Color::Red)
            };
            let error_prefix = if *success { "" } else { &format!("{}: ", tool_name) };
            println!(
                "    {} {}{}",
                icon.with(color),
                error_prefix.with(color),
                result_preview.as_str().with(Color::DarkGrey),
            );
        }
        ToolEvent::RoundComplete { tool_count } => {
            println!(
                "  {}",
                format!("  {} tool call(s) completed", tool_count).with(Color::DarkGrey),
            );
            println!();
        }
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
