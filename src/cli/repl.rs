use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use crossterm::style::{Attribute, Color, Stylize};
use rustyline::error::ReadlineError;
use rustyline::{CompletionType, Config, EditMode, Editor};

use crate::agent::AgentMode;
use crate::llm::TokenUsage;
use crate::middleware::builtin::confirmation::{
    self, ConfirmationReceiver, ConfirmationRequest, ToolRiskLevel, UserDecision,
};
use crate::middleware::builtin::permission_rules::RuleScope;
use crate::middleware::builtin::permission_rules::PermissionRuleSet;
use crate::middleware::builtin::cost::{SessionCost, SharedSessionCost};
use crate::tools::{ToolEvent, ToolEventCallback};
use super::commands::{self, Command};
use super::completer::SlashCommandHelper;
use super::render;
use super::render::ToolEventFormatter;
use super::render::tool_event::FormattedOutput;

/// Handle a parsed slash command. Returns `true` if the REPL should exit.
async fn handle_command(
    cmd: Command<'_>,
    agent: &mut dyn AgentMode,
    cost: &SharedSessionCost,
    confirm_rx: &Arc<tokio::sync::Mutex<ConfirmationReceiver>>,
) -> Result<bool> {
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
        Command::Compact { instruction, range } => {
            handle_compact(agent, instruction, range).await;
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
        Command::Permissions => handle_permissions(agent),
        Command::Undo => handle_undo().await,
        Command::Image { path } => {
            // Image handling is done in the REPL loop, not here.
            // This branch should not be reached — the REPL loop intercepts it.
            println!(
                "  {} Image queued: {}",
                "📎".with(Color::Cyan),
                path.as_str().with(Color::Grey),
            );
            println!();
        }
        Command::Resume => {
            handle_resume(agent, cost, confirm_rx).await;
        }
        Command::Unknown(raw) => render::unknown_command(raw),    }
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
        let output = {
            let mut fmt = formatter.lock().expect("tool event formatter poisoned");
            // If there's a pending inline progress line and we're about to
            // print something new (not a ToolCallComplete which overwrites it),
            // we need to finish the pending line first.
            if fmt.has_pending_inline {
                if !matches!(event, ToolEvent::ToolCallComplete { .. }) {
                    // Abandon the inline progress — print a newline to finalize it
                    println!();
                    fmt.has_pending_inline = false;
                }
            }
            fmt.format(&event)
        };
        match output {
            FormattedOutput::InlineProgress(line) => {
                // Print without newline — will be overwritten later
                use std::io::Write;
                print!("\r\x1B[2K{}", line);
                let _ = std::io::stdout().flush();
            }
            FormattedOutput::StatusAreaUpdate {
                graduated_lines,
                active_lines,
                prev_area_lines,
            } => {
                // Bazel-style multi-line status area refresh.
                //
                // The status area is a block of N lines at the bottom of
                // the output. On each update we:
                //   1. Move the cursor to the start of the previous status area
                //   2. Clear those lines
                //   3. Print graduated (completed) lines — these are permanent
                //   4. Print new active (in-progress) lines — these will be
                //      erased on the next update
                //
                // The last active line is printed *with* a newline (println!)
                // to keep cursor tracking simple. `rendered_lines` in StatusArea
                // always equals the number of println! calls for active lines.
                use std::io::Write;
                if prev_area_lines > 0 {
                    // Move cursor to the beginning of the previous status area.
                    // Each previous active line was printed with println! (has \n),
                    // so we move up `prev_area_lines` lines.
                    print!("\x1B[{}A", prev_area_lines);
                    for _ in 0..prev_area_lines {
                        print!("\x1B[2K\n");
                    }
                    // Move back up to where we started clearing
                    print!("\x1B[{}A", prev_area_lines);
                }
                // Print graduated lines (permanent — these won't be erased)
                for line in &graduated_lines {
                    println!("{}", line);
                }
                // Print active lines (refreshable status area)
                // All lines use println! so cursor tracking is straightforward.
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
/// Handle the `/permissions` command — display active permission rules.
///
/// Reads the live in-memory rules (including session rules and dynamically-added
/// rules from "Always Allow" prompts), not a fresh load from disk.
fn handle_permissions(agent: &dyn AgentMode) {
    // Use the shared rules engine from the agent. This includes:
    // - Session rules (in-memory only)
    // - Project rules (from .daedalus/permissions.json + YAML config)
    // - Global rules (from ~/.daedalus/permissions.json)
    // Unlike loading fresh from disk, this also shows session-level rules
    // and any rules added via "Always Allow" during this session.
    let mode = agent.permission_mode_name();

    if let Some(rules_arc) = agent.permission_rules() {
        // Block briefly to read the rules — this is a slash command handler,
        // not in the hot path.
        let rt = tokio::runtime::Handle::current();
        let all_rules: Vec<_> = rt.block_on(async {
            let rules = rules_arc.lock().await;
            rules.all_rules()
                .into_iter()
                .map(|(r, s)| (r.clone(), s))
                .collect()
        });
        render::permissions_list(&all_rules, mode);
    } else {
        // Fallback: load from disk (no shared rules available)
        let workspace_root = agent.workspace_root();
        let rule_set = PermissionRuleSet::load(workspace_root.as_deref());
        let all_rules: Vec<_> = rule_set.all_rules()
            .into_iter()
            .map(|(r, s)| (r.clone(), s))
            .collect();
        render::permissions_list(&all_rules, mode);
    }
}

/// Send user input to the agent and render the response.
async fn handle_chat(
    input: &str,
    agent: &mut dyn AgentMode,
    cost: &SharedSessionCost,
    confirm_rx: &Arc<tokio::sync::Mutex<ConfirmationReceiver>>,
) {
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

    // Spawn a background task to handle confirmation requests from the
    // confirmation middleware. This runs concurrently with agent.chat().
    let confirm_rx_clone = Arc::clone(confirm_rx);
    let confirm_spinner = Arc::clone(&spinner);
    let confirm_handle = tokio::spawn(async move {
        let mut rx = confirm_rx_clone.lock().await;
        while let Some(request) = rx.recv().await {
            // Pause the spinner while showing the confirmation prompt
            confirm_spinner.finish_and_clear();
            // Use spawn_blocking for stdin I/O to avoid blocking the tokio runtime.
            // The request (including response_tx) is moved into the blocking task
            // so the oneshot response is sent from within the blocking context.
            tokio::task::spawn_blocking(move || {
                let decision = prompt_user_confirmation(&request);
                let _ = request.response_tx.send(decision);
            }).await.ok();
            // Restart spinner
            confirm_spinner.reset_elapsed();
            confirm_spinner.set_message("Thinking\u{2026}");
            confirm_spinner.enable_steady_tick(std::time::Duration::from_millis(80));
        }
    });

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

            // ── Trigger Stop lifecycle hooks ──
            if let Some(hooks_config) = agent.hooks_config() {
                let session_id = agent.session_id().to_string();
                crate::hooks::run_stop_hooks(hooks_config, &session_id).await;
            }

            // Token usage is now automatically tracked by CostTurnMiddleware.
            // We only need to handle subagent stats for the turn summary.

            // Collect subagent stats and render turn summary
            let subagent_stats = stats_collector
                .lock()
                .map(|s| s.subagents.clone())
                .unwrap_or_default();

            if subagent_stats.is_empty() {
                // No subagents — simple footer
                render::response_footer(result.usage.as_ref(), elapsed, agent.context_window());
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
                    agent.context_window(),
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

    // Stop the confirmation handler task (no more requests expected this turn)
    confirm_handle.abort();
}

/// Prompt the user for confirmation of a tool call.
///
/// Displays the tool call details and risk level, then reads a single
/// character from stdin to determine the user's decision.
fn prompt_user_confirmation(request: &ConfirmationRequest) -> UserDecision {
    use std::io::Write;

    let risk_label = match request.risk_level {
        ToolRiskLevel::Sensitive => "Sensitive".with(Color::Yellow),
        ToolRiskLevel::Dangerous => "Dangerous".with(Color::Red).attribute(Attribute::Bold),
        ToolRiskLevel::ReadOnly => "ReadOnly".with(Color::Green), // shouldn't happen
    };

    let risk_reason = match request.risk_level {
        ToolRiskLevel::Sensitive => "modifies files",
        ToolRiskLevel::Dangerous => "arbitrary execution",
        ToolRiskLevel::ReadOnly => "read-only",
    };

    // Print the confirmation prompt
    println!();
    println!(
        "  {} {} wants to execute:",
        "⚠️".with(Color::Yellow),
        request.tool_name.clone().with(Color::Cyan).attribute(Attribute::Bold),
    );
    println!(
        "  {}  {}",
        "┃".with(Color::DarkGrey),
        request.description.clone().with(Color::White),
    );
    println!(
        "  {}",
        "┃".with(Color::DarkGrey),
    );
    println!(
        "  {}  Risk: {} ({})",
        "┃".with(Color::DarkGrey),
        risk_label,
        risk_reason,
    );
    println!(
        "  {}",
        "┃".with(Color::DarkGrey),
    );

    // Build the options line based on whether we have a suggested pattern
    let pattern_hint = if let Some(ref pattern) = request.suggested_pattern {
        format!(
            "  [{}] Always allow \"{}({})\"  ",
            "a".with(Color::Magenta).attribute(Attribute::Bold),
            request.tool_name,
            pattern,
        )
    } else {
        format!(
            "  [{}] Always allow {}  ",
            "a".with(Color::Magenta).attribute(Attribute::Bold),
            request.tool_name,
        )
    };

    print!(
        "  {}  [{}] Allow once  [{}] Allow for session {}[{}] Deny  > ",
        "┗━".with(Color::DarkGrey),
        "y".with(Color::Green).attribute(Attribute::Bold),
        "s".with(Color::Blue).attribute(Attribute::Bold),
        pattern_hint,
        "n".with(Color::Red).attribute(Attribute::Bold),
    );
    let _ = std::io::stdout().flush();

    // Read user input (single line)
    let decision = read_confirmation_input(request.suggested_pattern.clone());
    println!();
    decision
}

/// Read a single character of confirmation input from the terminal.
///
/// Falls back to "deny" if input cannot be read.
fn read_confirmation_input(suggested_pattern: Option<String>) -> UserDecision {
    let mut input = String::new();
    match std::io::stdin().read_line(&mut input) {
        Ok(_) => {
            let trimmed = input.trim().to_lowercase();
            match trimmed.as_str() {
                "y" | "yes" | "" => UserDecision::AllowOnce,
                "s" | "session" => UserDecision::AllowSession,
                "a" | "always" => UserDecision::AlwaysAllow {
                    scope: RuleScope::Project,
                    pattern: suggested_pattern,
                },
                "ag" | "always-global" | "global" => UserDecision::AlwaysAllow {
                    scope: RuleScope::Global,
                    pattern: suggested_pattern,
                },
                "n" | "no" | "d" | "deny" => UserDecision::Deny,
                _ => {
                    // Unknown input — treat as deny for safety
                    println!(
                        "  {} Unknown input '{}', denying.",
                        "⚠".with(Color::Yellow),
                        trimmed,
                    );
                    UserDecision::Deny
                }
            }
        }
        Err(_) => {
            // Can't read input — deny for safety
            UserDecision::Deny
        }
    }
}

/// Handle the `/compact` command — compress conversation history.
async fn handle_compact(agent: &mut dyn AgentMode, instruction: Option<&str>, range: Option<(usize, usize)>) {
    let spinner = Arc::new(render::spinner());
    let msg = if range.is_some() {
        "Compressing partial context\u{2026}"
    } else {
        "Compressing context\u{2026}"
    };
    spinner.set_message(msg);

    let result = if let Some(r) = range {
        agent.compact_range(instruction, r).await
    } else {
        agent.compact(instruction).await
    };

    match result {
        Ok(message) => {
            spinner.finish_and_clear();
            println!(
                "  {} {}",
                "✓".with(Color::Green).attribute(Attribute::Bold),
                message.with(Color::Grey),
            );
            println!();
        }
        Err(e) => {
            spinner.finish_and_clear();
            println!(
                "  {} Compact failed: {}",
                "✗".with(Color::Red).attribute(Attribute::Bold),
                e,
            );
            println!();
        }
    }
}

/// Handle the `/undo` command — restore the last file modification.
async fn handle_undo() {
    use crate::tools::checkpoint;

    match checkpoint::undo().await {
        Ok(message) => {
            println!(
                "  {} {}",
                "↩".with(Color::Green).attribute(Attribute::Bold),
                message.with(Color::Grey),
            );
            println!();
        }
        Err(e) => {
            println!(
                "  {} {}",
                "✗".with(Color::Red).attribute(Attribute::Bold),
                e.to_string().with(Color::Grey),
            );
            println!();
        }
    }
}

/// Handle the /resume command — check for a saved checkpoint and resume execution.
async fn handle_resume(
    agent: &mut dyn AgentMode,
    cost: &SharedSessionCost,
    confirm_rx: &Arc<tokio::sync::Mutex<ConfirmationReceiver>>,
) {
    use crate::agent::tool_loop::checkpoint::ToolLoopCheckpoint;

    // Try to find a checkpoint in the workspace
    let checkpoint_path = agent.checkpoint_path();
    let checkpoint_path = match checkpoint_path {
        Some(p) => p,
        None => {
            println!(
                "  {} No workspace configured — cannot resume.",
                "✗".with(Color::Red).attribute(Attribute::Bold),
            );
            println!();
            return;
        }
    };

    match ToolLoopCheckpoint::load(&checkpoint_path) {
        Ok(Some(cp)) => {
            println!(
                "  {} Found checkpoint: {}",
                "▶".with(Color::Green).attribute(Attribute::Bold),
                cp.summary().with(Color::Grey),
            );
            println!(
                "  {} Resuming with the original task...",
                "↻".with(Color::Cyan).attribute(Attribute::Bold),
            );
            println!();

            // Build a resume prompt that gives the LLM context about what
            // was accomplished before the crash. This is more reliable than
            // injecting raw tool_history (which may contain stale file contents).
            let tool_summary: Vec<String> = cp.restore_tool_history().iter().enumerate().map(|(i, round)| {
                let calls: Vec<String> = round.calls.iter().map(|c| {
                    format!("{}({})", c.function_name, crate::tools::truncate_chars(&c.arguments.to_string(), 100))
                }).collect();
                format!("  Round {}: {}", i + 1, calls.join(", "))
            }).collect();

            let files_read = cp.restore_files_read();
            let files_summary = if files_read.is_empty() {
                String::new()
            } else {
                let mut sorted: Vec<&String> = files_read.iter().collect();
                sorted.sort();
                format!("\nFiles read: {}", sorted.iter().map(|p| p.rsplit('/').next().unwrap_or(p)).collect::<Vec<_>>().join(", "))
            };

            let resume_prompt = format!(
                "[RESUMING INTERRUPTED TASK]\n\
                 Original task: {}\n\
                 Progress: completed {} rounds with {} tool calls.{}\n\
                 Tool call history:\n{}\n\n\
                 Continue from where you left off. Do NOT repeat work that was already done. \
                 Review the files that were already read and pick up the task from the point of interruption.",
                cp.user_input,
                cp.last_round,
                cp.total_tool_calls,
                files_summary,
                tool_summary.join("\n"),
            );

            // Clear the checkpoint now that we're resuming
            ToolLoopCheckpoint::clear(&checkpoint_path);

            handle_chat(&resume_prompt, agent, cost, confirm_rx).await;
        }
        Ok(None) => {
            println!(
                "  {} No checkpoint found — nothing to resume.",
                "ℹ".with(Color::Blue).attribute(Attribute::Bold),
            );
            println!();
        }
        Err(e) => {
            println!(
                "  {} Failed to load checkpoint: {}",
                "✗".with(Color::Red).attribute(Attribute::Bold),
                e.to_string().with(Color::Grey),
            );
            println!();
        }
    }
}

/// Load an image file and return its base64-encoded content with MIME type.
fn load_image_as_base64(path: &str) -> Result<(String, String)> {
    use std::path::Path;
    use anyhow::Context as _;

    let path = Path::new(path);
    if !path.exists() {
        anyhow::bail!("File not found: {}", path.display());
    }

    let extension = path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let media_type = match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        _ => anyhow::bail!("Unsupported image format: .{}. Supported: png, jpg, gif, webp, svg", extension),
    };

    let data = std::fs::read(path)
        .with_context(|| format!("Failed to read image file: {}", path.display()))?;

    // Check file size (max 20MB for most APIs)
    if data.len() > 20 * 1024 * 1024 {
        anyhow::bail!("Image file too large ({} bytes). Maximum: 20MB", data.len());
    }

    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&data);

    Ok((media_type.to_string(), encoded))
}

/// Handle a chat with a pre-built multimodal message.
async fn handle_chat_with_message(
    message: crate::llm::ChatMessage,
    agent: &mut dyn AgentMode,
    _cost: &SharedSessionCost,
    confirm_rx: &Arc<tokio::sync::Mutex<ConfirmationReceiver>>,
) {
    let text_preview = if message.content.len() > 50 {
        format!("{}...", &message.content[..50])
    } else {
        message.content.clone()
    };
    tracing::debug!("User input (multimodal): {}", text_preview);

    // Show spinner while waiting for LLM response
    let spinner = Arc::new(render::spinner());
    let start = Instant::now();

    let stats_collector = Arc::new(Mutex::new(TurnStatsCollector::default()));
    let streaming_state = Arc::new(Mutex::new(StreamingState::default()));
    let tool_callback = build_tool_event_callback(&spinner, &stats_collector, &streaming_state);

    agent.set_subagent_event_callback(Some(Arc::clone(&tool_callback)));

    let confirm_rx_clone = Arc::clone(confirm_rx);
    let confirm_spinner = Arc::clone(&spinner);
    let confirm_handle = tokio::spawn(async move {
        let mut rx = confirm_rx_clone.lock().await;
        while let Some(request) = rx.recv().await {
            confirm_spinner.finish_and_clear();
            tokio::task::spawn_blocking(move || {
                let decision = prompt_user_confirmation(&request);
                let _ = request.response_tx.send(decision);
            }).await.ok();
            confirm_spinner.reset_elapsed();
            confirm_spinner.set_message("Thinking\u{2026}");
            confirm_spinner.enable_steady_tick(std::time::Duration::from_millis(80));
        }
    });

    match agent.chat_with_message(message, Some(&tool_callback)).await {
        Ok(result) => {
            let elapsed = start.elapsed().as_secs_f64();
            spinner.finish_and_clear();
            confirm_handle.abort();

            let was_streamed = streaming_state.lock()
                .map(|s| s.content_was_streamed)
                .unwrap_or(false);

            if !was_streamed {
                render::response(&result.content);
            }

            // Persist memory
            agent.persist_memory().await;

            // ── Trigger Stop lifecycle hooks ──
            if let Some(hooks_config) = agent.hooks_config() {
                let session_id = agent.session_id().to_string();
                crate::hooks::run_stop_hooks(hooks_config, &session_id).await;
            }

            // Render footer
            render::response_footer(result.usage.as_ref(), elapsed, agent.context_window());
            println!();
        }
        Err(e) => {
            spinner.finish_and_clear();
            confirm_handle.abort();
            render::error(&e);
        }
    }

    agent.set_subagent_event_callback(None);
}

/// Run an interactive REPL loop in Claude Code style.
pub async fn run(agent: &mut dyn AgentMode) -> Result<()> {
    // Use the agent's shared session cost (populated by CostTurnMiddleware)
    // or fall back to a standalone tracker if the agent doesn't provide one.
    let cost: SharedSessionCost = agent
        .session_cost()
        .cloned()
        .unwrap_or_else(|| Arc::new(Mutex::new(SessionCost::new())));

    // Set up the confirmation channel for interactive tool approval.
    // The sender goes to the agent (passed through to the confirmation middleware),
    // the receiver stays here to handle confirmation prompts in the terminal.
    let (confirm_tx, confirm_rx) = confirmation::confirmation_channel();
    agent.set_confirmation_sender(confirm_tx);
    let confirm_rx = Arc::new(tokio::sync::Mutex::new(confirm_rx));

    render::banner(agent);

    // ── Trigger SessionStart lifecycle hooks ──
    if let Some(hooks_config) = agent.hooks_config() {
        let session_id = agent.session_id().to_string();
        crate::hooks::run_session_start_hooks(hooks_config, &session_id).await;
    }

    // Configure rustyline with tab-completion support
    let config = Config::builder()
        .completion_type(CompletionType::List)
        .edit_mode(EditMode::Emacs)
        .auto_add_history(true)
        .build();

    let helper = SlashCommandHelper::new();
    let mut rl = Editor::with_config(config)?;
    rl.set_helper(Some(helper));

    let prompt = format!("{} ", ">".with(Color::Blue).attribute(Attribute::Bold));

    // Pending image attachment for the next message
    let mut pending_image: Option<String> = None;

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
                    // Special handling for /image — store the path for the next message
                    if let commands::Command::Image { ref path } = cmd {
                        match load_image_as_base64(path) {
                            Ok((media_type, _data)) => {
                                pending_image = Some(path.clone());
                                println!(
                                    "  {} Image attached: {} ({}). Type your message to send it.",
                                    "📎".with(Color::Cyan).attribute(Attribute::Bold),
                                    path.as_str().with(Color::Grey),
                                    media_type.as_str().with(Color::DarkGrey),
                                );
                                println!();
                            }
                            Err(e) => {
                                println!(
                                    "  {} Failed to load image: {}",
                                    "✗".with(Color::Red).attribute(Attribute::Bold),
                                    e.to_string().with(Color::Grey),
                                );
                                println!();
                            }
                        }
                        continue;
                    }

                    if handle_command(cmd, agent, &cost, &confirm_rx).await? {
                        break;
                    }
                    continue;
                }

                // ── Handle quit/exit without slash ──
                if input.eq_ignore_ascii_case("quit") || input.eq_ignore_ascii_case("exit") {
                    render::goodbye();
                    break;
                }

                // ── Chat with the agent (with optional image attachment) ──
                if let Some(ref image_path) = pending_image.take() {
                    match load_image_as_base64(image_path) {
                        Ok((media_type, data)) => {
                            let image_source = crate::llm::ImageSource::Base64 {
                                media_type,
                                data,
                            };
                            // Create a multimodal message and pass it to the agent
                            let msg = crate::llm::ChatMessage::user_with_image(input, image_source);
                            handle_chat_with_message(msg, agent, &cost, &confirm_rx).await;
                        }
                        Err(e) => {
                            println!(
                                "  {} Failed to load image: {}. Sending text only.",
                                "⚠".with(Color::Yellow),
                                e.to_string().with(Color::Grey),
                            );
                            handle_chat(input, agent, &cost, &confirm_rx).await;
                        }
                    }
                } else {
                    handle_chat(input, agent, &cost, &confirm_rx).await;
                }
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
