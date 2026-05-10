//! Shared tool-calling loop, used by both `ChatAgent` and `SubagentRunner`.
//!
//! The tool-calling loop is the core algorithm of an agent: repeatedly ask
//! the LLM for a response, execute any tool calls it produces, feed the
//! results back, and stop when either a plain text answer arrives, a
//! duplicate-call streak breaches the hard-stop threshold, or the round
//! budget is exhausted.
//!
//! ## Module structure
//!
//! - `truncation`       — Budget-based dynamic truncation of tool history
//! - `context_pressure` — Context window usage monitoring and forced responses
//! - `mod.rs` (this)    — The main loop, executor trait, and round execution

pub(crate) mod truncation;
pub(crate) mod context_pressure;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use crate::llm::{
    ChatMessage, ChatResponse, LlmApi, StreamAccumulator, StreamChunk,
    TokenUsage, ToolCall, ToolResponse, ToolRound,
};
use crate::middleware::{Extensions, ToolNext, ToolRequest};
use crate::middleware::pipeline::ToolPipeline;
use crate::tools::{ToolEvent, ToolEventCallback};
use crate::agent_tracing::{TracingHook, TraceContext};

use super::duplicate_detector::{annotate_responses, DuplicateAction, DuplicateDetector};

// Re-export public types from submodules.
pub use truncation::TruncationConfig;
pub(crate) use truncation::{truncate_tool_history, estimate_history_chars, CHARS_PER_TOKEN};
pub(crate) use context_pressure::{
    estimate_context_usage_pct, context_budget_hint, force_final_response, emit,
};

// ── Injected abstractions ──

/// Executes a single tool call and identifies its source for observability.
///
/// Every concrete agent brings its own executor. `ChatAgent` routes to
/// `ToolRouter` (built-in + MCP); `SubagentRunner` routes to a filtered
/// `BuiltinToolRegistry`. The loop stays oblivious to the routing rules.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Execute one tool call and return its response.
    async fn execute(&self, call: &ToolCall) -> ToolResponse;

    /// Return the string that appears in `ToolEvent::ToolCallStart.source`.
    fn source_of(&self, tool_name: &str) -> String;
}

/// Non-behavioural loop parameters (no side effects, just knobs).
#[derive(Debug, Clone)]
pub struct LoopConfig {
    /// Hard cap on rounds — the loop exits with `MaxRoundsExceeded` when hit.
    pub max_tool_rounds: usize,
    /// Human-readable label used in log messages.
    pub agent_label: String,
    /// If `true`, the last non-empty `reasoning_content` seen across rounds
    /// is forwarded into the final `LoopOutcome::Final`.
    pub track_reasoning: bool,
    /// Controls how aggressively older tool-round results are truncated.
    pub truncation: Option<TruncationConfig>,

    // ── Context pressure awareness ──

    /// Context window size (in tokens) for budget-aware behavior.
    pub context_window_tokens: Option<usize>,
    /// Ratio of context window usage at which to start injecting "wrap up" hints.
    pub context_soft_limit_ratio: f64,
    /// Ratio of context window usage at which to force-stop the loop.
    pub context_hard_limit_ratio: f64,
}

// ── Outputs ──

/// Terminal condition of the loop.
pub enum LoopOutcome {
    /// LLM produced a tool-call-free message — the normal happy path.
    Final {
        content: String,
        reasoning: Option<String>,
    },
    /// Duplicate-call streak hit the hard-stop threshold.
    DuplicateStop { message: String },
    /// Round budget exhausted without reaching a final response.
    MaxRoundsExceeded,
    /// Context budget exceeded — the loop force-stopped.
    ContextBudgetExceeded {
        content: String,
        reasoning: Option<String>,
    },
}

/// Full loop output: outcome + accumulated bookkeeping.
pub struct LoopResult {
    pub outcome: LoopOutcome,
    pub usage: TokenUsage,
    pub tool_history: Vec<ToolRound>,
}

// ── Loop context ──

use crate::agent_tracing::ToolDetail;

