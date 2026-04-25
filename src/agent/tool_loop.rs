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

/// Default number of recent tool rounds whose results are kept verbatim.
/// Used as fallback when no context budget is configured.
const DEFAULT_FULL_RESULT_RECENT_ROUNDS: usize = 3;

/// Default maximum character length for tool responses in "near" older rounds.
const DEFAULT_TRUNCATED_RESULT_MAX_CHARS: usize = 500;

/// Default maximum character length for tool responses in very old rounds.
const DEFAULT_MICRO_TRUNCATED_RESULT_MAX_CHARS: usize = 120;

/// Approximate token-to-character ratio for estimating token counts from
/// character lengths.
///
/// Set to 3 rather than 4 because tool history contains a high proportion
/// of JSON structure (`{"id":"...","type":"function","function":{...}}`),
/// where short keys and punctuation average ~3 chars per token. Using 4
/// underestimates the true token count, causing the truncation algorithm
/// to trigger too late — resulting in a steep "cliff" (e.g., 240K → 113K)
/// when it finally fires and has to truncate many rounds at once.
const CHARS_PER_TOKEN: usize = 3;

/// Configuration for how tool history is truncated before sending to the LLM.
///
/// ## Strategy: Budget-based dynamic truncation
///
/// Instead of truncating based on fixed round-distance thresholds (which
/// cause premature truncation when the context is far from full), this
/// uses a **token budget** approach:
///
/// 1. If total estimated history tokens < `budget_tokens` → keep everything
///    verbatim (no truncation at all).
/// 2. If over budget → progressively truncate from oldest rounds first,
///    using three tiers: moderate → aggressive → micro, until the total
///    fits within budget.
///
/// The most recent `min_recent_rounds` are **never** truncated, ensuring
/// the model always has full context for its immediate next decision.
///
/// This eliminates the "150K peak then drop to 80K" problem where the old
/// fixed-tier strategy started truncating at round 14 regardless of how
/// much context budget remained.
#[derive(Debug, Clone)]
pub struct TruncationConfig {
    /// Token budget for the tool history portion of the context.
    /// When total estimated tokens are below this, no truncation occurs.
    /// A good default is ~60% of the model's context window.
    pub budget_tokens: usize,
    /// Minimum number of most-recent rounds that are never truncated.
    pub min_recent_rounds: usize,
    /// Truncation tier thresholds (in characters) applied from oldest first.
    /// tier 1 (moderate): first pass truncation limit.
    pub moderate_max_chars: usize,
    /// tier 2 (aggressive): second pass if still over budget.
    pub aggressive_max_chars: usize,
    /// tier 3 (micro): final pass for extreme over-budget situations.
    pub micro_max_chars: usize,
}

impl Default for TruncationConfig {
    fn default() -> Self {
        Self {
            budget_tokens: 40_000, // conservative for small context models
            min_recent_rounds: DEFAULT_FULL_RESULT_RECENT_ROUNDS,
            moderate_max_chars: DEFAULT_TRUNCATED_RESULT_MAX_CHARS,
            aggressive_max_chars: DEFAULT_MICRO_TRUNCATED_RESULT_MAX_CHARS,
            micro_max_chars: 60,
        }
    }
}

