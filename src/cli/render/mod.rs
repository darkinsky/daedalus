//! Terminal rendering for the CLI layer.
//!
//! Split into focused submodules:
//!
//! - [`tool_output`] — bash-command unfolding + smart truncation of tool output
//! - [`summarize`]   — tool-call argument → human-readable one-liner
//! - [`tool_event`]  — stateful event-stream → styled terminal lines
//! - [`lists`]       — `/tools` / `/skills` / `/agents` slash-command renderers
//!
//! This file keeps the small, glue-style renderers (banner, help, cost,
//! model info, spinner, response, session events, errors) and re-exports
//! the public surface so existing callers work unchanged via
//! `use super::render; render::xxx()`.

mod tool_output;
mod summarize;
mod tool_event;
mod lists;

use chrono::Local;
use crossterm::style::{Attribute, Color, Stylize};
use indicatif::{ProgressBar, ProgressStyle};

use crate::agent::AgentMetadata;
use crate::llm::TokenUsage;
use crate::middleware::builtin::cost::SessionCost;

use super::commands::SLASH_COMMANDS;

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

// ── Token usage formatting helpers ──

/// Format token usage into a list of human-readable parts.
///
/// Output example: `["input: 58,965 (cached: 56,320 95%)", "output: 868", "ctx: 59,833/1,000,000 5%"]`
///
/// When `context_window` is provided (> 0), the total is displayed as a
/// fraction of the context window with a percentage.
///
/// Used by `response_footer`, `turn_summary`, and `tool_event` to avoid
/// duplicating the same `if let Some(pt) = ...` pattern everywhere.
pub(super) fn format_token_parts(usage: &TokenUsage, context_window: Option<usize>) -> Vec<String> {
    let mut parts = Vec::new();
    if let Some(pt) = usage.prompt_tokens {
        let mut input_part = format!("input: {}", format_number(pt));
        if let Some(cached) = usage.cached_tokens {
            if cached > 0 {
                let pct = if pt > 0 { cached * 100 / pt } else { 0 };
                input_part.push_str(&format!(" (cached: {} {}%)", format_number(cached), pct));
            }
        }
        parts.push(input_part);
    } else if let Some(cached) = usage.cached_tokens {
        if cached > 0 {
            parts.push(format!("cached: {}", format_number(cached)));
        }
    }
    if let Some(ct) = usage.completion_tokens {
        parts.push(format!("output: {}", format_number(ct)));
    }
    if let Some(tt) = usage.total_tokens {
        match context_window {
            Some(cw) if cw > 0 => {
                let pct = tt * 100 / cw as u64;
                parts.push(format!("ctx: {}/{} {}%", format_number(tt), format_number(cw as u64), pct));
            }
            _ => {
                parts.push(format!("total: {}", format_number(tt)));
            }
        }
    }
    parts
}

/// Format a number with thousand separators for readability (e.g. 58965 → "58,965").
fn format_number(n: u64) -> String {
    if n < 1_000 {
        return n.to_string();
    }
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    result.chars().rev().collect()
}