/// Runtime context for a single tool-calling loop invocation.
///
/// Groups the optional callbacks, hooks, and pipeline that were previously
/// passed as individual parameters to `run_tool_loop`.
pub struct LoopContext<'a> {
    /// The tool executor that handles individual tool calls.
    pub executor: &'a dyn ToolExecutor,
    /// Pre-built messages from memory (system + history + user).
    pub messages: &'a [ChatMessage],
    /// Tool definitions in OpenAI function-calling JSON format.
    pub tools: &'a [Value],
    /// Optional callback for CLI event rendering.
    pub on_tool_event: Option<&'a ToolEventCallback>,
    /// Optional callback invoked after each LLM response.
    pub on_llm_response: Option<&'a (dyn Fn(&ChatResponse) + Send + Sync)>,
    /// Optional tracing hook for LLM call spans.
    pub tracing_hook: Option<&'a TracingHook>,
    /// Optional tool middleware pipeline.
    pub tool_pipeline: Option<&'a ToolPipeline>,
    /// Optional shared notes from `take_note` tool.
    pub shared_notes: Option<&'a crate::tools::take_note::SharedNotes>,
}

// ── The loop itself ──

/// Run the tool-calling loop against an LLM and a tool executor.
pub async fn run_tool_loop(
    llm: &dyn LlmApi,
    cfg: &LoopConfig,
    ctx: &LoopContext<'_>,
) -> Result<LoopResult> {
    let mut tool_history: Vec<ToolRound> = Vec::new();
    let mut total_usage = TokenUsage::default();
    let mut last_reasoning: Option<String> = None;
    let mut duplicate_detector = DuplicateDetector::new();

    // ── Session-level tracking (never truncated) ──
    let mut files_read: HashSet<String> = HashSet::new();
    let mut total_tool_calls: usize = 0;

    for round_idx in 0..cfg.max_tool_rounds {
        let round_number = round_idx + 1;

        // ── Context pressure check ──
        let context_usage_pct = estimate_context_usage_pct(
            ctx.messages,
            &tool_history,
            cfg.context_window_tokens,
        );

        if let Some(cw) = cfg.context_window_tokens {
            if context_usage_pct >= (cfg.context_hard_limit_ratio * 100.0) as u8 {
                tracing::warn!(
                    agent = %cfg.agent_label,
                    context_usage_pct,
                    hard_limit = %(cfg.context_hard_limit_ratio * 100.0),
                    round = round_number,
                    "Context budget exceeded — forcing final response"
                );

                emit(ctx.on_tool_event, ToolEvent::ContextBudgetExceeded {
                    usage_pct: context_usage_pct,
                });

                let final_result = force_final_response(
                    llm, cfg, ctx, &tool_history, &total_usage,
                ).await?;
                return Ok(final_result);
            }

            if context_usage_pct >= 60 {
                tracing::info!(
                    agent = %cfg.agent_label,
                    context_usage_pct,
                    context_window = cw,
                    round = round_number,
                    "Context pressure elevated"
                );
            }
        }

        let llm_start = Instant::now();

        // Start LLM call tracing span
        let mut llm_span = if let Some(hook) = ctx.tracing_hook {
            let tool_details: Vec<ToolDetail> = ctx.tools.iter()
                .filter_map(|t| {
                    let function = t.get("function")?;
                    let name = function.get("name")?.as_str()?;
                    let description = function.get("description")?.as_str()?;
                    let parameters = function.get("parameters")?;
                    Some(ToolDetail {
                        name: name.to_string(),
                        description: description.to_string(),
                        parameters_schema: parameters.clone(),
                    })
                })
                .collect();
            hook.on_llm_call_start(
                llm.model_name(),
                llm.provider_name(),
                ctx.messages,
                &tool_details,
            ).await
        } else {
            None
        };

        let use_streaming = ctx.on_tool_event.is_some() && ctx.tool_pipeline.is_some();

        // Build truncated tool history with context-pressure-aware tightening
        let trunc_cfg = cfg.truncation.as_ref().cloned().unwrap_or_default();
        let effective_trunc_cfg = trunc_cfg.tighten_for_pressure(context_usage_pct);
        let mut truncated_history = truncate_tool_history(&tool_history, &effective_trunc_cfg);

        // ── Inject session metadata into tool history ──
        inject_session_metadata(
            &mut truncated_history,
            round_number,
            cfg,
            &files_read,
            total_tool_calls,
            context_usage_pct,
            ctx.shared_notes,
        );

        let response = if use_streaming {
            stream_llm_response(llm, ctx, &truncated_history).await?
        } else {
            llm.chat_with_tools(ctx.messages, ctx.tools, &truncated_history, None)
                .await?
        };

        let llm_elapsed_ms = llm_start.elapsed().as_millis() as u64;

        // Finish LLM call tracing span
        if let Some(ref mut span) = llm_span {
            span.set_llm_response(&response);
        }
        if let Some(span) = llm_span {
            span.finish_ok().await;
        }

        if let Some(hook) = ctx.on_llm_response {
            hook(&response);
        }

        if let Some(ref usage) = response.usage {
            total_usage.accumulate(usage);
        }

        // Happy path: no more tool calls, LLM produced a final answer.
        if response.tool_calls.is_empty() {
            let reasoning = if cfg.track_reasoning {
                response.reasoning_content.or(last_reasoning)
            } else {
                None
            };
            return Ok(LoopResult {
                outcome: LoopOutcome::Final {
                    content: response.content,
                    reasoning,
                },
                usage: total_usage,
                tool_history,
            });
        }

        // Emit intermediate LLM response
        emit(ctx.on_tool_event, ToolEvent::LlmResponse {
            round: round_number,
            reasoning: if use_streaming { None } else { response.reasoning_content.clone() },
            content: if use_streaming { String::new() } else { response.content.clone() },
            usage: response.usage.clone(),
            elapsed_ms: llm_elapsed_ms,
        });

        let round_reasoning = response.reasoning_content.clone();

        if cfg.track_reasoning && response.reasoning_content.is_some() {
            last_reasoning = response.reasoning_content;
        }

        emit(ctx.on_tool_event, ToolEvent::RoundStart { round: round_number });

        let tool_calls = response.tool_calls;
        let tool_start = Instant::now();

        // Execute all tool calls
        let mut responses = if let Some(pipeline) = ctx.tool_pipeline {
            let trace_ctx = ctx.tracing_hook.map(|h| h.context_arc());
            execute_round_via_pipeline(
                ctx.executor, &tool_calls, pipeline, trace_ctx, round_number,
            ).await
        } else {
            execute_round_direct(ctx.executor, &tool_calls, ctx.on_tool_event, ctx.tracing_hook).await
        };

        // Check for runaway duplicate calls
        match duplicate_detector.record_round(&tool_calls) {
            DuplicateAction::Warn(warnings) => {
                for w in &warnings {
                    tracing::warn!(
                        agent = %cfg.agent_label,
                        tool = %w.tool_name,
                        streak = w.count,
                        round = round_number,
                        "Agent repeated identical tool call"
                    );
                }
                annotate_responses(&tool_calls, &mut responses, &warnings);
            }
            DuplicateAction::Stop(w) => {
                tracing::error!(
                    agent = %cfg.agent_label,
                    tool = %w.tool_name,
                    streak = w.count,
                    round = round_number,
                    "Agent force-stopped due to duplicate tool calls"
                );
                let stop_message = w.stop_message();
                tool_history.push(ToolRound {
                    calls: tool_calls,
                    responses,
                    reasoning_content: round_reasoning.clone(),
                });
                return Ok(LoopResult {
                    outcome: LoopOutcome::DuplicateStop { message: stop_message },
                    usage: total_usage,
                    tool_history,
                });
            }
            DuplicateAction::Ok => {}
        }

        // ── Track file reads ──
        for tc in &tool_calls {
            total_tool_calls += 1;
            if let Some(path) = tc.arguments.get("path").and_then(|v| v.as_str()) {
                if tc.function_name == "read_file"
                    || tc.function_name == "grep_search"
                    || tc.function_name == "search_files"
                    || tc.function_name == "list_directory"
                {
                    files_read.insert(path.to_string());
                }
            }
        }

        // ── Diagnostic logging ──
        let full_chars = estimate_history_chars(&tool_history);
        tracing::debug!(
            agent = %cfg.agent_label,
            round = round_number,
            max_rounds = cfg.max_tool_rounds,
            tool_calls_this_round = tool_calls.len(),
            total_tool_calls,
            unique_files = files_read.len(),
            history_chars = full_chars,
            estimated_history_tokens = full_chars / CHARS_PER_TOKEN,
            "Tool loop round stats"
        );

        tool_history.push(ToolRound {
            calls: tool_calls,
            responses,
            reasoning_content: round_reasoning,
        });

        emit(ctx.on_tool_event, ToolEvent::RoundComplete {
            tool_count: tool_history.last().map(|r| r.calls.len()).unwrap_or(0),
            elapsed_ms: tool_start.elapsed().as_millis() as u64,
        });
    }

    tracing::warn!(
        agent = %cfg.agent_label,
        max_tool_rounds = cfg.max_tool_rounds,
        "Agent exceeded maximum tool-calling rounds"
    );
    Ok(LoopResult {
        outcome: LoopOutcome::MaxRoundsExceeded,
        usage: total_usage,
        tool_history,
    })
}

