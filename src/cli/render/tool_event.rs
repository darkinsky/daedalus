//! Stateful tool-event → styled terminal output conversion.
//!
//! The [`ToolEventFormatter`] is held by each callback site (REPL spinner,
//! `--print` text-mode stderr) so concurrent tool calls inside one round can
//! be paired up visually via the `[round.index]` tag.
//!
//! ## Compact output design
//!
//! Tool calls use **inline refresh**: `ToolCallStart` emits a single line
//! without a trailing newline (via `InlineProgress`), and the matching
//! `ToolCallComplete` overwrites it with the final status using `\r`.
//! This halves the vertical space used by tool progress.

use std::collections::VecDeque;

use crossterm::style::{Attribute, Color, Stylize};

use crate::tools::{truncate_chars, ToolEvent};

use super::summarize::{edit_diff_preview, summarize_tool_args};

/// Output produced by the formatter for a single event.
///
/// Most events produce `Lines` (printed with `println!`). Tool call starts
/// produce `InlineProgress` — a single line printed with `print!` (no
/// trailing newline) so the subsequent `ToolCallComplete` can overwrite it
/// via `\r`.
pub(in crate::cli) enum FormattedOutput {
    /// Normal lines to print with `println!`.
    Lines(Vec<String>),
    /// A single in-progress line to print with `print!` (no trailing newline).
    /// The caller should `\r`-overwrite it when the matching Complete arrives.
    InlineProgress(String),
}

/// Render a tool-execution event into fully styled terminal lines.
///
/// Stateless convenience wrapper over [`ToolEventFormatter`].
#[allow(dead_code)]
pub(super) fn format_tool_event_lines(event: &ToolEvent) -> Vec<String> {
    match ToolEventFormatter::new().format(event) {
        FormattedOutput::Lines(lines) => lines,
        FormattedOutput::InlineProgress(line) => vec![line],
    }
}

/// Stateful formatter that assigns a `[round.index]` tag to every
/// `ToolCallStart` and re-emits the same tag on the matching
/// `ToolCallComplete`, so users can visually pair them up even when the
/// agent runs several tools in parallel.
///
/// The matching is order-based: inside one round the executor emits
/// completions in the exact same order as the starts, so a FIFO queue
/// is sufficient.
pub(in crate::cli) struct ToolEventFormatter {
    /// 1-based current round number. `0` means "no round seen yet".
    round: usize,
    /// 1-based index of the next `ToolCallStart` within the current round.
    next_index: usize,
    /// Tags waiting to be paired with their `ToolCallComplete`.
    pending_tags: VecDeque<String>,
    /// Whether the last output was an InlineProgress (no trailing newline).
    /// Used to know if we need to print a newline before the next output.
    pub(in crate::cli) has_pending_inline: bool,
}

impl ToolEventFormatter {
    pub(in crate::cli) fn new() -> Self {
        Self {
            round: 0,
            next_index: 0,
            pending_tags: VecDeque::new(),
            has_pending_inline: false,
        }
    }

    /// Render a single event, updating internal numbering state.
    pub(in crate::cli) fn format(&mut self, event: &ToolEvent) -> FormattedOutput {
        match event {
            ToolEvent::RoundStart { round } => FormattedOutput::Lines(self.format_round_start(*round)),
            ToolEvent::ToolCallStart { tool_name, source, arguments } => {
                self.format_call_start(tool_name, source, arguments)
            }
            ToolEvent::ToolCallComplete { tool_name, success, result_content, elapsed_ms } => {
                FormattedOutput::Lines(self.format_call_complete(tool_name, *success, result_content, *elapsed_ms))
            }
            ToolEvent::RoundComplete { tool_count, elapsed_ms } => {
                FormattedOutput::Lines(Self::format_round_complete(*tool_count, *elapsed_ms))
            }
            ToolEvent::LlmResponse { round, reasoning, content, usage, elapsed_ms } => {
                FormattedOutput::Lines(self.format_llm_response(*round, reasoning.as_deref(), content, usage.as_ref(), *elapsed_ms))
            }
            ToolEvent::SubagentStart { agent_name, task_preview } => {
                FormattedOutput::Lines(Self::format_subagent_start(agent_name, task_preview))
            }
            ToolEvent::SubagentComplete {
                agent_name, success, tool_rounds, result_preview, usage, elapsed_ms,
            } => FormattedOutput::Lines(Self::format_subagent_complete(
                agent_name, *success, *tool_rounds, result_preview, usage.as_ref(), *elapsed_ms,
            )),
            ToolEvent::StreamText { .. }
            | ToolEvent::StreamReasoning { .. }
            | ToolEvent::StreamDone => FormattedOutput::Lines(vec![]),
            ToolEvent::ContextBudgetExceeded { usage_pct } => {
                FormattedOutput::Lines(vec![format!(
                    "    {} {}",
                    "⚠️  Context budget exceeded".with(Color::Red).attribute(Attribute::Bold),
                    format!("({}% used) — forcing final response", usage_pct)
                        .with(Color::DarkGrey),
                )])
            }
        }
    }

