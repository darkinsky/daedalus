//! Non-interactive (print) mode runner.
//!
//! Executes a single prompt against the agent and outputs the result
//! in the requested format (text, json, or stream-json), then exits.
//!
//! This module is the core of the `--print` / `-p` CLI flag.

use std::io::Read;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use crossterm::style::{Color, Stylize};

use crate::agent::AgentMode;
use crate::tools::{ToolEvent, ToolEventCallback};

use super::cli_args::OutputFormat;
use super::output_format::{
    StreamEvent, ResultPayload, UsageSummary,
    emit_stream_event, emit_json_result,
};

/// Read the prompt from stdin (used when `-p -` is passed).
pub fn read_stdin_prompt() -> Result<String> {
    let mut buffer = String::new();
    std::io::stdin().read_to_string(&mut buffer)?;
    let trimmed = buffer.trim().to_string();
    if trimmed.is_empty() {
        anyhow::bail!("Empty prompt received from stdin");
    }
    Ok(trimmed)
}

/// Run a single prompt in non-interactive (print) mode.
///
/// This is the main entry point for `--print` / `-p`. It:
/// 1. Reads the prompt (from CLI arg or stdin).
/// 2. Sends it to the agent with an appropriate tool event callback.
/// 3. Outputs the result in the requested format.
/// 4. Returns an exit code (0 = success, 1 = error).
pub async fn run(
    agent: &mut dyn AgentMode,
    prompt: &str,
    format: &OutputFormat,
) -> Result<ExitCode> {
    let start = Instant::now();

    // Build the tool event callback based on output format
    let tool_callback: Option<ToolEventCallback> = match format {
        OutputFormat::StreamJson => Some(build_stream_json_callback()),
        OutputFormat::Text => Some(build_text_stderr_callback()),
        OutputFormat::Json => None, // Silent until final result
    };

    // Set the subagent event callback so subagent tool events are also captured
    if let Some(ref cb) = tool_callback {
        agent.set_subagent_event_callback(Some(Arc::clone(cb)));
    }

    // Emit initial system event for stream-json mode
    if matches!(format, OutputFormat::StreamJson) {
        emit_stream_event(&StreamEvent::System {
            message: format!("Daedalus v{}", env!("CARGO_PKG_VERSION")),
            session_id: agent.session().id.clone(),
            model: agent.model_name().to_string(),
            provider: agent.provider_name().to_string(),
        });
    }

    // Execute the prompt
    let result = agent.chat(prompt, tool_callback.as_ref()).await;
    let elapsed = start.elapsed();
    let duration_ms = elapsed.as_millis() as u64;

    // Clear the subagent event callback
    agent.set_subagent_event_callback(None);

    // Output the result in the requested format
    match result {
        Ok(response) => {
            emit_success(agent, &response, format, elapsed, duration_ms);
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            emit_error(agent, &e, format, duration_ms);
            Ok(ExitCode::FAILURE)
        }
    }
}

/// Emit a successful response in the requested output format.
fn emit_success(
    agent: &dyn AgentMode,
    response: &crate::llm::ChatResponse,
    format: &OutputFormat,
    elapsed: std::time::Duration,
    duration_ms: u64,
) {
    let usage_summary = response.usage.as_ref().map(|u| UsageSummary {
        prompt_tokens: u.prompt_tokens,
        completion_tokens: u.completion_tokens,
        total_tokens: u.total_tokens,
    });

    match format {
        OutputFormat::Text => {
            // Show reasoning if present
            if let Some(ref reasoning) = response.reasoning_content {
                if !reasoning.is_empty() {
                    eprintln!(
                        "{}",
                        format!("💭 Reasoning:\n{}", reasoning)
                            .with(Color::DarkGrey)
                    );
                    eprintln!();
                }
            }
            // Main response to stdout (for piping)
            print!("{}", response.content);
            // Token usage to stderr
            if let Some(ref usage) = response.usage {
                let parts: Vec<String> = [
                    usage.prompt_tokens.map(|t| format!("{}↑", t)),
                    usage.completion_tokens.map(|t| format!("{}↓", t)),
                    Some(format!("{:.1}s", elapsed.as_secs_f64())),
                ]
                .into_iter()
                .flatten()
                .collect();
                eprintln!();
                eprintln!(
                    "{}",
                    parts.join(" · ").with(Color::DarkGrey)
                );
            }
        }
        OutputFormat::Json => {
            let payload = ResultPayload {
                result: response.content.clone(),
                session_id: agent.session().id.clone(),
                is_error: false,
                usage: usage_summary,
                duration_ms,
                tool_rounds: agent.session().request_count.saturating_sub(1),
            };
            emit_json_result(&payload);
        }
        OutputFormat::StreamJson => {
            emit_stream_event(&StreamEvent::Assistant {
                content: response.content.clone(),
                reasoning: response.reasoning_content.clone(),
            });
            emit_stream_event(&StreamEvent::Result(ResultPayload {
                result: response.content.clone(),
                session_id: agent.session().id.clone(),
                is_error: false,
                usage: usage_summary,
                duration_ms,
                tool_rounds: agent.session().request_count.saturating_sub(1),
            }));
        }
    }
}

