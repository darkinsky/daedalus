//! Stateful tool-event → styled terminal output conversion.
//!
//! The [`ToolEventFormatter`] is held by each callback site (REPL spinner,
//! `--print` text-mode stderr) so concurrent tool calls inside one round can
//! be paired up visually via the `[round.index]` tag.
//!
//! ## Output modes
//!
//! - **Compact** (default): Tool calls use a **multi-line status area** that
//!   refreshes in-place (like bazel's build status). `ToolCallStart` adds a
//!   line to the status area; `ToolCallComplete` removes it and "graduates"
//!   the completed line above the refreshable region. The status area is
//!   erased and redrawn using ANSI cursor-up (`\x1B[nA`) + clear-line
//!   (`\x1B[2K`) sequences. Thinking/LLM output collapsed to single lines.
//! - **Verbose** (`--verbose` / `DAEDALUS_VERBOSE=1`): Full multi-line output
//!   with expanded thinking, tool argument details, diff previews, and
//!   per-round token statistics.

use std::collections::VecDeque;

use chrono::Local;
use crossterm::style::{Attribute, Color, Stylize};

use crate::tools::{truncate_chars, ToolEvent};

use super::summarize::{edit_diff_preview, summarize_tool_args};

/// Output produced by the formatter for a single event.
///
/// Most events produce `Lines` (printed with `println!`). In compact mode,
/// tool calls use `StatusAreaUpdate` — a multi-line region that is redrawn
/// in-place using ANSI cursor movement (like bazel's build status area).
pub(in crate::cli) enum FormattedOutput {
    /// Normal lines to print with `println!`.
    Lines(Vec<String>),
    /// A single in-progress line to print with `print!` (no trailing newline).
    /// Only used in compact mode for non-tool events that need inline display.
    #[allow(dead_code)]
    InlineProgress(String),
    /// Multi-line status area update (compact mode only).
    ///
    /// The caller should erase the previous status area (using `prev_area_lines`
    /// to know how many lines to move up and clear), then print the new lines.
    /// Completed tool lines are "graduated" — printed permanently above the
    /// status area so they don't get erased on the next update.
    StatusAreaUpdate {
        /// Lines that have just completed and should be printed permanently
        /// (above the refreshable area). These are "graduated" from the status.
        graduated_lines: Vec<String>,
        /// Current in-progress tool lines (the refreshable status area).
        /// Printed without final newline on the last line if `active_count > 0`.
        active_lines: Vec<String>,
        /// How many terminal lines the *previous* status area occupied.
        /// The caller uses this to move the cursor up and erase before reprinting.
        prev_area_lines: usize,
    },
}