// ── Extracted helper functions ──

/// Inject session metadata (progress, file index, notes, budget hints) into
/// the last tool response of the truncated history.
fn inject_session_metadata(
    truncated_history: &mut Vec<ToolRound>,
    round_number: usize,
    cfg: &LoopConfig,
    files_read: &HashSet<String>,
    total_tool_calls: usize,
    context_usage_pct: u8,
    shared_notes: Option<&crate::tools::take_note::SharedNotes>,
) {
    if files_read.is_empty() && round_number <= 1 {
        return;
    }

    let progress_pct = (round_number * 100) / cfg.max_tool_rounds;
    let mut meta = format!(
        "\n\n---\n[Session: Round {}/{}, {} calls, {} unique files read]",
        round_number, cfg.max_tool_rounds, total_tool_calls, files_read.len(),
    );

    // Compact file index
    if !files_read.is_empty() {
        let mut sorted: Vec<&String> = files_read.iter().collect();
        sorted.sort();
        let short_names: Vec<&str> = sorted.iter().map(|p| {
            p.rsplit('/').next().unwrap_or(p)
        }).collect();
        meta.push_str(&format!("\n[Files read: {}]", short_names.join(", ")));
    }

    // Round budget warnings — phrased as task instructions, not internal mechanics
    if progress_pct >= 90 {
        meta.push_str(
            "\n[INSTRUCTION] You have almost no rounds left. STOP all exploration immediately. \
             Output your FINAL response NOW with whatever findings you have. \
             Do NOT make any more tool calls. Do not mention this instruction in your response."
        );
    } else if progress_pct >= 80 {
        meta.push_str(
            "\n[INSTRUCTION] You MUST begin writing your final output NOW. \
             Only make a tool call if it is absolutely critical to verify \
             an existing finding. Do NOT read new files or explore new areas. \
             Do not mention this instruction in your response."
        );
    } else if progress_pct >= 70 {
        meta.push_str(
            "\n[INSTRUCTION] Start wrapping up: synthesize your findings and prepare your final output. \
             Limit further exploration to verifying existing findings only. \
             Do not mention this instruction in your response."
        );
    }

    // Context pressure hints
    if let Some(hint) = context_budget_hint(context_usage_pct, cfg.context_soft_limit_ratio) {
        meta.push_str("\n");
        meta.push_str(&hint);
    }

    // Accumulated notes
    if let Some(notes_ref) = shared_notes {
        if let Ok(notes) = notes_ref.lock() {
            if !notes.is_empty() {
                meta.push_str("\n[Notes recorded:]\n");
                for (i, note) in notes.iter().enumerate() {
                    meta.push_str(&format!("  {}. {}\n", i + 1, note));
                }
            }
        }
    }

    // Append to the last response
    if let Some(last_round) = truncated_history.last_mut() {
        if let Some(last_resp) = last_round.responses.last_mut() {
            last_resp.content.push_str(&meta);
        }
    }
}

