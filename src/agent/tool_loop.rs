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
//! The loop **never** panics or bails on exhausted budgets / duplicate
//! stops — it surfaces those as [`LoopOutcome`] variants so the caller
//! can choose the appropriate failure mode (the main agent bails, the
//! subagent returns a partial result).

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::time::Instant;

use crate::llm::{
    ChatMessage, ChatResponse, LlmApi, TokenUsage, ToolCall, ToolResponse, ToolRound,
};
use crate::tools::{ToolEvent, ToolEventCallback};
use crate::agent_tracing::TracingHook;

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

// ── The loop itself ──

/// Run the tool-calling loop against an LLM and a tool executor.
///
/// See module docs for the high-level flow. `on_llm_response` is an
/// optional per-round hook for logging raw LLM responses; pass `None`
/// if you don't need it (subagents don't).
pub async fn run_tool_loop(
    llm: &dyn LlmApi,
    executor: &dyn ToolExecutor,
    messages: &[ChatMessage],
    tools: &[Value],
    on_event: Option<&ToolEventCallback>,
    cfg: &LoopConfig,
    on_llm_response: Option<&(dyn Fn(&ChatResponse) + Send + Sync)>,
    tracing_hook: Option<&TracingHook>,
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
        let mut llm_span = if let Some(hook) = tracing_hook {
            hook.on_llm_call_start(
                llm.model_name(),
                llm.provider_name(),
                messages,
            ).await
        } else {
            None
        };

        let response = llm
            .chat_with_tools(messages, tools, &tool_history, None)
            .await?;
        let llm_elapsed_ms = llm_start.elapsed().as_millis() as u64;

        // Finish LLM call tracing span
        if let Some(ref mut span) = llm_span {
            span.set_llm_response(&response);
        }
        if let Some(span) = llm_span {
            span.finish_ok().await;
        }

        if let Some(hook) = on_llm_response {
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
        emit(on_event, ToolEvent::LlmResponse {
            round: round_number,
            reasoning: response.reasoning_content.clone(),
            content: response.content.clone(),
            usage: response.usage.clone(),
            elapsed_ms: llm_elapsed_ms,
        });

        // Log intermediate LLM response details for tracing/debugging
        tracing::info!(
            agent = %cfg.agent_label,
            round = round_number,
            llm_elapsed_ms = llm_elapsed_ms,
            tool_calls = response.tool_calls.len(),
            content_len = response.content.len(),
            has_reasoning = response.reasoning_content.as_ref().map_or(false, |r| !r.is_empty()),
            prompt_tokens = response.usage.as_ref().and_then(|u| u.prompt_tokens),
            completion_tokens = response.usage.as_ref().and_then(|u| u.completion_tokens),
            total_tokens = response.usage.as_ref().and_then(|u| u.total_tokens),
            "LLM round response: requested tool calls"
        );

        // Log reasoning content at debug level (can be large)
        if let Some(ref reasoning) = response.reasoning_content {
            if !reasoning.is_empty() {
                tracing::debug!(
                    agent = %cfg.agent_label,
                    round = round_number,
                    reasoning_len = reasoning.len(),
                    reasoning_content = reasoning.as_str(),
                    "LLM round reasoning/thinking"
                );
            }
        }

        // Log intermediate content at debug level
        if !response.content.is_empty() {
            tracing::debug!(
                agent = %cfg.agent_label,
                round = round_number,
                content = response.content.as_str(),
                "LLM round intermediate content"
            );
        }

        if cfg.track_reasoning && response.reasoning_content.is_some() {
            last_reasoning = response.reasoning_content;
        }

        emit(on_event, ToolEvent::RoundStart { round: round_number });

        let tool_calls = response.tool_calls;
        let mut responses =
            execute_round(executor, &tool_calls, on_event, tracing_hook).await;

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
                // Preserve the triggering round in history so the trace
                // the caller emits is complete.
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

/// Execute all tool calls in a single round in parallel.
///
/// Emits `ToolCallStart` events before each dispatch, `ToolCallComplete`
/// events after each returns, and a final `RoundComplete` event.
/// Each tool call and the overall round are timed for observability.
async fn execute_round(
    executor: &dyn ToolExecutor,
    tool_calls: &[ToolCall],
    on_event: Option<&ToolEventCallback>,
    tracing_hook: Option<&TracingHook>,
) -> Vec<ToolResponse> {
    let round_start = Instant::now();

    // Start events (fire before dispatch so the UI can render spinners
    // before any async work happens).
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

    // Parallel dispatch: all calls in a round run concurrently.
    // Each call is individually timed and traced.
    let futures = tool_calls.iter().map(|tc| {
        let source = executor.source_of(&tc.function_name);
        async move {
            // Start tool call tracing span
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

            // Finish tool call tracing span
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

    // Completion events, in the same order the calls arrived.
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

    let round_elapsed_ms = round_start.elapsed().as_millis() as u64;
    emit(
        on_event,
        ToolEvent::RoundComplete {
            tool_count: responses.len(),
            elapsed_ms: round_elapsed_ms,
        },
    );

    responses
}

/// Tiny helper: fire the callback if it is set, otherwise ignore.
fn emit(callback: Option<&ToolEventCallback>, event: ToolEvent) {
    if let Some(cb) = callback {
        cb(event);
    }
}
