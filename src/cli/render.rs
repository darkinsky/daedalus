//! Terminal rendering for the CLI layer.
//!
//! Originally one 1,000+-line file that mixed banner/help/cost/spinner/
//! list/tool-event rendering into a single namespace. The logic is now
//! split into focused submodules (loaded from `render/*.rs` via
//! `#[path]` so the file layout can live in a directory even though
//! `mod.rs` is deliberately unused to avoid filesystem-layout churn):
//!
//! - [`tool_output`] — bash-command unfolding + smart truncation of tool output
//! - [`summarize`]   — tool-call argument → human-readable one-liner
//! - [`tool_event`]  — stateful event-stream → styled terminal lines
//! - [`lists`]       — `/tools` / `/skills` / `/agents` slash-command renderers
//!
//! This top-level file keeps the small, glue-style renderers (banner,
//! help, cost, model info, spinner, response, session events, errors)
//! and re-exports the public surface so existing callers work unchanged
//! via `use super::render; render::xxx()`.

#[path = "render/tool_output.rs"]
mod tool_output;
#[path = "render/summarize.rs"]
mod summarize;
#[path = "render/tool_event.rs"]
mod tool_event;
#[path = "render/lists.rs"]
mod lists;

use chrono::Local;
use crossterm::style::{Attribute, Color, Stylize};
use indicatif::{ProgressBar, ProgressStyle};

use crate::agent::AgentMetadata;
use crate::llm::TokenUsage;

use super::commands::SLASH_COMMANDS;
use super::cost::SessionCost;

// ── Public re-exports ──

// Slash-command list views live in the `lists` submodule.
pub(in crate::cli) use lists::{agents_list, skills_list, tools_list};

// Tool-event rendering machinery. `ToolEventFormatter` is the stateful
// primitive used by long-lived callbacks (REPL spinner, `--print` text
// mode); `tool_event` is a one-shot convenience wrapper.
#[allow(unused_imports)]
pub(in crate::cli) use tool_event::tool_event;
pub(in crate::cli) use tool_event::ToolEventFormatter;

const VERSION: &str = env!("CARGO_PKG_VERSION");

// ── Primitive helpers (used by submodules via `super::print_dim`) ──

