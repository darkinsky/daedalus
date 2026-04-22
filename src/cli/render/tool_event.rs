//! Stateful tool-event → styled terminal lines conversion.
//!
//! The [`ToolEventFormatter`] is held by each callback site (REPL spinner,
//! `--print` text-mode stderr) so concurrent tool calls inside one round can
//! be paired up visually via the `[round.index]` tag.

use std::collections::VecDeque;

use chrono::Local;
use crossterm::style::{Attribute, Color, Stylize};

use crate::tools::{truncate_chars, ToolEvent};

use super::summarize::{edit_diff_preview, summarize_tool_args};

/// Render a tool-execution event into fully styled terminal lines.
///
/// Stateless convenience wrapper over [`ToolEventFormatter`]. Callers that
/// don't care about per-call numbering (one-shot renders, tests) can use
/// this; interactive callbacks should hold a long-lived formatter instead.
#[allow(dead_code)]
pub(super) fn format_tool_event_lines(event: &ToolEvent) -> Vec<String> {
    ToolEventFormatter::new().format(event)
}

/// Stateful formatter that assigns a `[round.index]` tag to every
/// `ToolCallStart` and re-emits the same tag on the matching
/// `ToolCallComplete`, so users can visually pair them up even when the
/// agent runs several tools in parallel.
///
/// The matching is order-based: inside one round the executor emits
/// completions in the exact same order as the starts (see
/// `agent::chat::execute_tool_round` and `subagent::runner`), so a FIFO
/// queue is sufficient.
pub(in crate::cli) struct ToolEventFormatter {
    /// 1-based current round number. `0` means "no round seen yet".
    round: usize,
    /// 1-based index of the next `ToolCallStart` within the current round.
    next_index: usize,
    /// Tags waiting to be paired with their `ToolCallComplete`.
    pending_tags: VecDeque<String>,
}

impl ToolEventFormatter {
    pub(in crate::cli) fn new() -> Self {
        Self { round: 0, next_index: 0, pending_tags: VecDeque::new() }
    }