/// Accumulate token usage from multiple sources into grand totals.
///
/// Returns `(prompt, completion, total, has_any_tokens)`.
fn accumulate_usage<'a>(usages: impl Iterator<Item = Option<&'a TokenUsage>>) -> (u64, u64, u64, bool) {
    let mut prompt: u64 = 0;
    let mut completion: u64 = 0;
    let mut total: u64 = 0;
    let mut has_tokens = false;
    for usage in usages {
        if let Some(u) = usage {
            prompt += u.prompt_tokens.unwrap_or(0);
            completion += u.completion_tokens.unwrap_or(0);
            total += u.total_tokens.unwrap_or(0);
            if u.total_tokens.is_some() {
                has_tokens = true;
            }
        }
    }
    (prompt, completion, total, has_tokens)
}

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
/// The spinner shows a rotating Braille animation with a blinking cursor
/// block so users can clearly perceive the model is actively working.
/// The `{elapsed}` placeholder is built into `indicatif` and updates
/// automatically.
pub fn spinner() -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    // Alternate between cursor-visible and cursor-hidden frames to create
    // a blinking cursor effect alongside the Braille spinner.
    pb.set_style(
        ProgressStyle::default_spinner()
            .tick_strings(&[
                "⠋ ▊", "⠙ ▊", "⠹ ▊", "⠸ ▊", "⠼ ▊",
                "⠴  ", "⠦  ", "⠧  ", "⠇  ", "⠏  ",
            ])
            .template("  {spinner:.cyan} {msg} {elapsed:.dim}")
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

/// Print the streaming response header (before any chunks arrive).
pub fn stream_response_header() {
    let ts = Local::now().format("%H:%M:%S");
    println!();
    println!(
        "  {} {}",
        "🤖 Response".with(Color::Cyan).attribute(Attribute::Bold),
        format!("[{}]", ts).with(Color::DarkGrey),
    );
    // Print the indent prefix for the first line of streaming content
    print!("  ");
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

/// Print a streaming text chunk (no newline, no markdown processing).
///
/// This is called for each `StreamText` event during streaming mode.
/// The text is printed raw (without markdown rendering) for real-time
/// display. The full response will be re-rendered with markdown after
/// streaming completes.
pub fn stream_text_chunk(text: &str) {
    use std::io::Write;
    // Handle newlines in the chunk: indent continuation lines
    let mut first = true;
    for line in text.split('\n') {
        if !first {
            print!("\n  ");
        }
        print!("{}", line);
        first = false;
    }
    let _ = std::io::stdout().flush();
}

/// Print a streaming reasoning chunk.
pub fn stream_reasoning_chunk(text: &str) {
    use std::io::Write;
    print!("{}", text.with(Color::DarkGrey));
    let _ = std::io::stdout().flush();
}

/// Print the streaming reasoning header.
pub fn stream_reasoning_header() {
    let ts = Local::now().format("%H:%M:%S");
    println!();
    println!(
        "  {} {} {}",
        "💭".to_string(),
        "Reasoning:".with(Color::DarkGrey).attribute(Attribute::Italic),
        format!("[{}]", ts).with(Color::DarkGrey),
    );
    print!("  {}  ", "┊".with(Color::DarkGrey));
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

/// Finish the streaming output (print a final newline).
pub fn stream_done() {
    println!();
}

/// Print a compact token-usage / elapsed-time line after each response.
///
/// `context_window` is the model's max context size in tokens, used to
/// display a usage percentage (e.g. "ctx: 59,833/1,000,000 5%").
pub fn response_footer(usage: Option<&TokenUsage>, elapsed: f64, context_window: usize) {
    let cw = if context_window > 0 { Some(context_window) } else { None };
    let mut parts: Vec<String> = if let Some(u) = usage {
        format_token_parts(u, cw)
    } else {
        Vec::new()
    };

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
    context_window: usize,
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

    let cw = if context_window > 0 { Some(context_window) } else { None };

    // Lead agent row
    {
        let mut parts: Vec<String> = if let Some(u) = lead_usage {
            format_token_parts(u, cw)
        } else {
            Vec::new()
        };
        parts.push(format!("{:.1}s", total_elapsed));
        println!(
            "    {} {}",
            "Lead:".with(Color::White).attribute(Attribute::Bold),
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
        let mut parts: Vec<String> = if let Some(ref u) = sa.usage {
            format_token_parts(u, None)
        } else {
            Vec::new()
        };
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
        let all_usages = std::iter::once(lead_usage)
            .chain(subagents.iter().map(|sa| sa.usage.as_ref()));
        let (grand_prompt, grand_completion, grand_total, has_tokens) = accumulate_usage(all_usages);

        if has_tokens {
            // Don't show ctx percentage in Total row when subagents are present.
            // The grand_total is the cumulative token consumption across all agents
            // and rounds, NOT the current context window occupancy. Dividing it by
            // the lead agent's context window is misleading (e.g. 209% makes no sense).
            // ctx% is only meaningful per-agent (shown in the Lead row above).
            let ctx_info = if subagents.is_empty() {
                // No subagents: Total == Lead, ctx% is meaningful
                match cw {
                    Some(cw_val) if cw_val > 0 => {
                        let pct = grand_total * 100 / cw_val as u64;
                        format!("ctx: {}/{} {}%", format_number(grand_total), format_number(cw_val as u64), pct)
                    }
                    _ => format!("total: {}", format_number(grand_total)),
                }
            } else {
                // With subagents: show plain total (cumulative consumption)
                format!("total: {}", format_number(grand_total))
            };
            println!(
                "    {} {}",
                "Total:".with(Color::Yellow).attribute(Attribute::Bold),
                format!(
                    "input: {} · output: {} · {} · {:.1}s",
                    format_number(grand_prompt), format_number(grand_completion),
                    ctx_info, total_elapsed,
                )
                .with(Color::DarkGrey),
            );
        }
    }
}