/// Print a dim/muted line to stdout (for secondary information).
pub(super) fn print_dim(text: &str) {
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
    if cost.subagent_invocations() > 0 {
        println!();
        println!(
            "{}",
            "  Subagent token usage:"
                .with(Color::White)
                .attribute(Attribute::Bold)
        );
        println!();
        print_key_value("Invocations:", &cost.subagent_invocations().to_string(), Color::White);
        print_key_value("Prompt tokens:", &cost.subagent_prompt_tokens().to_string(), Color::White);
        print_key_value("Completion tokens:", &cost.subagent_completion_tokens().to_string(), Color::White);
        print_key_value("Total tokens:", &cost.subagent_total_tokens().to_string(), Color::Cyan);
        println!();
        print_key_value(
            "Grand total:",
            &cost.grand_total_tokens().to_string(),
            Color::Yellow,
        );
    }
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
///
/// The spinner shows elapsed time so users can perceive the model is
/// actively working. The `{elapsed_precise}` placeholder is built into
/// `indicatif` and updates automatically.
pub fn spinner() -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
            .template("  {spinner} {msg} {elapsed}")
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
    let ts = Local::now().format("%H:%M:%S");
    println!();
    println!(
        "  {} {} {}",
        "💭".to_string(),
        "Reasoning:".with(Color::DarkGrey).attribute(Attribute::Italic),
        format!("[{}]", ts).with(Color::DarkGrey),
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
    let ts = Local::now().format("%H:%M:%S");
    println!();
    println!(
        "  {} {}",
        "🤖 Response".with(Color::Cyan).attribute(Attribute::Bold),
        format!("[{}]", ts).with(Color::DarkGrey),
    );
    let rendered = skin.term_text(content);
    for line in rendered.to_string().lines() {
        println!("  {}", line);
    }
}

/// Print a compact token-usage / elapsed-time line after each response.
pub fn response_footer(usage: Option<&TokenUsage>, elapsed: f64) {
    let mut parts: Vec<String> = Vec::new();

    if let Some(u) = usage {
        if let Some(pt) = u.prompt_tokens {
            parts.push(format!("{}↑", pt));
        }
        if let Some(ct) = u.completion_tokens {
            parts.push(format!("{}↓", ct));
        }
        if let Some(tt) = u.total_tokens {
            parts.push(format!("{}total", tt));
        }
    }

    parts.push(format!("{:.1}s", elapsed));

    if !parts.is_empty() {
        let ts = Local::now().format("%H:%M:%S");
        println!();
        println!(
            "  {} {}",
            parts.join(" · ").with(Color::DarkGrey),
            format!("[{}]", ts).with(Color::DarkGrey),
        );
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
        "\u{2717}".with(Color::Red).attribute(Attribute::Bold),
        format!("Error: {}", err).with(Color::Red),
    );
    println!();
}

// ── Turn summary (lead agent + subagents) ──

/// Summary of a single subagent's usage within a turn.
pub struct SubagentUsageSummary {
    pub agent_name: String,
    pub success: bool,
    pub tool_rounds: usize,
    pub usage: Option<TokenUsage>,
    pub elapsed_secs: f64,
}

/// Render a detailed turn summary showing lead agent and all subagent stats.
///
/// Displayed at the end of a turn when subagents were invoked, replacing
/// the simple `response_footer`.
pub fn turn_summary(
    lead_usage: Option<&TokenUsage>,
    total_elapsed: f64,
    subagents: &[SubagentUsageSummary],
) {
    let ts = Local::now().format("%H:%M:%S");

    println!();
    println!(
        "  {} {}",
        "\u{1F4CA} Turn Summary"
            .with(Color::Cyan)
            .attribute(Attribute::Bold),
        format!("[{}]", ts).with(Color::DarkGrey),
    );

    // Lead agent row
    {
        let mut parts: Vec<String> = Vec::new();
        if let Some(u) = lead_usage {
            if let Some(pt) = u.prompt_tokens {
                parts.push(format!("{}\u{2191}", pt));
            }
            if let Some(ct) = u.completion_tokens {
                parts.push(format!("{}\u{2193}", ct));
            }
            if let Some(tt) = u.total_tokens {
                parts.push(format!("{}total", tt));
            }
        }
        parts.push(format!("{:.1}s", total_elapsed));
        println!(
            "    {} {}",
            "Lead agent:".with(Color::White).attribute(Attribute::Bold),
            parts.join(" \u{00b7} ").with(Color::DarkGrey),
        );
    }

    // Subagent rows
    for sa in subagents {
        let icon = if sa.success {
            "\u{2713}".with(Color::Green)
        } else {
            "\u{2717}".with(Color::Red)
        };
        let mut parts: Vec<String> = Vec::new();
        if let Some(ref u) = sa.usage {
            if let Some(pt) = u.prompt_tokens {
                parts.push(format!("{}\u{2191}", pt));
            }
            if let Some(ct) = u.completion_tokens {
                parts.push(format!("{}\u{2193}", ct));
            }
            if let Some(tt) = u.total_tokens {
                parts.push(format!("{}total", tt));
            }
        }
        parts.push(format!("{} rounds", sa.tool_rounds));
        parts.push(format!("{:.1}s", sa.elapsed_secs));
        println!(
            "    {} {} {}",
            icon,
            format!("{}:", sa.agent_name).with(Color::Magenta),
            parts.join(" \u{00b7} ").with(Color::DarkGrey),
        );
    }

    // Grand total row
    {
        let mut grand_prompt: u64 = 0;
        let mut grand_completion: u64 = 0;
        let mut grand_total: u64 = 0;
        let mut has_tokens = false;

        if let Some(u) = lead_usage {
            grand_prompt += u.prompt_tokens.unwrap_or(0);
            grand_completion += u.completion_tokens.unwrap_or(0);
            grand_total += u.total_tokens.unwrap_or(0);
            if u.total_tokens.is_some() {
                has_tokens = true;
            }
        }
        for sa in subagents {
            if let Some(ref u) = sa.usage {
                grand_prompt += u.prompt_tokens.unwrap_or(0);
                grand_completion += u.completion_tokens.unwrap_or(0);
                grand_total += u.total_tokens.unwrap_or(0);
                if u.total_tokens.is_some() {
                    has_tokens = true;
                }
            }
        }

        if has_tokens {
            println!(
                "    {} {}",
                "Grand total:".with(Color::Yellow).attribute(Attribute::Bold),
                format!(
                    "{}\u{2191} \u{00b7} {}\u{2193} \u{00b7} {}total \u{00b7} {:.1}s",
                    grand_prompt, grand_completion, grand_total, total_elapsed,
                )
                .with(Color::DarkGrey),
            );
        }
    }
}