    /// Render a single event, updating internal numbering state.
    pub(in crate::cli) fn format(&mut self, event: &ToolEvent) -> Vec<String> {
        match event {
            ToolEvent::RoundStart { round } => self.format_round_start(*round),
            ToolEvent::ToolCallStart { tool_name, source, arguments } => {
                self.format_call_start(tool_name, source, arguments)
            }
            ToolEvent::ToolCallComplete { tool_name, success, result_content, elapsed_ms } => {
                self.format_call_complete(tool_name, *success, result_content, *elapsed_ms)
            }
            ToolEvent::RoundComplete { tool_count, elapsed_ms } => Self::format_round_complete(*tool_count, *elapsed_ms),
            ToolEvent::LlmResponse { round, reasoning, content, usage, elapsed_ms } => {
                self.format_llm_response(*round, reasoning.as_deref(), content, usage.as_ref(), *elapsed_ms)
            }
            ToolEvent::SubagentStart { agent_name, task_preview } => {
                Self::format_subagent_start(agent_name, task_preview)
            }
            ToolEvent::SubagentComplete {
                agent_name,
                success,
                tool_rounds,
                result_preview,
                usage,
                elapsed_ms,
            } => Self::format_subagent_complete(
                agent_name,
                *success,
                *tool_rounds,
                result_preview,
                usage.as_ref(),
                *elapsed_ms,
            ),
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

    fn format_llm_response(
        &self,
        round: usize,
        reasoning: Option<&str>,
        content: &str,
        usage: Option<&crate::llm::TokenUsage>,
        elapsed_ms: u64,
    ) -> Vec<String> {
        let mut lines = Vec::new();
        let ts = Local::now().format("%H:%M:%S");
        let elapsed_str = format_elapsed(elapsed_ms);

        // Show reasoning/thinking if present
        if let Some(r) = reasoning {
            if !r.is_empty() {
                lines.push(String::new());
                lines.push(format!(
                    "  {} {} {}",
                    "💭".to_string(),
                    format!("Thinking (round {})", round)
                        .with(Color::DarkGrey)
                        .attribute(Attribute::Italic),
                    format!("[{} | {}]", ts, elapsed_str).with(Color::DarkGrey),
                ));
                // Show up to 8 lines of reasoning, truncated
                let reasoning_lines: Vec<&str> = r.lines().collect();
                let show_count = reasoning_lines.len().min(8);
                for line in &reasoning_lines[..show_count] {
                    lines.push(format!(
                        "  {}  {}",
                        "┊".with(Color::DarkGrey),
                        truncate_chars(line, 120).with(Color::DarkGrey),
                    ));
                }
                if reasoning_lines.len() > 8 {
                    lines.push(format!(
                        "  {}  {}",
                        "┊".with(Color::DarkGrey),
                        format!("... ({} more lines)", reasoning_lines.len() - 8)
                            .with(Color::DarkGrey),
                    ));
                }
            }
        }

        // Show content if non-empty (intermediate LLM text)
        if !content.is_empty() {
            lines.push(format!(
                "  {} {}",
                "📝".to_string(),
                format!("LLM output (round {})", round)
                    .with(Color::White)
                    .attribute(Attribute::Italic),
            ));
            let content_lines: Vec<&str> = content.lines().collect();
            let show_count = content_lines.len().min(4);
            for line in &content_lines[..show_count] {
                lines.push(format!(
                    "  {}  {}",
                    "│".with(Color::DarkGrey),
                    truncate_chars(line, 120).with(Color::Grey),
                ));
            }
            if content_lines.len() > 4 {
                lines.push(format!(
                    "  {}  {}",
                    "│".with(Color::DarkGrey),
                    format!("... ({} more lines)", content_lines.len() - 4)
                        .with(Color::DarkGrey),
                ));
            }
        }

        // Show per-round token usage and LLM elapsed time
        {
            let mut parts: Vec<String> = if let Some(u) = usage {
                super::format_token_parts(u)
            } else {
                Vec::new()
            };
            parts.push(format!("llm {}", elapsed_str));
            lines.push(format!(
                "  {}",
                format!("  {}", parts.join(" · ")).with(Color::DarkGrey),
            ));
        }

        lines
    }

    fn format_round_start(&mut self, round: usize) -> Vec<String> {
        self.round = round;
        self.next_index = 0;
        self.pending_tags.clear();
        let ts = Local::now().format("%H:%M:%S");
        vec![format!(
            "  🔧 {} {}",
            format!("Tool round {}", round)
                .with(Color::Cyan)
                .attribute(Attribute::Bold),
            format!("[{}]", ts).with(Color::DarkGrey),
        )]
    }

    fn format_call_start(
        &mut self,
        tool_name: &str,
        source: &str,
        arguments: &serde_json::Value,
    ) -> Vec<String> {
        let tag = self.next_start_tag();
        self.pending_tags.push_back(tag.clone());
        let mut lines = Vec::new();
        lines.push(format!(
            "  {}  {} {} {}",
            "▸".with(Color::Yellow),
            tag.as_str().with(Color::Yellow).attribute(Attribute::Bold),
            tool_name.with(Color::White).attribute(Attribute::Bold),
            format!("({})", source).with(Color::DarkGrey),
        ));
        if let Some(summary) = summarize_tool_args(tool_name, arguments) {
            lines.extend(style_summary_block(tool_name, &summary));
        }
        for diff_line in edit_diff_preview(tool_name, arguments) {
            lines.push(format!("      {}", diff_line));
        }
        lines
    }

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
        let mut lines = Vec::new();
        if success {
            let content_lines: Vec<&str> = result_content.lines().collect();
            let line_count = content_lines.len();
            lines.push(format!(
                "    {} {}{}",
                icon.with(color),
                tag_prefix.as_str().with(color).attribute(Attribute::Bold),
                format!("{} ({} lines, {})", tool_name, line_count, elapsed_str).with(Color::DarkGrey),
            ));
            // Tool result content is hidden by default in interactive mode.
            // Use tracing/file collector to inspect full tool outputs.
        } else {
            let first_line = result_content.lines().next().unwrap_or("");
            lines.push(format!(
                "    {} {}{}{} {}",
                icon.with(color),
                tag_prefix.as_str().with(color).attribute(Attribute::Bold),
                format!("{}: ", tool_name).with(color),
                first_line.with(Color::DarkGrey),
                format!("({})", elapsed_str).with(Color::DarkGrey),
            ));
        }
        lines
    }

    fn format_round_complete(tool_count: usize, elapsed_ms: u64) -> Vec<String> {
        let ts = Local::now().format("%H:%M:%S");
        let elapsed_str = format_elapsed(elapsed_ms);
        vec![
            format!(
                "  {}",
                format!("  {} tool call(s) completed in {} [{}]", tool_count, elapsed_str, ts).with(Color::DarkGrey),
            ),
            String::new(),
        ]
    }

    fn format_subagent_start(agent_name: &str, task_preview: &str) -> Vec<String> {
        let preview = truncate_chars(task_preview, 100);
        vec![
            String::new(),
            format!(
                "  {} {} {}",
                "\u{1F916}".to_string(),
                format!("Subagent '{}' started", agent_name)
                    .with(Color::Magenta)
                    .attribute(Attribute::Bold),
                "—".with(Color::DarkGrey),
            ),
            format!("    {}", preview.with(Color::DarkGrey)),
            String::new(),
        ]
    }

    fn format_subagent_complete(
        agent_name: &str,
        success: bool,
        tool_rounds: usize,
        result_preview: &str,
        usage: Option<&crate::llm::TokenUsage>,
        elapsed_ms: u64,
    ) -> Vec<String> {
        let (icon, color) = if success { ("✓", Color::Green) } else { ("✗", Color::Red) };
        let elapsed_str = format_elapsed(elapsed_ms);

        // Build token info string
        let token_info = if let Some(u) = usage {
            let parts = super::format_token_parts(u);
            if parts.is_empty() {
                String::new()
            } else {
                format!(", {}", parts.join(" · "))
            }
        } else {
            String::new()
        };

        let mut lines = vec![format!(
            "  {} {} {}",
            icon.with(color),
            format!("Subagent '{}' completed", agent_name).with(color),
            format!("({} tool rounds, {}{})", tool_rounds, elapsed_str, token_info).with(Color::DarkGrey),
        )];
        let preview = truncate_chars(result_preview, 120);
        if !preview.is_empty() {
            lines.push(format!("    {}", preview.with(Color::DarkGrey)));
        }
        lines.push(String::new());
        lines
    }
}

/// Format elapsed milliseconds into a human-readable string.
///
/// - < 1000ms → "123ms"
/// - >= 1000ms → "1.2s"
fn format_elapsed(ms: u64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

/// Style a multi-line argument summary. Bash commands get shell-style
/// colouring (cyan `$`, green body, dim brackets for `[cwd:]`/`[timeout:]`);
/// everything else uses dim grey.
fn style_summary_block(tool_name: &str, summary: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let is_bash = tool_name == "bash";
    for (idx, raw_line) in summary.split('\n').enumerate() {
        if is_bash {
            if idx == 0
                && let Some(rest) = raw_line.strip_prefix("$ ")
            {
                lines.push(format!(
                    "      {} {}",
                    "$".with(Color::Cyan).attribute(Attribute::Bold),
                    rest.with(Color::Green),
                ));
                continue;
            }
            let trimmed = raw_line.trim_start();
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                lines.push(format!("      {}", raw_line.with(Color::Grey)));
            } else {
                lines.push(format!("      {}", raw_line.with(Color::Green)));
            }
        } else {
            lines.push(format!("      {}", raw_line.with(Color::Grey)));
        }
    }
    lines
}

/// Render a tool execution event to stdout (interactive REPL mode).
///
/// Called in real-time during the tool-calling loop to show progress.
#[allow(dead_code)]
pub(in crate::cli) fn tool_event(event: &ToolEvent) {
    for line in format_tool_event_lines(event) {
        println!("{}", line);
    }
}
