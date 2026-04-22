use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use crossterm::style::{Attribute, Color, Stylize};
use rustyline::error::ReadlineError;
use rustyline::{CompletionType, Config, EditMode, Editor};

use crate::agent::AgentMode;
use crate::llm::TokenUsage;
use crate::middleware::builtin::cost::{SessionCost, SharedSessionCost};
use crate::tools::{ToolEvent, ToolEventCallback};
use super::commands::{self, Command};
use super::completer::SlashCommandHelper;
use super::render;
use super::render::ToolEventFormatter;

/// Handle a parsed slash command. Returns `true` if the REPL should exit.
fn handle_command(cmd: Command<'_>, agent: &mut dyn AgentMode, cost: &SharedSessionCost) -> Result<bool> {
    match cmd {
        Command::Exit => {
            render::goodbye();
            return Ok(true);
        }
        Command::Help => render::help(),
        Command::NewSession => {
            agent.new_session();
            if let Ok(mut c) = cost.lock() {
                c.reset();
            }
            render::new_session(agent);
        }
        Command::Clear => {
            print!("\x1B[2J\x1B[1;1H");
            std::io::Write::flush(&mut std::io::stdout())?;
            render::screen_cleared(agent);
        }
        Command::Cost => {
            if let Ok(c) = cost.lock() {
                render::cost(&c);
            }
        }
        Command::Model => render::model_info(agent),
        Command::Tools => render::tools_list(agent),
        Command::Skills => render::skills_list(agent),
        Command::Agents => render::agents_list(agent),
        Command::Unknown(raw) => render::unknown_command(raw),
    }
    Ok(false)
}

/// Statistics collected for a single subagent during a turn.
#[derive(Debug, Clone)]
struct SubagentStats {
    agent_name: String,
    success: bool,
    tool_rounds: usize,
    usage: Option<TokenUsage>,
    elapsed_ms: u64,
}

/// Collector for subagent statistics during a single turn.
///
/// Shared between the tool event callback and the REPL handler via `Arc<Mutex<_>>`.
#[derive(Debug, Default)]
struct TurnStatsCollector {
    subagents: Vec<SubagentStats>,
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
fn build_tool_event_callback(
    spinner: &Arc<indicatif::ProgressBar>,
    stats_collector: &Arc<Mutex<TurnStatsCollector>>,
    streaming_state: &Arc<Mutex<StreamingState>>,
) -> ToolEventCallback {
    let spinner = Arc::clone(spinner);
    let formatter = Arc::new(Mutex::new(ToolEventFormatter::new()));
    let collector = Arc::clone(stats_collector);
    // Track streaming state: whether we've started streaming and need
    // to handle the transition between streaming output and tool events.
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
                    // First streaming chunk: clear spinner and print header
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
                    // Reset for next round (tool calls may follow)
                    state.header_printed = false;
                    state.reasoning_header_printed = false;
                }
                return;
            }
            _ => {
                // Non-streaming event: if we were streaming, finish the stream first
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
        let rendered = {
            let mut fmt = formatter.lock().expect("tool event formatter poisoned");
            fmt.format(&event)
        };
        for line in rendered {
            println!("{}", line);
        }
        // Restart spinner for the next LLM round
        spinner.reset_elapsed();
        spinner.set_message("Thinking\u{2026}");
        spinner.enable_steady_tick(std::time::Duration::from_millis(80));
    })
}

