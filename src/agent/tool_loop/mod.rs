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
pub(crate) mod checkpoint;
pub(crate) mod plan_tracker;
pub(crate) mod hierarchical_compression;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
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

use super::duplicate_detector::{annotate_responses, fingerprint, DuplicateAction, DuplicateDetector};

// Re-export public types from submodules.
pub use truncation::TruncationConfig;
pub(crate) use truncation::{truncate_tool_history, estimate_history_chars, CHARS_PER_TOKEN};
pub(crate) use context_pressure::{
    estimate_context_usage_pct, context_budget_hint, force_final_response, emit,
};
pub(crate) use hierarchical_compression::compress_hierarchically;

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

    /// Execute a tool call with optional streaming output callback.
    ///
    /// For tools that support streaming (e.g., bash), the `on_output` callback
    /// is invoked with each line of output as it arrives. The full result is
    /// still collected and returned as a `ToolResponse`.
    ///
    /// Default implementation ignores the callback and delegates to `execute()`.
    async fn execute_streaming(
        &self,
        call: &ToolCall,
        _on_output: Option<Arc<dyn Fn(String) + Send + Sync>>,
    ) -> ToolResponse {
        self.execute(call).await
    }

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

    // ── Checkpoint / resume ──

    /// Optional path to save tool loop checkpoints for crash recovery.
    /// When set, the loop saves state every N rounds (see `CHECKPOINT_INTERVAL`).
    pub checkpoint_path: Option<std::path::PathBuf>,
    /// The user's original input (needed for checkpoint metadata).
    pub user_input: Option<String>,

    // ── Resume from checkpoint ──

    /// Initial tool history to restore from a checkpoint (for `/resume`).
    /// When non-empty, the loop starts with this history pre-populated.
    pub initial_tool_history: Vec<ToolRound>,
    /// Initial token usage to restore from a checkpoint.
    pub initial_usage: Option<TokenUsage>,
    /// Initial round offset (so round numbering continues from checkpoint).
    pub initial_round_offset: usize,
    /// Initial total tool calls count from checkpoint.
    pub initial_total_tool_calls: usize,
    /// Initial files_read set from checkpoint.
    pub initial_files_read: HashSet<String>,
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
    /// Optional shared plan from `create_plan`/`update_plan` tools.
    pub shared_plan: Option<&'a plan_tracker::SharedPlan>,
}

// ── The loop itself ──