impl TruncationConfig {
    /// Build a truncation config scaled to a context window size (in tokens).
    ///
    /// Allocates ~60% of the context window to tool history budget.
    /// The remaining 40% is reserved for system prompt, user message,
    /// tool definitions, and the model's output.
    pub fn for_context_window(context_tokens: usize) -> Self {
        let budget = context_tokens * 60 / 100;

        if context_tokens >= 200_000 {
            // Large context (200K+): generous budget, gentle truncation
            Self {
                budget_tokens: budget,
                min_recent_rounds: 10,
                moderate_max_chars: 6000,
                aggressive_max_chars: 2000,
                micro_max_chars: 500,
            }
        } else if context_tokens >= 100_000 {
            // Medium context (100K-200K)
            Self {
                budget_tokens: budget,
                min_recent_rounds: 6,
                moderate_max_chars: 3000,
                aggressive_max_chars: 1000,
                micro_max_chars: 200,
            }
        } else {
            // Small context (<100K)
            Self {
                budget_tokens: budget,
                min_recent_rounds: DEFAULT_FULL_RESULT_RECENT_ROUNDS,
                moderate_max_chars: DEFAULT_TRUNCATED_RESULT_MAX_CHARS,
                aggressive_max_chars: DEFAULT_MICRO_TRUNCATED_RESULT_MAX_CHARS,
                micro_max_chars: 60,
            }
        }
    }
}

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
    /// Controls how aggressively older tool-round results are truncated.
    ///
    /// When `None`, uses conservative defaults suitable for small context
    /// windows. Callers should set this based on the model's context window
    /// size to avoid over-truncating on large-context models (e.g. 256K).
    pub truncation: Option<TruncationConfig>,
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
use crate::agent_tracing::ToolDetail;

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
    /// Optional shared notes from `take_note` tool, injected into session metadata.
    /// When `Some`, accumulated notes are included in the progress summary each round.
    pub shared_notes: Option<&'a crate::tools::take_note::SharedNotes>,
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

    // ── Session-level tracking (never truncated) ──
    // Tracks which files have been read and how many total tool calls made.
    // This is injected into the tool history as a synthetic system message
    // so the LLM knows what it has already done, even after truncation.
    let mut files_read: HashSet<String> = HashSet::new();
    let mut total_tool_calls: usize = 0;

    for round_idx in 0..cfg.max_tool_rounds {
        // Human-facing round number (1-based) for logs / events.
        let round_number = round_idx + 1;

        let llm_start = Instant::now();

        // Start LLM call tracing span
        let mut llm_span = if let Some(hook) = ctx.tracing_hook {
            // Extract detailed tool information from JSON definitions for tracing
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

        // Use streaming if:
        // 1. A tool event callback is available (interactive mode), AND
        // 2. A tool pipeline is present (lead agent only, not subagents).
        //
        // Subagents pass `on_tool_event` for tool progress display but should
        // NOT stream LLM text to the terminal — their output is collected and
        // returned as a tool result to the lead agent.
        let use_streaming = ctx.on_tool_event.is_some() && ctx.tool_pipeline.is_some();

        // Build a context-efficient view of tool history for the LLM.
        // Recent rounds keep full results; older rounds are truncated to save tokens.
        // The original `tool_history` is preserved intact for tracing and memory.
        let trunc_cfg = cfg.truncation.as_ref().cloned().unwrap_or_default();
        let mut truncated_history = truncate_tool_history(&tool_history, &trunc_cfg);

        // ── Inject session metadata into tool history ──
        // Appends a compact session summary to the last tool response in the
        // truncated history. This gives the LLM:
        // 1. Progress awareness (round X/Y, Z% budget used)
        // 2. File access index (which files already read — prevents re-reads)
        // 3. Accumulated notes from take_note
        //
        // Unlike the previous synthetic-tool-round approach (which used a fake
        // tool name `_session_progress` that APIs may ignore), this piggybacks
        // on real tool responses, ensuring the metadata always reaches the LLM.
        //
        // Cost: ~100-500 bytes per injection, negligible vs. a single file read.
        if !files_read.is_empty() || round_number > 1 {
            let progress_pct = (round_number * 100) / cfg.max_tool_rounds;
            let mut meta = format!(
                "\n\n---\n[Session: Round {}/{} ({}% budget), {} calls, {} unique files read",
                round_number, cfg.max_tool_rounds, progress_pct, total_tool_calls, files_read.len(),
            );
            meta.push(']');

            // Compact file index (just filenames, not full paths)
            if !files_read.is_empty() {
                let mut sorted: Vec<&String> = files_read.iter().collect();
                sorted.sort();
                let short_names: Vec<&str> = sorted.iter().map(|p| {
                    p.rsplit('/').next().unwrap_or(p)
                }).collect();
                meta.push_str(&format!("\n[Files read: {}]", short_names.join(", ")));
            }

            // Progress warning at 70%+
            if progress_pct >= 70 {
                meta.push_str(&format!(
                    "\n⚠️ {}% budget used — start writing your final output now.",
                    progress_pct
                ));
            }

            // Accumulated notes from take_note tool
            if let Some(notes_ref) = ctx.shared_notes {
                if let Ok(notes) = notes_ref.lock() {
                    if !notes.is_empty() {
                        meta.push_str("\n[Notes recorded:]\n");
                        for (i, note) in notes.iter().enumerate() {
                            meta.push_str(&format!("  {}. {}\n", i + 1, note));
                        }
                    }
                }
            }

            // Append to the last response of the last round in truncated history
            if let Some(last_round) = truncated_history.last_mut() {
                if let Some(last_resp) = last_round.responses.last_mut() {
                    last_resp.content.push_str(&meta);
                }
            }
        }

        let response = if use_streaming {
            // Streaming path: emit chunks to CLI in real time
            let mut rx = llm
                .chat_with_tools_stream(ctx.messages, ctx.tools, &truncated_history, None)
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
                            // Reserved for a future "thinking…" header event.
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

            accumulator.into_response()
        } else {
            // Non-streaming path (print mode, subagent runner)
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

        // Emit intermediate LLM response so the CLI can display
        // reasoning/content in real time during tool-calling rounds.
        // When streaming was used, reasoning/content were already displayed
        // via StreamText/StreamReasoning events, so we emit empty values
        // to avoid duplicate display. Usage and elapsed are still useful.
        emit(ctx.on_tool_event, ToolEvent::LlmResponse {
            round: round_number,
            reasoning: if use_streaming { None } else { response.reasoning_content.clone() },
            content: if use_streaming { String::new() } else { response.content.clone() },
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
                ctx.executor, &tool_calls, pipeline, trace_ctx, round_number,
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

        // ── Track file reads and update session metadata ──
        for tc in &tool_calls {
            total_tool_calls += 1;
            // Track files accessed by read_file, grep_search, search_files
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

        // ── Diagnostic logging (#8) ──
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

/// Build a context-efficient copy of tool history for the LLM.
///
/// ## Budget-based dynamic truncation
///
/// Unlike a fixed-tier approach (which truncates based on round distance
/// regardless of total size), this algorithm:
///
/// 1. **Estimates** the total token cost of the full history.
/// 2. If under budget → returns everything verbatim (zero truncation).
/// 3. If over budget → applies progressive truncation from the **oldest**
///    rounds first, through three tiers (moderate → aggressive → micro),
///    re-checking the budget after each tier.
///
/// The most recent `min_recent_rounds` are **never** truncated.
///
/// This ensures the context stays as close to the budget ceiling as possible
/// without premature truncation (the "150K peak then drop to 80K" problem).
///
/// Tool calls (function name + arguments) are always kept in full — they're
/// small and provide important structural context.
fn truncate_tool_history(history: &[ToolRound], cfg: &TruncationConfig) -> Vec<ToolRound> {
    if history.is_empty() {
        return Vec::new();
    }

    // Step 1: Estimate total tokens of the full (untruncated) history.
    let total_chars = estimate_history_chars(history);
    let estimated_tokens = total_chars / CHARS_PER_TOKEN;

    // Under budget? Return everything verbatim — no truncation needed.
    if estimated_tokens <= cfg.budget_tokens {
        return history.to_vec();
    }

    tracing::debug!(
        estimated_tokens,
        budget = cfg.budget_tokens,
        rounds = history.len(),
        "Tool history over budget, applying truncation"
    );

    // Step 2: Clone the history so we can mutate response content.
    let mut result: Vec<ToolRound> = history.to_vec();
    let len = result.len();

    // The most recent N rounds are protected from truncation.
    let protected_start = len.saturating_sub(cfg.min_recent_rounds);

    // Step 3: Truncate round-by-round from oldest to newest.
    //
    // For each unprotected round, try progressively harsher truncation
    // tiers until we're back within budget. This ensures we truncate
    // the *minimum* number of rounds needed, keeping newer rounds as
    // intact as possible.
    //
    // Previous approach applied each tier to ALL rounds simultaneously,
    // causing a "230K → 73K cliff". This per-round approach produces
    // a smooth decline: only the oldest 3-5 rounds get micro-truncated,
    // the rest stay at moderate or full.
    let tiers = [
        cfg.moderate_max_chars,
        cfg.aggressive_max_chars,
        cfg.micro_max_chars,
    ];

    for i in 0..protected_start {
        // Check budget before touching this round
        let current_chars = estimate_history_chars(&result);
        let current_tokens = current_chars / CHARS_PER_TOKEN;
        if current_tokens <= cfg.budget_tokens {
            break; // We're within budget — stop truncating.
        }

        // Apply progressively harsher tiers to this single round
        for &tier_limit in &tiers {
            for resp in &mut result[i].responses {
                if resp.content.len() > tier_limit {
                    let truncated = crate::tools::truncate_at_char_boundary(
                        &resp.content,
                        tier_limit,
                    );
                    resp.content = format!(
                        "{}...(truncated, {} bytes total)",
                        truncated,
                        resp.content.len()
                    );
                }
            }

            // Re-check after each tier — stop as soon as we're within budget
            let after_chars = estimate_history_chars(&result);
            let after_tokens = after_chars / CHARS_PER_TOKEN;
            if after_tokens <= cfg.budget_tokens {
                break;
            }
        }
    }

    result
}

/// Estimate the total character count of a tool history.
///
/// Counts tool call arguments + response content for each round.
/// This is a rough estimate used for budget comparison, not exact token counting.
///
/// For tool calls, we add a fixed overhead per call (~80 chars) to account
/// for the JSON wire format wrapping (`{"id":"...","type":"function",
/// "function":{"name":"...","arguments":...}}`). Without this, the estimate
/// is ~30% too low for tool-call-heavy histories.
fn estimate_history_chars(history: &[ToolRound]) -> usize {
    /// Fixed overhead per tool call for JSON structure (id, type, function wrapper).
    const TOOL_CALL_JSON_OVERHEAD: usize = 80;

    let mut total = 0;
    for round in history {
        for call in &round.calls {
            // Use byte length of the JSON value directly instead of
            // serializing to a new String each time. For Object/Array
            // values this is an undercount, but combined with the fixed
            // overhead it's close enough for budget estimation.
            let args_len = estimate_json_len(&call.arguments);
            total += call.function_name.len() + args_len + TOOL_CALL_JSON_OVERHEAD;
        }
        for resp in &round.responses {
            total += resp.content.len();
        }
    }
    total
}

/// Estimate the serialized length of a JSON value without allocating.
///
/// This avoids `value.to_string()` which allocates a new String on every call.
/// The estimate is approximate but sufficient for budget comparison.
fn estimate_json_len(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Null => 4,
        serde_json::Value::Bool(b) => if *b { 4 } else { 5 },
        serde_json::Value::Number(n) => {
            // Most numbers are < 10 digits
            let s = n.to_string();
            s.len()
        }
        serde_json::Value::String(s) => s.len() + 2, // quotes
        serde_json::Value::Array(arr) => {
            let inner: usize = arr.iter().map(estimate_json_len).sum();
            inner + arr.len().saturating_sub(1) + 2 // commas + brackets
        }
        serde_json::Value::Object(map) => {
            let inner: usize = map.iter()
                .map(|(k, v)| k.len() + 2 + 1 + estimate_json_len(v)) // "key":value
                .sum();
            inner + map.len().saturating_sub(1) + 2 // commas + braces
        }
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