/// Emit an error response in the requested output format.
fn emit_error(
    agent: &dyn AgentMode,
    error: &anyhow::Error,
    format: &OutputFormat,
    duration_ms: u64,
) {
    let error_msg = format!("{:#}", error);

    match format {
        OutputFormat::Text => {
            eprintln!(
                "{} {}",
                "✗".with(Color::Red),
                format!("Error: {}", error_msg).with(Color::Red)
            );
        }
        OutputFormat::Json => {
            let payload = ResultPayload {
                result: error_msg,
                session_id: agent.session().id.clone(),
                is_error: true,
                usage: None,
                duration_ms,
                tool_rounds: 0,
            };
            emit_json_result(&payload);
        }
        OutputFormat::StreamJson => {
            emit_stream_event(&StreamEvent::Result(ResultPayload {
                result: error_msg,
                session_id: agent.session().id.clone(),
                is_error: true,
                usage: None,
                duration_ms,
                tool_rounds: 0,
            }));
        }
    }
}

/// Build a tool event callback that emits NDJSON events to stdout.
///
/// Used in `stream-json` output mode for real-time tool progress.
fn build_stream_json_callback() -> ToolEventCallback {
    Arc::new(move |event: ToolEvent| {
        let stream_event = match event {
            ToolEvent::RoundStart { round } => {
                StreamEvent::ToolRoundStart { round }
            }
            ToolEvent::ToolCallStart { tool_name, source, arguments } => {
                StreamEvent::ToolUse {
                    tool: tool_name,
                    source,
                    input: Some(arguments),
                }
            }
            ToolEvent::ToolCallComplete { tool_name, success, result_content } => {
                StreamEvent::ToolResult {
                    tool: tool_name,
                    content: result_content,
                    success,
                }
            }
            ToolEvent::RoundComplete { tool_count } => {
                StreamEvent::ToolRoundComplete { tool_count }
            }
            ToolEvent::SubagentStart { agent_name, task_preview } => {
                StreamEvent::SubagentStart { agent_name, task_preview }
            }
            ToolEvent::SubagentComplete { agent_name, success, tool_rounds, result_preview } => {
                StreamEvent::SubagentComplete {
                    agent_name,
                    success,
                    tool_rounds,
                    result_preview,
                }
            }
        };
        emit_stream_event(&stream_event);
    })
}

/// Build a tool event callback that prints progress to stderr.
///
/// Used in `text` output mode so tool progress doesn't pollute stdout
/// (which is reserved for the final response, suitable for piping).
///
/// Holds a long-lived [`ToolEventFormatter`] so each tool call gets a
/// `[round.index]` tag that is repeated on its completion line, making
/// it easy to pair concurrent starts and completes visually.
fn build_text_stderr_callback() -> ToolEventCallback {
    use std::sync::Mutex;
    use super::render::ToolEventFormatter;

    let formatter = Arc::new(Mutex::new(ToolEventFormatter::new()));
    Arc::new(move |event: ToolEvent| {
        let rendered = {
            let mut fmt = formatter.lock().expect("tool event formatter poisoned");
            fmt.format(&event)
        };
        for line in rendered {
            eprintln!("{}", line);
        }
    })
}