/// Run the tool-calling loop against an LLM and a tool executor.
pub async fn run_tool_loop(
    llm: &dyn LlmApi,
    cfg: &LoopConfig,
    ctx: &LoopContext<'_>,
) -> Result<LoopResult> {
    // Initialize from checkpoint state if resuming, otherwise start fresh.
    let mut tool_history: Vec<ToolRound> = cfg.initial_tool_history.clone();
    let mut total_usage = cfg.initial_usage.clone().unwrap_or_default();
    let mut last_reasoning: Option<String> = None;
    let mut duplicate_detector = DuplicateDetector::new();
    let mut read_only_cache = ReadOnlyCache::new();

    // ── Session-level tracking (never truncated) ──
    let mut files_read: HashSet<String> = cfg.initial_files_read.clone();
    let mut total_tool_calls: usize = cfg.initial_total_tool_calls;

    for round_idx in 0..cfg.max_tool_rounds {
        let round_number = round_idx + 1 + cfg.initial_round_offset;

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

        // ── Hierarchical compression (L1/L2/L3 based on context health) ──
        let health = context_pressure::assess_context_health(
            &truncated_history,
            round_number,
            context_usage_pct,
            cfg.context_window_tokens,
        );
        if health.severity > context_pressure::ContextHealthSeverity::Healthy {
            let compression_cfg = cfg.context_window_tokens
                .map(|cw| hierarchical_compression::CompressionConfig::for_context_window(cw))
                .unwrap_or_default();
            let (compressed, _stats) = compress_hierarchically(
                &truncated_history,
                health.severity,
                &compression_cfg,
            );
            truncated_history = compressed;
        }

        // ── Inject session metadata into tool history ──
        inject_session_metadata(
            &mut truncated_history,
            round_number,
            cfg,
            &files_read,
            total_tool_calls,
            context_usage_pct,
            ctx.shared_notes,
            ctx.shared_plan,
        );

        let response = if use_streaming {
            match stream_llm_response(llm, ctx, &truncated_history).await {
                Ok(r) => r,
                Err(e) => {
                    // Finish LLM span with error before propagating
                    if let Some(span) = llm_span {
                        span.finish_error(e.to_string()).await;
                    }
                    return Err(e);
                }
            }
        } else {
            match llm.chat_with_tools(ctx.messages, ctx.tools, &truncated_history, None).await {
                Ok(r) => r,
                Err(e) => {
                    // Finish LLM span with error before propagating
                    if let Some(span) = llm_span {
                        span.finish_error(e.to_string()).await;
                    }
                    return Err(e);
                }
            }
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
            // Clear checkpoint on successful completion
            if let Some(ref cp_path) = cfg.checkpoint_path {
                checkpoint::ToolLoopCheckpoint::clear(cp_path);
            }
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

        // ── Metadata-only short-circuit ──
        // If the LLM produced a non-empty content response AND all tool calls
        // are metadata-only (e.g., update_plan, take_note), execute the tools
        // for their side effects but return the content as the final answer
        // without an additional LLM round-trip.
        let all_metadata_only = !response.content.trim().is_empty()
            && response.tool_calls.iter().all(|tc| is_metadata_only_tool(&tc.function_name));

        if all_metadata_only {
            tracing::debug!(
                agent = %cfg.agent_label,
                round = round_number,
                tool_count = response.tool_calls.len(),
                "All tool calls are metadata-only with non-empty content — executing and short-circuiting"
            );

            // Execute the metadata-only tools for their side effects
            let (responses, _) = execute_with_cache(
                ctx, &response.tool_calls, &mut read_only_cache, round_number,
            ).await;

            // Record this round in tool_history for audit/replay purposes
            let round_reasoning = response.reasoning_content.clone();
            tool_history.push(ToolRound {
                calls: response.tool_calls,
                responses,
                reasoning_content: round_reasoning,
            });

            // Clear checkpoint on successful completion
            if let Some(ref cp_path) = cfg.checkpoint_path {
                checkpoint::ToolLoopCheckpoint::clear(cp_path);
            }
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

        // Invalidate read-only cache if any write tool is in this round
        if tool_calls.iter().any(|tc| is_write_tool(&tc.function_name)) {
            read_only_cache.invalidate();
        }

        // Execute all tool calls (with read-only cache for eligible tools)
        let (mut responses, cache_hits) = execute_with_cache(
            ctx, &tool_calls, &mut read_only_cache, round_number,
        ).await;

        // Check for runaway duplicate calls.
        // Exclude cache-hit calls from duplicate detection — they are zero-cost
        // and should not count toward the "LLM is stuck" streak.
        let non_cached_calls: Vec<ToolCall> = tool_calls.iter().enumerate()
            .filter(|(i, _)| !cache_hits.contains(i))
            .map(|(_, tc)| tc.clone())
            .collect();
        match duplicate_detector.record_round(&non_cached_calls) {
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

        // ── Auto-checkpoint for crash recovery ──
        if let Some(ref cp_path) = cfg.checkpoint_path {
            if round_number % checkpoint::CHECKPOINT_INTERVAL == 0 {
                let user_input = cfg.user_input.as_deref().unwrap_or("");
                let cp = checkpoint::ToolLoopCheckpoint::new(
                    user_input,
                    &tool_history,
                    &total_usage,
                    round_number,
                    total_tool_calls,
                    &files_read,
                );
                if let Err(e) = cp.save(cp_path) {
                    tracing::warn!(error = %e, "Failed to save tool loop checkpoint");
                }
            }
        }
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
///
/// Also implements "information saturation detection": if the agent has been
/// running for many rounds without recording new notes (via take_note), it's
/// likely that further exploration has diminishing returns. In this case,
/// inject a stronger convergence hint even before the round budget warning fires.
fn inject_session_metadata(
    truncated_history: &mut Vec<ToolRound>,
    round_number: usize,
    cfg: &LoopConfig,
    files_read: &HashSet<String>,
    total_tool_calls: usize,
    context_usage_pct: u8,
    shared_notes: Option<&crate::tools::take_note::SharedNotes>,
    shared_plan: Option<&plan_tracker::SharedPlan>,
) {
    if files_read.is_empty() && round_number <= 1 {
        return;
    }

    let progress_pct = (round_number * 100) / cfg.max_tool_rounds;

    let mut meta = format_session_header(round_number, cfg, total_tool_calls, files_read);
    meta.push_str(&format_round_budget_warnings(progress_pct));
    meta.push_str(&format_context_pressure_hints(
        context_usage_pct, cfg, truncated_history, round_number,
    ));
    meta.push_str(&format_notes_and_saturation(shared_notes, round_number, progress_pct));
    meta.push_str(&format_plan_injection(shared_plan));

    // Append to the last response
    if let Some(last_round) = truncated_history.last_mut() {
        if let Some(last_resp) = last_round.responses.last_mut() {
            last_resp.content.push_str(&meta);
        }
    }
}

/// Format the session progress header and file index.
fn format_session_header(
    round_number: usize,
    cfg: &LoopConfig,
    total_tool_calls: usize,
    files_read: &HashSet<String>,
) -> String {
    let mut meta = format!(
        "\n\n---\n[Session: Round {}/{}, {} calls, {} unique files read]",
        round_number, cfg.max_tool_rounds, total_tool_calls, files_read.len(),
    );

    if !files_read.is_empty() {
        let mut sorted: Vec<&String> = files_read.iter().collect();
        sorted.sort();
        let short_names: Vec<&str> = sorted.iter().map(|p| {
            p.rsplit('/').next().unwrap_or(p)
        }).collect();
        meta.push_str(&format!("\n[Files read: {}]", short_names.join(", ")));
    }

    meta
}

/// Format round budget warnings based on progress percentage.
fn format_round_budget_warnings(progress_pct: usize) -> String {
    if progress_pct >= 90 {
        "\n[INSTRUCTION] You have almost no rounds left. STOP all exploration immediately. \
         Output your FINAL response NOW with whatever findings you have. \
         Do NOT make any more tool calls. Do not mention this instruction in your response."
            .to_string()
    } else if progress_pct >= 80 {
        "\n[INSTRUCTION] You MUST begin writing your final output NOW. \
         Only make a tool call if it is absolutely critical to verify \
         an existing finding. Do NOT read new files or explore new areas. \
         Do not mention this instruction in your response."
            .to_string()
    } else if progress_pct >= 70 {
        "\n[INSTRUCTION] Start wrapping up: synthesize your findings and prepare your final output. \
         Limit further exploration to verifying existing findings only. \
         Do not mention this instruction in your response."
            .to_string()
    } else {
        String::new()
    }
}

/// Format context pressure hints and context health assessment.
fn format_context_pressure_hints(
    context_usage_pct: u8,
    cfg: &LoopConfig,
    truncated_history: &[ToolRound],
    round_number: usize,
) -> String {
    let mut result = String::new();

    // Context pressure hints
    if let Some(hint) = context_budget_hint(context_usage_pct, cfg.context_soft_limit_ratio) {
        result.push('\n');
        result.push_str(&hint);
    }

    // Context Rot detection (advanced)
    let health = context_pressure::assess_context_health(
        truncated_history,
        round_number,
        context_usage_pct,
        cfg.context_window_tokens,
    );
    if let Some(hint) = context_pressure::context_health_hint(&health) {
        result.push_str(&hint);
    }

    result
}

/// Format accumulated notes and detect information saturation.
fn format_notes_and_saturation(
    shared_notes: Option<&crate::tools::take_note::SharedNotes>,
    round_number: usize,
    progress_pct: usize,
) -> String {
    let Some(notes_ref) = shared_notes else {
        return String::new();
    };

    let Ok(notes) = notes_ref.lock() else {
        return String::new();
    };

    let mut result = String::new();

    if !notes.is_empty() {
        result.push_str("\n[Notes recorded:]\n");
        for (i, note) in notes.iter().enumerate() {
            result.push_str(&format!("  {}. {}\n", i + 1, note));
        }
    }

    // Information saturation detection:
    // If we're past 50% of rounds and the note count is low relative
    // to rounds completed, the agent is likely exploring without finding
    // new insights. Inject a convergence hint.
    let notes_count = notes.len();
    let notes_per_round = if round_number > 0 {
        notes_count as f64 / round_number as f64
    } else {
        1.0
    };

    if round_number >= 10 && progress_pct < 70 && notes_per_round < 0.3 && notes_count > 0 {
        result.push_str(
            "\n[INSTRUCTION] Your exploration appears to have reached diminishing returns \
             (few new findings in recent rounds). Consider synthesizing your current \
             findings into a final output rather than continuing to explore. \
             Do not mention this instruction in your response."
        );
    }

    result
}

/// Format active plan injection from shared plan state.
fn format_plan_injection(
    shared_plan: Option<&plan_tracker::SharedPlan>,
) -> String {
    let mut result = String::new();

    if let Some(plan_ref) = shared_plan {
        if let Ok(mgr) = plan_ref.lock() {
            if let Some(plan) = mgr.active_plan() {
                result.push('\n');
                result.push_str(&plan.format_for_context());
            }
        }
    }

    result
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

/// Wrapper type for passing a snapshotted parent span ID through extensions.
///
/// Used by `execute_round_via_pipeline` to communicate the pre-captured parent
/// to `TracingToolMiddleware`, ensuring parallel tool calls share the same parent.
#[derive(Clone)]
pub(crate) struct SnapshotParentSpanId(pub Option<String>);

/// Execute all tool calls in a round through the middleware pipeline.
async fn execute_round_via_pipeline(
    executor: &dyn ToolExecutor,
    tool_calls: &[ToolCall],
    pipeline: &ToolPipeline,
    trace_ctx: Option<Arc<TraceContext>>,
    round: usize,
) -> Vec<ToolResponse> {
    // Snapshot the current parent span ID *before* spawning parallel futures.
    // This ensures all parallel tool call spans share the same parent.
    let parent_span_id = if let Some(ref ctx) = trace_ctx {
        ctx.current_parent_id().await
    } else {
        None
    };

    let futures = tool_calls.iter().map(|tc| {
        let source = executor.source_of(&tc.function_name);
        let trace_ctx = trace_ctx.clone();
        let parent_id = parent_span_id.clone();
        async move {
            let mut extensions = Extensions::new();
            if let Some(ctx) = trace_ctx {
                extensions.insert(ctx);
            }
            extensions.insert(SnapshotParentSpanId(parent_id));
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

    // Snapshot the current parent span ID *before* spawning parallel futures.
    // This ensures all parallel tool call spans share the same parent instead
    // of accidentally nesting under each other (span stack race fix).
    let parent_span_id = if let Some(hook) = tracing_hook {
        hook.snapshot_parent_id().await
    } else {
        None
    };

    // Parallel dispatch with inline tracing
    let futures = tool_calls.iter().map(|tc| {
        let source = executor.source_of(&tc.function_name);
        let parent_id = parent_span_id.clone();
        async move {
            let mut tool_span = if let Some(hook) = tracing_hook {
                hook.on_tool_call_start_with_parent(
                    &tc.function_name,
                    &source,
                    &tc.arguments,
                    parent_id,
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
    /// Optional event callback for streaming bash output.
    pub on_tool_event: Option<ToolEventCallback>,
}

#[async_trait]
impl ToolNext for ToolExecutorCore {
    async fn run(&self, request: ToolRequest) -> ToolResponse {
        // For bash tool, use streaming execution if we have an event callback.
        if request.call.function_name == "bash" {
            if let Some(ref callback) = self.on_tool_event {
                let cb = Arc::clone(callback);
                let on_output: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |line: String| {
                    (cb)(ToolEvent::BashStreamLine { line });
                });
                return self.executor.execute_streaming(&request.call, Some(on_output)).await;
            }
        }
        self.executor.execute(&request.call).await
    }
}

// ── Read-only tool result cache ──

/// Tools whose results are safe to cache (read-only, deterministic within a session).
const CACHEABLE_TOOLS: &[&str] = &[
    "list_directory",
    "search_files",
    "get_file_info",
];

/// Tools that modify the filesystem or state, triggering cache invalidation.
fn is_write_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "edit_file" | "multi_edit" | "write_file" | "bash"
    )
}

/// Tools that only update internal metadata (plan state, notes) without producing
/// results the LLM needs to reason about. When the LLM emits a final answer
/// alongside only metadata-only tool calls, the loop can short-circuit: execute
/// the tools for side effects and return the content without another LLM round.
fn is_metadata_only_tool(tool_name: &str) -> bool {
    matches!(tool_name, "update_plan" | "take_note")
}

/// Lightweight read-only tool result cache.
///
/// Caches results from deterministic read-only tools (like `list_directory`)
/// to avoid redundant calls when the LLM requests the same information
/// multiple times within a session.
///
/// Cache is invalidated when any write tool (`edit_file`, `write_file`,
/// `bash`) is executed, since those may change the filesystem state.
struct ReadOnlyCache {
    /// fingerprint → cached result content.
    entries: HashMap<String, String>,
    /// Number of cache hits (for logging).
    hits: usize,
}

impl ReadOnlyCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            hits: 0,
        }
    }

    /// Check if a tool call has a cached result.
    fn get(&self, call: &ToolCall) -> Option<&String> {
        if !CACHEABLE_TOOLS.contains(&call.function_name.as_str()) {
            return None;
        }
        let fp = fingerprint(call);
        self.entries.get(&fp)
    }

    /// Store a successful tool result in the cache.
    fn put(&mut self, call: &ToolCall, content: &str) {
        if !CACHEABLE_TOOLS.contains(&call.function_name.as_str()) {
            return;
        }
        let fp = fingerprint(call);
        self.entries.insert(fp, content.to_string());
    }

    /// Invalidate all cached entries (called when a write tool executes).
    fn invalidate(&mut self) {
        if !self.entries.is_empty() {
            tracing::debug!(
                entries = self.entries.len(),
                "Read-only cache invalidated due to write operation"
            );
            self.entries.clear();
        }
    }
}

/// Execute tool calls with read-only caching.
///
/// For cacheable read-only tools, checks the cache first. On cache hit,
/// returns the cached result immediately (with a hint appended so the LLM
/// knows it's seeing cached data). On cache miss, executes normally and
/// stores the result for future use.
///
/// Non-cacheable tools are always executed normally.
/// Returns (responses, cache_hit_indices) — the set of indices that were served from cache.
async fn execute_with_cache(
    ctx: &LoopContext<'_>,
    tool_calls: &[ToolCall],
    cache: &mut ReadOnlyCache,
    round_number: usize,
) -> (Vec<ToolResponse>, HashSet<usize>) {
    // Partition tool calls into cached hits and calls that need execution.
    let mut results: Vec<Option<ToolResponse>> = vec![None; tool_calls.len()];
    let mut to_execute: Vec<(usize, &ToolCall)> = Vec::new();
    let mut hit_indices: HashSet<usize> = HashSet::new();

    for (i, tc) in tool_calls.iter().enumerate() {
        if let Some(cached_content) = cache.get(tc).cloned() {
            cache.hits += 1;
            hit_indices.insert(i);
            tracing::debug!(
                tool = %tc.function_name,
                cache_hits = cache.hits,
                round = round_number,
                "Read-only cache hit — returning cached result"
            );
            let content = format!(
                "{}\n\n[cached: this is the same result as a previous identical call]",
                &cached_content
            );
            results[i] = Some(ToolResponse::new(&tc.call_id, content));
        } else {
            to_execute.push((i, tc));
        }
    }

    // If all calls were cache hits, return immediately.
    if to_execute.is_empty() {
        return (results.into_iter().map(|r| r.unwrap()).collect(), hit_indices);
    }

    // Execute non-cached calls through the normal pipeline.
    let uncached_calls: Vec<ToolCall> = to_execute.iter().map(|(_, tc)| (*tc).clone()).collect();
    let executed_responses = if let Some(pipeline) = ctx.tool_pipeline {
        let trace_ctx = ctx.tracing_hook.map(|h| h.context_arc());
        execute_round_via_pipeline(
            ctx.executor, &uncached_calls, pipeline, trace_ctx, round_number,
        ).await
    } else {
        execute_round_direct(ctx.executor, &uncached_calls, ctx.on_tool_event, ctx.tracing_hook).await
    };

    // Merge executed results back and populate cache.
    for ((original_idx, tc), resp) in to_execute.into_iter().zip(executed_responses.into_iter()) {
        // Only cache successful results from cacheable tools.
        if resp.success {
            cache.put(tc, &resp.content);
        }
        results[original_idx] = Some(resp);
    }

    (results.into_iter().map(|r| r.unwrap()).collect(), hit_indices)
}