    fn next_start_tag(&mut self) -> String {
        self.next_index += 1;
        if self.round == 0 {
            format!("[{}]", self.next_index)
        } else {
            format!("[{}.{}]", self.round, self.next_index)
        }
    }

    // ── Compact LLM response (方案3) ──

    fn format_llm_response(
        &self,
        _round: usize,
        reasoning: Option<&str>,
        content: &str,
        usage: Option<&crate::llm::TokenUsage>,
        elapsed_ms: u64,
    ) -> Vec<String> {
        let mut lines = Vec::new();
        let elapsed_str = format_elapsed(elapsed_ms);

        // Collapsed thinking: single line with line count
        if let Some(r) = reasoning {
            if !r.is_empty() {
                let line_count = r.lines().count();
                let first_line = r.lines().next().unwrap_or("");
                let preview = truncate_chars(first_line, 80);
                lines.push(format!(
                    "  {} {} {}",
                    "💭".to_string(),
                    format!("\"{}\"", preview).with(Color::DarkGrey),
                    format!("({} lines)", line_count).with(Color::DarkGrey),
                ));
            }
        }

        // Collapsed LLM output: single line with preview
        if !content.is_empty() {
            let line_count = content.lines().count();
            let first_line = content.lines().next().unwrap_or("");
            let preview = truncate_chars(first_line, 80);
            lines.push(format!(
                "  {} {} {}",
                "📝".to_string(),
                format!("\"{}\"", preview).with(Color::Grey),
                format!("({} lines)", line_count).with(Color::DarkGrey),
            ));
        }

        // Compact token stats line
        {
            let mut parts: Vec<String> = if let Some(u) = usage {
                super::format_token_parts(u, None)
            } else {
                Vec::new()
            };
            parts.push(format!("llm {}", elapsed_str));
            lines.push(format!(
                "    {}",
                parts.join(" · ").with(Color::DarkGrey),
            ));
        }

        lines
    }

    // ── Compact round header (方案4) ──

    fn format_round_start(&mut self, round: usize) -> Vec<String> {
        self.round = round;
        self.next_index = 0;
        self.pending_tags.clear();
        self.has_pending_inline = false;
        vec![format!(
            "  {} {}",
            format!("── round {} ", round).with(Color::Cyan).attribute(Attribute::Bold),
            "─".repeat(40).with(Color::DarkGrey),
        )]
    }

    // ── Inline tool call start (方案1) ──

    fn format_call_start(
        &mut self,
        tool_name: &str,
        _source: &str,
        arguments: &serde_json::Value,
    ) -> FormattedOutput {
        let tag = self.next_start_tag();
        self.pending_tags.push_back(tag.clone());

        // Build a compact single-line summary: "▸ [1.1] read_file  src/main.rs"
        let summary = summarize_tool_args(tool_name, arguments)
            .map(|s| {
                // For bash, take only the first line (the $ command)
                let first = s.lines().next().unwrap_or(&s);
                format!("  {}", truncate_chars(first, 80))
            })
            .unwrap_or_default();

        // For edit tools, include a tiny diff hint
        let diff_hint = edit_diff_preview(tool_name, arguments);
        let diff_suffix = if !diff_hint.is_empty() {
            // Just show "(-old / +new)" on the same line
            String::new() // diff preview is too wide for inline; skip in compact mode
        } else {
            String::new()
        };

        let line = format!(
            "  {}  {} {}{}{}",
            "▸".with(Color::Yellow),
            tag.as_str().with(Color::Yellow).attribute(Attribute::Bold),
            tool_name.with(Color::White).attribute(Attribute::Bold),
            summary.with(Color::DarkGrey),
            diff_suffix,
        );

        self.has_pending_inline = true;
        FormattedOutput::InlineProgress(line)
    }

