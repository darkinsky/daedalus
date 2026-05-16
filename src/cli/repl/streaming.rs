//! Streaming state management and tool event callback construction.
//!
//! Contains the `StreamingState` state machine, subagent stats collection,
//! and the `build_tool_event_callback` factory that wires spinner, streaming,
//! and tool event rendering together.

use std::sync::{Arc, Mutex};

use crate::llm::TokenUsage;
use crate::tools::{ToolEvent, ToolEventCallback};
use super::super::render;
use super::super::render::ToolEventFormatter;
use super::super::render::tool_event::FormattedOutput;

/// Tracks the state of streaming output within a single turn.
#[derive(Debug, Default)]
pub(crate) struct StreamingState {
    /// Whether the response header has been printed for the current stream.
    pub header_printed: bool,
    /// Whether the reasoning header has been printed.
    pub reasoning_header_printed: bool,
    /// Whether we are currently in the middle of streaming output.
    pub is_streaming: bool,
    /// Whether any content was streamed during this turn.
    /// Used by `handle_chat` to decide whether to re-render with markdown.
    pub content_was_streamed: bool,
    /// Whether any reasoning was streamed during this turn.
    pub reasoning_was_streamed: bool,
}

/// Statistics collected for a single subagent during a turn.
#[derive(Debug, Clone)]
pub(crate) struct SubagentStats {
    pub agent_name: String,
    pub success: bool,
    pub tool_rounds: usize,
    pub usage: Option<TokenUsage>,
    pub elapsed_ms: u64,
}

/// Collector for subagent statistics during a single turn.
///
/// Shared between the tool event callback and the REPL handler via `Arc<Mutex<_>>`.
#[derive(Debug, Default)]
pub(crate) struct TurnStatsCollector {
    pub subagents: Vec<SubagentStats>,
}

/// Build the tool event callback that renders tool progress to the terminal.
///
/// The callback clears the spinner before printing tool events, then
/// restarts it for the next LLM thinking phase.
///
/// A [`ToolEventFormatter`] is kept alive across events so per-tool-call
/// tags (e.g. `[1.2]`) emitted on `ToolCallStart` match the tags emitted
/// on the corresponding `ToolCallComplete`, even when several tools run
/// in parallel within a single round.
///
/// The `stats_collector` captures subagent completion events so the REPL
/// can display a combined turn summary at the end.
pub(crate) fn build_tool_event_callback(
    spinner: &Arc<indicatif::ProgressBar>,
    stats_collector: &Arc<Mutex<TurnStatsCollector>>,
    streaming_state: &Arc<Mutex<StreamingState>>,
) -> ToolEventCallback {
    let spinner = Arc::clone(spinner);
    let formatter = Arc::new(Mutex::new(ToolEventFormatter::new()));
    let collector = Arc::clone(stats_collector);
    let streaming_state = Arc::clone(streaming_state);
    Arc::new(move |event| {
        // Capture subagent completion stats before rendering
        if let ToolEvent::SubagentComplete {
            ref agent_name,
            success,
            tool_rounds,
            ref usage,
            elapsed_ms,
            ..
        } = event
        {
            if let Ok(mut stats) = collector.lock() {
                stats.subagents.push(SubagentStats {
                    agent_name: agent_name.clone(),
                    success,
                    tool_rounds,
                    usage: usage.clone(),
                    elapsed_ms,
                });
            }
        }

        // Handle streaming events without clearing the spinner
        match &event {
            ToolEvent::StreamText { text } => {
                let mut state = streaming_state.lock().expect("streaming state poisoned");
                if !state.header_printed {
                    spinner.finish_and_clear();
                    state.header_printed = true;
                    state.is_streaming = true;
                    state.content_was_streamed = true;
                    render::stream_response_header();
                }
                render::stream_text_chunk(text);
                return;
            }
            ToolEvent::StreamReasoning { text } => {
                let mut state = streaming_state.lock().expect("streaming state poisoned");
                if !state.reasoning_header_printed {
                    spinner.finish_and_clear();
                    state.reasoning_header_printed = true;
                    state.is_streaming = true;
                    state.reasoning_was_streamed = true;
                    render::stream_reasoning_header();
                }
                render::stream_reasoning_chunk(text);
                return;
            }
            ToolEvent::StreamDone => {
                let mut state = streaming_state.lock().expect("streaming state poisoned");
                if state.is_streaming {
                    render::stream_done();
                    state.is_streaming = false;
                    state.header_printed = false;
                    state.reasoning_header_printed = false;
                }
                return;
            }
            _ => {
                let mut state = streaming_state.lock().expect("streaming state poisoned");
                if state.is_streaming {
                    render::stream_done();
                    state.is_streaming = false;
                    state.header_printed = false;
                    state.reasoning_header_printed = false;
                }
            }
        }

        // Pause the spinner so tool output is not interleaved
        spinner.finish_and_clear();
        let output = {
            let mut fmt = formatter.lock().expect("tool event formatter poisoned");
            if fmt.has_pending_inline {
                if !matches!(event, ToolEvent::ToolCallComplete { .. }) {
                    println!();
                    fmt.has_pending_inline = false;
                }
            }
            fmt.format(&event)
        };
        match output {
            FormattedOutput::InlineProgress(line) => {
                use std::io::Write;
                print!("\r\x1B[2K{}", line);
                let _ = std::io::stdout().flush();
            }
            FormattedOutput::StatusAreaUpdate {
                graduated_lines,
                active_lines,
                prev_area_lines,
            } => {
                use std::io::Write;
                if prev_area_lines > 0 {
                    print!("\x1B[{}A", prev_area_lines);
                    for _ in 0..prev_area_lines {
                        print!("\x1B[2K\n");
                    }
                    print!("\x1B[{}A", prev_area_lines);
                }
                for line in &graduated_lines {
                    println!("{}", line);
                }
                for line in &active_lines {
                    println!("{}", line);
                }
                let _ = std::io::stdout().flush();
            }
            FormattedOutput::Lines(lines) => {
                for line in &lines {
                    println!("{}", line);
                }
            }
        }
        // Restart spinner for the next LLM round
        spinner.reset_elapsed();
        spinner.set_message("Thinking\u{2026}");
        spinner.enable_steady_tick(std::time::Duration::from_millis(80));
    })
}