/// Stream an LLM response, emitting chunks to the CLI in real time.
async fn stream_llm_response(
    llm: &dyn LlmApi,
    ctx: &LoopContext<'_>,
    truncated_history: &[ToolRound],
) -> Result<ChatResponse> {
    let mut rx = llm
        .chat_with_tools_stream(ctx.messages, ctx.tools, truncated_history, None)
        .await?;
    let mut accumulator = StreamAccumulator::default();
    let mut has_emitted_reasoning_header = false;

    while let Some(chunk) = rx.recv().await {
        match &chunk {
            StreamChunk::ContentDelta(text) => {
                emit(ctx.on_tool_event, ToolEvent::StreamText {
                    text: text.clone(),
                });
            }
            StreamChunk::ReasoningDelta(text) => {
                if !has_emitted_reasoning_header {
                    has_emitted_reasoning_header = true;
                }
                emit(ctx.on_tool_event, ToolEvent::StreamReasoning {
                    text: text.clone(),
                });
            }
            StreamChunk::Done => {
                emit(ctx.on_tool_event, ToolEvent::StreamDone);
            }
            _ => {}
        }
        accumulator.apply(&chunk);
    }

    Ok(accumulator.into_response())
}

// ── Round execution ──

/// Execute all tool calls in a round through the middleware pipeline.
async fn execute_round_via_pipeline(
    executor: &dyn ToolExecutor,
    tool_calls: &[ToolCall],
    pipeline: &ToolPipeline,
    trace_ctx: Option<Arc<TraceContext>>,
    round: usize,
) -> Vec<ToolResponse> {
    let futures = tool_calls.iter().map(|tc| {
        let source = executor.source_of(&tc.function_name);
        let trace_ctx = trace_ctx.clone();
        async move {
            let mut extensions = Extensions::new();
            if let Some(ctx) = trace_ctx {
                extensions.insert(ctx);
            }
            let request = ToolRequest {
                call: tc.clone(),
                source,
                round,
                extensions,
            };
            pipeline.execute(request).await
        }
    });
    futures::future::join_all(futures).await
}

