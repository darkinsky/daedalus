//! Shared tool-calling loop, used by both `ChatAgent` and `SubagentRunner`.
//!
//! The tool-calling loop is the core algorithm of an agent: repeatedly ask
//! the LLM for a response, execute any tool calls it produces, feed the
//! results back, and stop when either a plain text answer arrives, a
//! duplicate-call streak breaches the hard-stop threshold, or the round
//! budget is exhausted.
//!
//! Before this module existed, the exact same loop was implemented twice
//! — once in `agent/chat.rs` and once in `subagent/runner.rs` — leading
//! to drift risk every time the protocol changed (round numbering, event
//! emission, duplicate detection). This module unifies both.
//!
//! ## Separation of concerns
//!
//! The loop depends on two small, injected abstractions:
//!
//! - [`ToolExecutor`]: how to actually run a single tool call and what
//!   `source` label to attach to its `ToolCallStart` event. `ChatAgent`
//!   implements it by delegating to `ToolRouter`; `SubagentRunner`
//!   implements it against a filtered `BuiltinToolRegistry`.
//! - [`LoopConfig`]: non-behavioural knobs — round budget, log label,
//!   whether to track LLM reasoning content across rounds.
//!
//! Tool-level cross-cutting concerns (tracing, logging, permission, events)
//! are handled by the optional [`ToolPipeline`] middleware, keeping the loop
//! focused on the LLM ↔ tool interaction protocol.
//!
//! The loop **never** panics or bails on exhausted budgets / duplicate
//! stops — it surfaces those as [`LoopOutcome`] variants so the caller
//! can choose the appropriate failure mode (the main agent bails, the
//! subagent returns a partial result).

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use std::time::Instant;

use crate::llm::{
    ChatMessage, ChatResponse, LlmApi, TokenUsage, ToolCall, ToolResponse, ToolRound,
};
use crate::middleware::{Extensions, ToolNext, ToolRequest};
use crate::middleware::pipeline::ToolPipeline;
use crate::tools::{ToolEvent, ToolEventCallback};
use crate::agent_tracing::{TracingHook, TraceContext};

use super::duplicate_detector::{annotate_responses, DuplicateAction, DuplicateDetector};

// ── Injected abstractions ──

/// Executes a single tool call and identifies its source for observability.
///
/// Every concrete agent brings its own executor. `ChatAgent` routes to
/// `ToolRouter` (built-in + MCP); `SubagentRunner` routes to a filtered
/// `BuiltinToolRegistry`. The loop stays oblivious to the routing rules.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Execute one tool call and return its response.
    ///
    /// The executor must always return a `ToolResponse` — transport
    /// errors should be encoded as `ToolResponse::error`, never bubbled
    /// up through `Result`, because a single failing call should not
    /// abort the entire loop.
    async fn execute(&self, call: &ToolCall) -> ToolResponse;

    /// Return the string that appears in `ToolEvent::ToolCallStart.source`.
    ///
    /// Typical values: `"built-in"`, `"mcp"`, `"subagent:<name>"`.
    fn source_of(&self, tool_name: &str) -> String;
}

/// Non-behavioural loop parameters (no side effects, just knobs).
#[derive(Debug, Clone)]
pub struct LoopConfig {
    /// Hard cap on rounds — the loop exits with `MaxRoundsExceeded` when hit.
    pub max_tool_rounds: usize,
    /// Human-readable label used in log messages (e.g. `"Lead agent"`,
    /// `"Subagent 'reviewer'"`).
    pub agent_label: String,
    /// If `true`, the last non-empty `reasoning_content` seen across rounds
    /// is forwarded into the final `LoopOutcome::Final`. Subagents don't
    /// need this — only the main chat surface uses reasoning content.
    pub track_reasoning: bool,
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
    ///
    /// The loop appended the triggering round to `tool_history` before
    /// returning so the trace stays complete.
    DuplicateStop { message: String },
    /// Round budget exhausted without reaching a final response.
    MaxRoundsExceeded,
}

/// Full loop output: outcome + accumulated bookkeeping.
pub struct LoopResult {
    pub outcome: LoopOutcome,
    pub usage: TokenUsage,
    pub tool_history: Vec<ToolRound>,
}

// ── Loop context ──

/// Runtime context for a single tool-calling loop invocation.
///
/// Groups the optional callbacks, hooks, and pipeline that were previously
/// passed as individual parameters to `run_tool_loop`. This reduces the
/// function signature from 9 parameters to 3 (`llm`, `cfg`, `ctx`), making
/// call sites clearer and easier to extend.
pub struct LoopContext<'a> {
    /// The tool executor that handles individual tool calls.
    pub executor: &'a dyn ToolExecutor,
    /// Pre-built messages from memory (system + history + user).
    pub messages: &'a [ChatMessage],
    /// Tool definitions in OpenAI function-calling JSON format.
    pub tools: &'a [Value],
    /// Optional callback for CLI event rendering (spinners, progress).
    pub on_tool_event: Option<&'a ToolEventCallback>,
    /// Optional callback invoked after each LLM response (used by subagent runner).
    pub on_llm_response: Option<&'a (dyn Fn(&ChatResponse) + Send + Sync)>,
    /// Optional tracing hook for LLM call spans and fallback tool spans.
    pub tracing_hook: Option<&'a TracingHook>,
    /// Optional tool middleware pipeline (tracing → permission → logging → event → executor).
    /// When `None`, tool calls go directly to the executor (backward compatible
    /// for subagent runner which doesn't need middleware).
    pub tool_pipeline: Option<&'a ToolPipeline>,
}

// ── The loop itself ──