/// Tracks the state of streaming output within a single turn.
#[derive(Debug, Default)]
struct StreamingState {
    /// Whether the response header has been printed for the current stream.
    header_printed: bool,
    /// Whether the reasoning header has been printed.
    reasoning_header_printed: bool,
    /// Whether we are currently in the middle of streaming output.
    is_streaming: bool,
    /// Whether any content was streamed during this turn.
    /// Used by `handle_chat` to decide whether to re-render with markdown.
    content_was_streamed: bool,
    /// Whether any reasoning was streamed during this turn.
    reasoning_was_streamed: bool,
}
/// Send user input to the agent and render the response.
async fn handle_chat(input: &str, agent: &mut dyn AgentMode, cost: &SharedSessionCost) {
    tracing::debug!("User input: {}", input);

    // Show spinner while waiting for LLM response
    let spinner = Arc::new(render::spinner());
    let start = Instant::now();

    // Collector for subagent statistics
    let stats_collector = Arc::new(Mutex::new(TurnStatsCollector::default()));

    // Shared streaming state so handle_chat can check if content was streamed
    let streaming_state = Arc::new(Mutex::new(StreamingState::default()));

    // Build tool event callback for real-time tool progress display
    let tool_callback = build_tool_event_callback(&spinner, &stats_collector, &streaming_state);

    // Set the subagent event callback so subagent tool events are also rendered
    agent.set_subagent_event_callback(Some(Arc::clone(&tool_callback)));

    match agent.chat(input, Some(&tool_callback)).await {
        Ok(result) => {
            let elapsed = start.elapsed().as_secs_f64();
            spinner.finish_and_clear();

            // Clear the subagent event callback
            agent.set_subagent_event_callback(None);

            // In streaming mode, the response text was already printed
            // incrementally via StreamText events. We skip the full markdown
            // re-render to avoid duplicate output. The streamed text is raw
            // (no markdown), which is acceptable for real-time display.
            let was_streamed = streaming_state
                .lock()
                .map(|s| s.content_was_streamed)
                .unwrap_or(false);
            let reasoning_was_streamed = streaming_state
                .lock()
                .map(|s| s.reasoning_was_streamed)
                .unwrap_or(false);

            // Show reasoning/thinking process if present (non-streamed path)
            if !was_streamed {
                if !reasoning_was_streamed {
                    if let Some(ref reasoning) = result.reasoning_content
                        && !reasoning.is_empty()
                    {
                        render::reasoning_content(reasoning);
                    }
                }

                render::response(&result.content);
            }

            // Persist memory to disk after each successful turn
            // to ensure conversation history survives process crashes.
            agent.persist_memory().await;

            // Token usage is now automatically tracked by CostTurnMiddleware.
            // We only need to handle subagent stats for the turn summary.

            // Collect subagent stats and render turn summary
            let subagent_stats = stats_collector
                .lock()
                .map(|s| s.subagents.clone())
                .unwrap_or_default();

            if subagent_stats.is_empty() {
                // No subagents — simple footer
                render::response_footer(result.usage.as_ref(), elapsed);
            } else {
                // Has subagents — render detailed turn summary
                render::turn_summary(
                    result.usage.as_ref(),
                    elapsed,
                    &subagent_stats
                        .iter()
                        .map(|s| render::SubagentUsageSummary {
                            agent_name: s.agent_name.clone(),
                            success: s.success,
                            tool_rounds: s.tool_rounds,
                            usage: s.usage.clone(),
                            elapsed_secs: s.elapsed_ms as f64 / 1000.0,
                        })
                        .collect::<Vec<_>>(),
                );

                // Also add subagent token usage to session cost
                if let Ok(mut c) = cost.lock() {
                    for s in &subagent_stats {
                        c.add_subagent_usage(s.usage.as_ref());
                    }
                }
            }
            println!();
        }
        Err(e) => {
            spinner.finish_and_clear();

            // Clear the subagent event callback
            agent.set_subagent_event_callback(None);

            tracing::error!("Agent error: {}", e);
            render::error(&e);
        }
    }
}

/// Run an interactive REPL loop in Claude Code style.
pub async fn run(agent: &mut dyn AgentMode) -> Result<()> {
    // Use the agent's shared session cost (populated by CostTurnMiddleware)
    // or fall back to a standalone tracker if the agent doesn't provide one.
    let cost: SharedSessionCost = agent
        .session_cost()
        .cloned()
        .unwrap_or_else(|| Arc::new(Mutex::new(SessionCost::new())));

    render::banner(agent);

    // Configure rustyline with tab-completion support
    let config = Config::builder()
        .completion_type(CompletionType::List)
        .edit_mode(EditMode::Emacs)
        .auto_add_history(true)
        .build();

    let helper = SlashCommandHelper::new();
    let mut rl = Editor::with_config(config)?;
    rl.set_helper(Some(helper));

    let prompt = format!("{} ", ">".with(Color::Cyan).attribute(Attribute::Bold));

    loop {
        let readline = rl.readline(&prompt);

        match readline {
            Ok(line) => {
                let input = line.trim();

                if input.is_empty() {
                    continue;
                }

                // ── Handle slash commands ──
                if let Some(cmd) = commands::parse(input) {
                    if handle_command(cmd, agent, &cost)? {
                        break;
                    }
                    continue;
                }

                // ── Handle quit/exit without slash ──
                if input.eq_ignore_ascii_case("quit") || input.eq_ignore_ascii_case("exit") {
                    render::goodbye();
                    break;
                }

                // ── Chat with the agent ──
                handle_chat(input, agent, &cost).await;
            }
            Err(ReadlineError::Interrupted) => {
                // Ctrl-C: just print a new line and continue
                println!();
                continue;
            }
            Err(ReadlineError::Eof) => {
                // Ctrl-D: exit gracefully
                render::goodbye();
                break;
            }
            Err(err) => {
                tracing::error!("Readline error: {}", err);
                render::error(&anyhow::anyhow!("Input error: {}", err));
                break;
            }
        }
    }

    // Persist memory and perform cleanup before exiting
    if let Err(e) = agent.shutdown().await {
        tracing::error!(error = %e, "Failed to shutdown agent cleanly");
    }

    Ok(())
}