/// Render a tool-execution event into fully styled terminal lines.
///
/// Stateless convenience wrapper over [`ToolEventFormatter`].
#[allow(dead_code)]
pub(super) fn format_tool_event_lines(event: &ToolEvent) -> Vec<String> {
    match ToolEventFormatter::new().format(event) {
        FormattedOutput::Lines(lines) => lines,
        FormattedOutput::InlineProgress(line) => vec![line],
        FormattedOutput::StatusAreaUpdate { graduated_lines, active_lines, .. } => {
            [graduated_lines, active_lines].concat()
        }
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
    /// Status area state for compact mode multi-line refresh.
    status_area: StatusArea,
}

/// Tracks the in-place refreshable status area for compact mode.
///
/// Models a bazel-like status region at the bottom of the terminal output:
/// completed tools "graduate" upward as permanent lines, while in-progress
/// tools occupy a refreshable area that is redrawn on each event.
struct StatusArea {
    /// Currently active (in-progress) tools: (tag, tool_name, summary).
    active: Vec<(String, String, String)>,
    /// Number of terminal lines the status area occupied on the last render.
    /// Used to calculate how far to move the cursor up when erasing.
    rendered_lines: usize,
}

impl StatusArea {
    fn new() -> Self {
        Self {
            active: Vec::new(),
            rendered_lines: 0,
        }
    }

    fn reset(&mut self) {
        self.active.clear();
        self.rendered_lines = 0;
    }

    /// Add an active tool and return a StatusAreaUpdate.
    fn add_active(
        &mut self,
        tag: String,
        tool_name: String,
        summary: String,
    ) -> FormattedOutput {
        let prev = self.rendered_lines;
        self.active.push((tag, tool_name, summary));
        let active_lines = self.render_active_lines();
        self.rendered_lines = active_lines.len();
        FormattedOutput::StatusAreaUpdate {
            graduated_lines: vec![],
            active_lines,
            prev_area_lines: prev,
        }
    }

    /// Complete a tool (by tag) and return a StatusAreaUpdate with the
    /// completed line graduated.
    fn complete_tool(
        &mut self,
        tag: &str,
        completed_line: String,
    ) -> FormattedOutput {
        let prev = self.rendered_lines;
        // Remove the matching active entry
        if let Some(pos) = self.active.iter().position(|(t, _, _)| t == tag) {
            self.active.remove(pos);
        }
        let active_lines = self.render_active_lines();
        self.rendered_lines = active_lines.len();
        FormattedOutput::StatusAreaUpdate {
            graduated_lines: vec![completed_line],
            active_lines,
            prev_area_lines: prev,
        }
    }

    /// Render the current active tools as styled lines.
    fn render_active_lines(&self) -> Vec<String> {
        self.active
            .iter()
            .map(|(tag, tool_name, summary)| {
                let summary_part = if summary.is_empty() {
                    String::new()
                } else {
                    format!("  {}", summary).with(Color::Grey).to_string()
                };
                format!(
                    "  {}  {} {}{}",
                    "▸".with(Color::Yellow),
                    tag.as_str().with(Color::Yellow).attribute(Attribute::Bold),
                    tool_name.as_str().with(Color::White).attribute(Attribute::Bold),
                    summary_part,
                )
            })
            .collect()
    }
}

impl ToolEventFormatter {
    pub(in crate::cli) fn new() -> Self {
        Self {
            round: 0,
            next_index: 0,
            pending_tags: VecDeque::new(),
            has_pending_inline: false,
            status_area: StatusArea::new(),
        }
    }

    /// Check if verbose output mode is enabled.
    fn verbose(&self) -> bool {
        crate::cli::is_verbose()
    }

    /// Render a single event, updating internal numbering state.
    pub(in crate::cli) fn format(&mut self, event: &ToolEvent) -> FormattedOutput {
        let output = match event {
            ToolEvent::RoundStart { round } => FormattedOutput::Lines(self.format_round_start(*round)),
            ToolEvent::ToolCallStart { tool_name, source, arguments } => {
                self.format_call_start(tool_name, source, arguments)
            }
            ToolEvent::ToolCallComplete { tool_name, success, result_content, elapsed_ms } => {
                self.format_call_complete(tool_name, *success, result_content, *elapsed_ms)
            }
            ToolEvent::RoundComplete { tool_count, elapsed_ms } => {
                self.format_round_complete(*tool_count, *elapsed_ms)
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
        };

        // If the output is `Lines` but there's an active status area,
        // convert to `StatusAreaUpdate` so the caller erases the stale
        // status lines before printing the new content.
        if let FormattedOutput::Lines(lines) = output {
            let prev = self.status_area.rendered_lines;
            if prev > 0 {
                self.status_area.reset();
                FormattedOutput::StatusAreaUpdate {
                    graduated_lines: lines,
                    active_lines: vec![],
                    prev_area_lines: prev,
                }
            } else {
                FormattedOutput::Lines(lines)
            }
        } else {
            output
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

    // ── LLM response ──

    fn format_llm_response(
        &self,
        round: usize,
        reasoning: Option<&str>,
        content: &str,
        usage: Option<&crate::llm::TokenUsage>,
        elapsed_ms: u64,
    ) -> Vec<String> {
        if self.verbose() {
            self.format_llm_response_verbose(round, reasoning, content, usage, elapsed_ms)
        } else {
            self.format_llm_response_compact(reasoning, content, usage, elapsed_ms)
        }
    }

    fn format_llm_response_compact(
        &self,
        reasoning: Option<&str>,
        content: &str,
        usage: Option<&crate::llm::TokenUsage>,
        elapsed_ms: u64,
    ) -> Vec<String> {
        let mut lines = Vec::new();
        let elapsed_str = format_elapsed(elapsed_ms);

        // Collapsed thinking: single line with preview
        if let Some(r) = reasoning {
            if !r.is_empty() {
                let line_count = r.lines().count();
                let first_line = r.lines().next().unwrap_or("");
                let preview = truncate_chars(first_line, 80);
                lines.push(format!(
                    "  {} {} {}",
                    "💭".to_string(),
                    format!("\"{}\"" , preview).with(Color::DarkCyan),
                    format!("({} lines)", line_count).with(Color::Grey),
                ));            }
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

        // Compact token stats
        {
            let mut parts: Vec<String> = if let Some(u) = usage {
                super::format_token_parts(u, None)
            } else {
                Vec::new()
            };
            parts.push(format!("llm {}", elapsed_str));
            lines.push(format!(
                "    {}",
                parts.join(" · ").with(Color::Grey),
            ));
        }

        lines
    }

    fn format_llm_response_verbose(
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

        // Expanded thinking with up to 8 lines
        if let Some(r) = reasoning {
            if !r.is_empty() {
                lines.push(String::new());
                lines.push(format!(
                    "  {} {} {}",
                    "💭".to_string(),
                    format!("Thinking (round {})", round)
                        .with(Color::DarkBlue)
                        .attribute(Attribute::Italic),
                    format!("[{} | {}]", ts, elapsed_str).with(Color::DarkGrey),
                ));
                let reasoning_lines: Vec<&str> = r.lines().collect();
                let show_count = reasoning_lines.len().min(8);
                for line in &reasoning_lines[..show_count] {
                    lines.push(format!(
                        "  {}  {}",
                        "┊".with(Color::DarkBlue),
                        truncate_chars(line, 120).with(Color::DarkCyan),
                    ));
                }
                if reasoning_lines.len() > 8 {
                    lines.push(format!(
                        "  {}  {}",
                        "┊".with(Color::DarkBlue),
                        format!("... ({} more lines)", reasoning_lines.len() - 8)
                            .with(Color::Grey),
                    ));
                }
            }
        }

        // Expanded LLM output with up to 4 lines
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
                    truncate_chars(line, 120).with(Color::White),
                ));
            }
            if content_lines.len() > 4 {
                lines.push(format!(
                    "  {}  {}",
                    "│".with(Color::DarkGrey),
                    format!("... ({} more lines)", content_lines.len() - 4)
                        .with(Color::Grey),
                ));
            }
        }

        // Token stats
        {
            let mut parts: Vec<String> = if let Some(u) = usage {
                super::format_token_parts(u, None)
            } else {
                Vec::new()
            };
            parts.push(format!("llm {}", elapsed_str));
            lines.push(format!(
                "  {}",
                format!("  {}", parts.join(" · ")).with(Color::Grey),
            ));
        }

        lines
    }

    // ── Round header ──

    fn format_round_start(&mut self, round: usize) -> Vec<String> {
        self.round = round;
        self.next_index = 0;
        self.pending_tags.clear();
        self.has_pending_inline = false;
        self.status_area.reset();

        if self.verbose() {
            let ts = Local::now().format("%H:%M:%S");
            vec![format!(
                "  🔧 {} {}",
                format!("Tool round {}", round)
                    .with(Color::Blue)
                    .attribute(Attribute::Bold),
                format!("[{}]", ts).with(Color::DarkGrey),
            )]
        } else {
            vec![format!(
                "  {} {}",
                format!("── round {} ", round).with(Color::Blue).attribute(Attribute::Bold),
                "─".repeat(40).with(Color::DarkGrey),
            )]
        }
    }

    // ── Tool call start ──

    /// Internal-only tools that should not appear in terminal output.
    /// Their tag bookkeeping is still maintained for queue consistency.
    const HIDDEN_TOOLS: &'static [&'static str] = &["take_note"];

    fn format_call_start(
        &mut self,
        tool_name: &str,
        source: &str,
        arguments: &serde_json::Value,
    ) -> FormattedOutput {
        let tag = self.next_start_tag();
        self.pending_tags.push_back(tag.clone());

        // Hide internal-only tools from terminal output
        if Self::HIDDEN_TOOLS.contains(&tool_name) {
            self.has_pending_inline = false;
            return FormattedOutput::Lines(vec![]);
        }

        if self.verbose() {
            self.has_pending_inline = false;
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
            FormattedOutput::Lines(lines)
        } else {
            // Compact: multi-line status area refresh (bazel-style)
            let summary = summarize_tool_args(tool_name, arguments)
                .map(|s| {
                    let first = s.lines().next().unwrap_or(&s);
                    truncate_chars(first, 80)
                })
                .unwrap_or_default();

            self.has_pending_inline = false;
            self.status_area.add_active(tag, tool_name.to_string(), summary)
        }
    }

    // ── Tool call complete ──

    fn format_call_complete(
        &mut self,
        tool_name: &str,
        success: bool,
        result_content: &str,
        elapsed_ms: u64,
    ) -> FormattedOutput {
        let (icon, color) = if success { ("✓", Color::Green) } else { ("✗", Color::Red) };
        let tag = self.pending_tags
            .pop_front()
            .unwrap_or_default();

        // Hide internal-only tools from terminal output
        if Self::HIDDEN_TOOLS.contains(&tool_name) {
            self.has_pending_inline = false;
            return FormattedOutput::Lines(vec![]);
        }
        let tag_prefix = if tag.is_empty() {
            String::new()
        } else {
            format!("{} ", tag)
        };
        let elapsed_str = format_elapsed(elapsed_ms);
        self.has_pending_inline = false;

        let completed_line = if success {
            let line_count = result_content.lines().count();
            format!(
                "  {}  {}{}{}",
                icon.with(color),
                tag_prefix.as_str().with(color).attribute(Attribute::Bold),
                tool_name.with(Color::White),
                format!("  ({} lines, {})", line_count, elapsed_str).with(Color::Grey),
            )
        } else {
            let first_line = result_content.lines().next().unwrap_or("");
            format!(
                "  {}  {}{}: {} {}",
                icon.with(color),
                tag_prefix.as_str().with(color).attribute(Attribute::Bold),
                tool_name.with(color),
                truncate_chars(first_line, 80).with(Color::Grey),
                format!("({})", elapsed_str).with(Color::Grey),
            )
        };

        if self.verbose() {
            FormattedOutput::Lines(vec![completed_line])
        } else {
            // Compact: graduate the completed tool from the status area
            self.status_area.complete_tool(&tag, completed_line)
        }
    }

    // ── Round complete ──

    fn format_round_complete(&mut self, tool_count: usize, elapsed_ms: u64) -> FormattedOutput {
        // Clear any remaining status area state
        let prev = self.status_area.rendered_lines;
        self.status_area.reset();

        let elapsed_str = format_elapsed(elapsed_ms);
        if crate::cli::is_verbose() {
            let ts = Local::now().format("%H:%M:%S");
            FormattedOutput::Lines(vec![
                format!(
                    "  {}",
                    format!("  {} tool call(s) completed in {} [{}]", tool_count, elapsed_str, ts).with(Color::Grey),
                ),
                String::new(),
            ])
        } else {
            // In compact mode, we need to erase any leftover status area lines
            // before printing the round summary.
            let summary_line = format!(
                "    {}",
                format!("{} calls, {}", tool_count, elapsed_str).with(Color::Grey),
            );
            if prev > 0 {
                FormattedOutput::StatusAreaUpdate {
                    graduated_lines: vec![summary_line],
                    active_lines: vec![],
                    prev_area_lines: prev,
                }
            } else {
                FormattedOutput::Lines(vec![summary_line])
            }
        }
    }

    // ── Subagent events ──

    fn format_subagent_start(agent_name: &str, task_preview: &str) -> Vec<String> {
        let preview = truncate_chars(task_preview, 100);
        if crate::cli::is_verbose() {
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
                format!("    {}", preview.with(Color::Grey)),
                String::new(),
            ]
        } else {
            vec![format!(
                "  {} {} {} {}",
                "\u{1F916}".to_string(),
                format!("Subagent '{}'", agent_name)
                    .with(Color::Magenta)
                    .attribute(Attribute::Bold),
                "—".with(Color::DarkGrey),
                preview.with(Color::Grey),
            )]
        }
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

        if crate::cli::is_verbose() {
            let mut lines = vec![format!(
                "  {} {} {}",
                icon.with(color),
                format!("Subagent '{}' completed", agent_name).with(color),
                format!("({} tool rounds, {}{})", tool_rounds, elapsed_str, token_info).with(Color::Grey),
            )];
            let preview = truncate_chars(result_preview, 120);
            if !preview.is_empty() {
                lines.push(format!("    {}", preview.with(Color::Grey)));
            }
            lines.push(String::new());
            lines
        } else {
            vec![format!(
                "  {} {} {}",
                icon.with(color),
                format!("Subagent '{}'", agent_name).with(color),
                format!("({} rounds, {}{})", tool_rounds, elapsed_str, token_info).with(Color::Grey),
            )]
        }
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

/// Style a multi-line argument summary (verbose mode only).
///
/// Bash commands get shell-style colouring; everything else uses dim grey.
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
#[allow(dead_code)]
pub(in crate::cli) fn tool_event(event: &ToolEvent) {
    for line in format_tool_event_lines(event) {
        println!("{}", line);
    }
}
