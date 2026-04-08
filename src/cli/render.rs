use crossterm::style::{Attribute, Color, Stylize};
use indicatif::{ProgressBar, ProgressStyle};

use crate::agent::AgentMode;
use super::commands::SLASH_COMMANDS;
use super::cost::SessionCost;

const VERSION: &str = env!("CARGO_PKG_VERSION");

// ── Primitive helpers ──

/// Print a dim/muted line (for secondary information).
fn dim(text: &str) {
    println!("{}", text.with(Color::DarkGrey));
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
    dim(&format!(
        "  Model:    {}  ({})",
        agent.model_name(),
        agent.provider_name(),
    ));
    dim(&format!("  Mode:     {}", agent.mode_name()));
    if agent.has_tools() {
        dim(&format!("  Tools:    {} available", agent.tool_count()));
    }
    dim(&format!(
        "  Session:  {} ({})",
        agent.session().title,
        &agent.session().id[..8],
    ));
    println!();
    dim("  Type /help for available commands.");
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
    dim("  Or just type a message to chat with the assistant.");
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
    println!(
        "    {}  {}",
        "Requests:".with(Color::DarkGrey),
        cost.requests().to_string().with(Color::White),
    );
    println!(
        "    {}  {}",
        "Prompt tokens:".with(Color::DarkGrey),
        cost.prompt_tokens().to_string().with(Color::White),
    );
    println!(
        "    {}  {}",
        "Completion tokens:".with(Color::DarkGrey),
        cost.completion_tokens().to_string().with(Color::White),
    );
    println!(
        "    {}  {}",
        "Total tokens:".with(Color::DarkGrey),
        cost.total_tokens().to_string().with(Color::Cyan),
    );
    println!();
}

// ── Model info ──

/// Print the `/model` output.
pub fn model_info(agent: &dyn AgentMode) {
    println!();
    println!(
        "    {}  {}",
        "Provider:".with(Color::DarkGrey),
        agent.provider_name().with(Color::White),
    );
    println!(
        "    {}  {}",
        "Model:".with(Color::DarkGrey),
        agent.model_name().with(Color::Cyan),
    );
    println!(
        "    {}  {}",
        "Mode:".with(Color::DarkGrey),
        agent.mode_name().with(Color::White),
    );
    if agent.has_tools() {
        println!(
            "    {}  {}",
            "Tools:".with(Color::DarkGrey),
            format!("{} available", agent.tool_count()).with(Color::Green),
        );
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
pub fn response_footer(
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    elapsed: f64,
) {
    let parts: Vec<String> = [
        prompt_tokens.map(|t| format!("{}↑", t)),
        completion_tokens.map(|t| format!("{}↓", t)),
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
    dim(&format!(
        "  Session: {} ({})",
        agent.session().title,
        &agent.session().id[..8],
    ));
    println!();
}

/// Print the "screen cleared" message.
pub fn screen_cleared(agent: &dyn AgentMode) {
    dim(&format!(
        "  Screen cleared. Session: {} ({})",
        agent.session().title,
        &agent.session().id[..8],
    ));
    println!();
}

// ── Tools list ──

/// Print the `/tools` output — list all available MCP tools.
pub fn tools_list(agent: &dyn AgentMode) {
    println!();
    if !agent.has_tools() {
        dim("  No MCP tools available.");
        dim("  Configure MCP servers in mcp.json to enable tool calling.");
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

    for tool in agent.tool_descriptions() {
        println!(
            "    {}  {}",
            tool.name.with(Color::Cyan),
            format!("({})", tool.server).with(Color::DarkGrey),
        );
        if !tool.description.is_empty() {
            dim(&format!("      {}", tool.description));
        }
    }

    println!();
    dim("    The LLM will automatically use these tools when needed.");
    println!();
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