    // ── Inline tool call complete (方案1) ──

    fn format_call_complete(
        &mut self,
        tool_name: &str,
        success: bool,
        result_content: &str,
        elapsed_ms: u64,
    ) -> Vec<String> {
        let (icon, color) = if success { ("✓", Color::Green) } else { ("✗", Color::Red) };
        let tag_prefix = self.pending_tags
            .pop_front()
            .map(|t| format!("{} ", t))
            .unwrap_or_default();
        let elapsed_str = format_elapsed(elapsed_ms);
        self.has_pending_inline = false;

        if success {
            let line_count = result_content.lines().count();
            // Overwrite the inline progress line with the final status
            vec![format!(
                "  {}  {}{}{}",
                icon.with(color),
                tag_prefix.as_str().with(color).attribute(Attribute::Bold),
                tool_name.with(Color::White),
                format!("  ({} lines, {})", line_count, elapsed_str).with(Color::DarkGrey),
            )]
        } else {
            let first_line = result_content.lines().next().unwrap_or("");
            vec![format!(
                "  {}  {}{}: {} {}",
                icon.with(color),
                tag_prefix.as_str().with(color).attribute(Attribute::Bold),
                tool_name.with(color),
                truncate_chars(first_line, 80).with(Color::DarkGrey),
                format!("({})", elapsed_str).with(Color::DarkGrey),
            )]
        }
    }

    // ── Compact round complete (方案4) ──

    fn format_round_complete(tool_count: usize, elapsed_ms: u64) -> Vec<String> {
        let elapsed_str = format_elapsed(elapsed_ms);
        // Single compact line, no extra blank line
        vec![format!(
            "    {}",
            format!("{} calls, {}", tool_count, elapsed_str).with(Color::DarkGrey),
        )]
    }

    fn format_subagent_start(agent_name: &str, task_preview: &str) -> Vec<String> {
        let preview = truncate_chars(task_preview, 100);
        vec![
            format!(
                "  {} {} {} {}",
                "\u{1F916}".to_string(),
                format!("Subagent '{}'", agent_name)
                    .with(Color::Magenta)
                    .attribute(Attribute::Bold),
                "—".with(Color::DarkGrey),
                preview.with(Color::DarkGrey),
            ),
        ]
    }

    fn format_subagent_complete(
        agent_name: &str,
        success: bool,
        tool_rounds: usize,
        _result_preview: &str,
        usage: Option<&crate::llm::TokenUsage>,
        elapsed_ms: u64,
    ) -> Vec<String> {
        let (icon, color) = if success { ("✓", Color::Green) } else { ("✗", Color::Red) };
        let elapsed_str = format_elapsed(elapsed_ms);

        let token_info = if let Some(u) = usage {
            let parts = super::format_token_parts(u, None);
            if parts.is_empty() {
                String::new()
            } else {
                format!(", {}", parts.join(" · "))
            }
        } else {
            String::new()
        };

        vec![format!(
            "  {} {} {}",
            icon.with(color),
            format!("Subagent '{}'", agent_name).with(color),
            format!("({} rounds, {}{})", tool_rounds, elapsed_str, token_info).with(Color::DarkGrey),
        )]
    }
}

/// Format elapsed milliseconds into a human-readable string.
fn format_elapsed(ms: u64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

/// Render a tool execution event to stdout (interactive REPL mode).
#[allow(dead_code)]
pub(in crate::cli) fn tool_event(event: &ToolEvent) {
    for line in format_tool_event_lines(event) {
        println!("{}", line);
    }
}