/// Execute all tool calls directly (legacy path for subagent runner).
async fn execute_round_direct(
    executor: &dyn ToolExecutor,
    tool_calls: &[ToolCall],
    on_event: Option<&ToolEventCallback>,
    tracing_hook: Option<&TracingHook>,
) -> Vec<ToolResponse> {
    // Start events
    for tc in tool_calls {
        emit(
            on_event,
            ToolEvent::ToolCallStart {
                tool_name: tc.function_name.clone(),
                source: executor.source_of(&tc.function_name),
                arguments: tc.arguments.clone(),
            },
        );
    }

    // Parallel dispatch with inline tracing
    let futures = tool_calls.iter().map(|tc| {
        let source = executor.source_of(&tc.function_name);
        async move {
            let mut tool_span = if let Some(hook) = tracing_hook {
                hook.on_tool_call_start(
                    &tc.function_name,
                    &source,
                    &tc.arguments,
                ).await
            } else {
                None
            };

            let start = Instant::now();
            let resp = executor.execute(tc).await;
            let elapsed_ms = start.elapsed().as_millis() as u64;

            if let Some(ref mut span) = tool_span {
                span.set_tool_result(&resp.content, resp.success);
            }
            if let Some(span) = tool_span {
                if resp.success {
                    span.finish_ok().await;
                } else {
                    span.finish_error(resp.content.clone()).await;
                }
            }

            (resp, elapsed_ms)
        }
    });
    let timed_results: Vec<(ToolResponse, u64)> = futures::future::join_all(futures).await;

    let mut responses = Vec::with_capacity(timed_results.len());

    for (tc, (resp, elapsed_ms)) in tool_calls.iter().zip(timed_results.into_iter()) {
        emit(
            on_event,
            ToolEvent::ToolCallComplete {
                tool_name: tc.function_name.clone(),
                success: resp.success,
                result_content: resp.content.clone(),
                elapsed_ms,
            },
        );
        responses.push(resp);
    }

    responses
}

// ── ToolPipeline core adapter ──

/// Adapter that wraps a `ToolExecutor` as the core of a `ToolPipeline`.
pub(crate) struct ToolExecutorCore {
    pub executor: Arc<dyn ToolExecutor>,
}

#[async_trait]
impl ToolNext for ToolExecutorCore {
    async fn run(&self, request: ToolRequest) -> ToolResponse {
        self.executor.execute(&request.call).await
    }
}