/// Run the tool-calling loop against an LLM and a tool executor.
///
/// ## Tool middleware pipeline
///
/// If `ctx.tool_pipeline` is provided, each tool call is routed through the
/// pipeline (tracing → permission → logging → event → executor). If
/// `None`, tool calls go directly to the executor (backward compatible
/// for subagent runner which doesn't need middleware).
///
/// ## Tracing hook
///
/// `ctx.tracing_hook` is used exclusively for LLM call spans. Tool-level
/// tracing is handled by the tool pipeline's `TracingToolMiddleware`.
/// If no pipeline is provided, `ctx.tracing_hook` also handles tool spans
/// (fallback for subagent runner).
pub async fn run_tool_loop(
    llm: &dyn LlmApi,
    cfg: &LoopConfig,
    ctx: &LoopContext<'_>,
) -> Result<LoopResult> {
    let mut tool_history: Vec<ToolRound> = Vec::new();
    let mut total_usage = TokenUsage::default();
    let mut last_reasoning: Option<String> = None;
    let mut duplicate_detector = DuplicateDetector::new();

    for round_idx in 0..cfg.max_tool_rounds {
        // Human-facing round number (1-based) for logs / events.
        let round_number = round_idx + 1;

        let llm_start = Instant::now();

        // Start LLM call tracing span
        let mut llm_span = if let Some(hook) = ctx.tracing_hook {
            hook.on_llm_call_start(
                llm.model_name(),
                llm.provider_name(),
                ctx.messages,
            ).await
        } else {
            None
        };

        let response = llm
            .chat_with_tools(ctx.messages, ctx.tools, &tool_history, None)
            .await?;
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

        // Emit intermediate LLM response so the CLI can display
        // reasoning/content in real time during tool-calling rounds.
        emit(ctx.on_tool_event, ToolEvent::LlmResponse {
            round: round_number,
            reasoning: response.reasoning_content.clone(),
            content: response.content.clone(),
            usage: response.usage.clone(),
            elapsed_ms: llm_elapsed_ms,
        });

        if cfg.track_reasoning && response.reasoning_content.is_some() {
            last_reasoning = response.reasoning_content;
        }

        emit(ctx.on_tool_event, ToolEvent::RoundStart { round: round_number });

        let tool_calls = response.tool_calls;
        let tool_start = Instant::now();

        // Execute all tool calls — through pipeline if available, else directly.
        let mut responses = if let Some(pipeline) = ctx.tool_pipeline {
            // Extract trace context from the tracing hook for tool-level middleware
            let trace_ctx = ctx.tracing_hook.map(|h| h.context_arc());
            execute_round_via_pipeline(
                ctx.executor, &tool_calls, pipeline, trace_ctx, round_number, ctx.on_tool_event,
            ).await
        } else {
            // Legacy path: direct execution with inline tracing (for subagent runner)
            execute_round_direct(ctx.executor, &tool_calls, ctx.on_tool_event, ctx.tracing_hook).await
        };

        // Check for runaway duplicate calls and react.
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
                });
                return Ok(LoopResult {
                    outcome: LoopOutcome::DuplicateStop { message: stop_message },
                    usage: total_usage,
                    tool_history,
                });
            }
            DuplicateAction::Ok => {}
        }

        tool_history.push(ToolRound {
            calls: tool_calls,
            responses,
        });

        emit(ctx.on_tool_event, ToolEvent::RoundComplete {
            tool_count: tool_history.last().map(|r| r.calls.len()).unwrap_or(0),
            elapsed_ms: tool_start.elapsed().as_millis() as u64,
        });
    }

    // Fell off the end of the round budget.
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

// ── Internals ──

/// Execute all tool calls in a round through the middleware pipeline.
///
/// Each tool call is wrapped in a `ToolRequest` and routed through the pipeline.
/// The pipeline handles tracing spans, permission checks, logging, and event emission.
///
/// Note: Per-call events (`ToolCallStart`/`ToolCallComplete`) are now handled by
/// `EventToolMiddleware` in the pipeline. This function only dispatches requests.
async fn execute_round_via_pipeline(
    executor: &dyn ToolExecutor,
    tool_calls: &[ToolCall],
    pipeline: &ToolPipeline,
    trace_ctx: Option<Arc<TraceContext>>,
    round: usize,
    _on_event: Option<&ToolEventCallback>,
) -> Vec<ToolResponse> {
    // Parallel dispatch through pipeline
    // Per-call events (ToolCallStart/ToolCallComplete) are handled by EventToolMiddleware.
    let futures = tool_calls.iter().map(|tc| {
        let source = executor.source_of(&tc.function_name);
        let trace_ctx = trace_ctx.clone();
        async move {
            let mut extensions = Extensions::new();
            // Inject trace context for TracingToolMiddleware
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
///
/// This path is used when no tool pipeline is configured. It handles
/// tracing spans inline, preserving backward compatibility.
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

    // Note: RoundComplete is emitted by the main loop in `run_tool_loop`,
    // not here, to avoid duplicate events.

    responses
}

/// Tiny helper: fire the callback if it is set, otherwise ignore.
fn emit(callback: Option<&ToolEventCallback>, event: ToolEvent) {
    if let Some(cb) = callback {
        cb(event);
    }
}

// ── ToolPipeline core adapter ──

/// Adapter that wraps a `ToolExecutor` as the core of a `ToolPipeline`.
///
/// This sits at the innermost layer of the tool pipeline and delegates
/// to the actual `ToolExecutor` implementation. Uses `Arc<dyn ToolExecutor>`
/// to satisfy the `'static` bound required by `Box<dyn ToolNext>`.
pub(crate) struct ToolExecutorCore {
    pub executor: Arc<dyn ToolExecutor>,
}

#[async_trait]
impl ToolNext for ToolExecutorCore {
    async fn run(&self, request: ToolRequest) -> ToolResponse {
        self.executor.execute(&request.call).await
    }
}
