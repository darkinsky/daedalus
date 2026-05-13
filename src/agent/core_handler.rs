//! Core turn handler — the innermost layer of the turn middleware pipeline.
//!
//! Extracted from `chat.rs` to reduce `ChatAgent`'s responsibilities.
//! This module contains:
//!
//! - [`CoreTurnHandler`]: Does the actual LLM call + tool loop.
//! - [`ToolRouterExecutor`]: Adapter from `ToolRouter` to `ToolExecutor`.
//! - Tool pipeline construction logic.
//!
//! All cross-cutting concerns (tracing, logging, memory, cost) are handled
//! by outer middleware layers. This module focuses purely on the LLM ↔ tool
//! interaction protocol.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::llm::{ChatResponse, LlmApi, ToolResponse};
use crate::middleware::builtin::confirmation::{ConfirmationSender, ConfirmationToolMiddleware};
use crate::middleware::builtin::event::EventToolMiddleware;
use crate::middleware::builtin::logging::LoggingToolMiddleware;
use crate::middleware::builtin::permission::{PermissionPolicy, PermissionToolMiddleware};
use crate::middleware::builtin::permission_rules::{PermissionMode, PermissionRuleSet};
use crate::middleware::builtin::tracing::TracingToolMiddleware;
use crate::middleware::pipeline::ToolPipeline;
use crate::middleware::{Extensions, TurnNext, TurnRequest, TurnResponse};
use crate::tools::ToolEventCallback;
use crate::agent_tracing;
use crate::hooks::config::HooksConfig;
use crate::hooks::middleware::HooksToolMiddleware;

use crate::middleware::config::MiddlewareEntry;

use super::tool_loop::{
    run_tool_loop, LoopConfig, LoopContext, LoopOutcome, LoopResult, ToolExecutor, ToolExecutorCore,
};
use super::tool_router::ToolRouter;

/// The core turn handler — does the actual LLM call + tool loop.
///
/// This is the innermost layer of the turn pipeline. All cross-cutting
/// concerns (tracing spans, logging, memory) are handled by outer
/// middleware layers. Tool-level concerns (tracing, permission, logging)
/// are handled by the tool pipeline.
pub(crate) struct CoreTurnHandler {
    llm: Arc<dyn LlmApi>,
    tool_router: Arc<ToolRouter>,
    max_tool_rounds: usize,
    on_tool_event: Option<ToolEventCallback>,
    /// Tool middleware config entries for building per-turn tool pipelines.
    tool_middleware_config: Vec<MiddlewareEntry>,
    /// Model context window size (in tokens) for truncation scaling.
    context_window: usize,
    /// Channel to send confirmation requests to the CLI layer.
    /// `None` in non-interactive mode or when permissions are bypassed.
    confirmation_tx: Option<ConfirmationSender>,
    /// Whether to bypass all permission checks.
    skip_permissions: bool,
    /// Shared session-level approved tools (persists across turns).
    session_approved: Arc<tokio::sync::Mutex<HashSet<String>>>,
    /// Shared permission rules engine (persists across turns).
    permission_rules: Arc<tokio::sync::Mutex<PermissionRuleSet>>,
    /// Resolved permission mode.
    permission_mode: PermissionMode,
    /// Hooks configuration.
    hooks_config: HooksConfig,
    /// Session ID for hooks environment variables.
    session_id: String,
}

impl CoreTurnHandler {
    /// Create a new core turn handler.
    ///
    /// All fields are private — this constructor is the only way to create
    /// an instance, enforcing encapsulation.
    pub(crate) fn new(
        llm: Arc<dyn LlmApi>,
        tool_router: Arc<ToolRouter>,
        max_tool_rounds: usize,
        on_tool_event: Option<ToolEventCallback>,
        tool_middleware_config: Vec<MiddlewareEntry>,
        context_window: usize,
        confirmation_tx: Option<ConfirmationSender>,
        skip_permissions: bool,
        session_approved: Arc<tokio::sync::Mutex<HashSet<String>>>,
        permission_rules: Arc<tokio::sync::Mutex<PermissionRuleSet>>,
        permission_mode: PermissionMode,
        hooks_config: HooksConfig,
        session_id: String,
    ) -> Self {
        Self {
            llm,
            tool_router,
            max_tool_rounds,
            on_tool_event,
            tool_middleware_config,
            context_window,
            confirmation_tx,
            skip_permissions,
            session_approved,
            permission_rules,
            permission_mode,
            hooks_config,
            session_id,
        }
    }
}

#[async_trait]
impl TurnNext for CoreTurnHandler {
    async fn run(&self, request: TurnRequest<'_>) -> Result<TurnResponse> {
        let trace_ctx = request
            .extensions
            .get::<Arc<agent_tracing::TraceContext>>()
            .cloned();

        if self.tool_router.has_tools() && self.llm.supports_tools() {
            self.run_with_tools(request, trace_ctx).await
        } else {
            self.run_simple_chat(request, trace_ctx).await
        }
    }
}

impl CoreTurnHandler {
    /// Tool-calling path: LLM + tool loop with middleware pipeline.
    async fn run_with_tools(
        &self,
        request: TurnRequest<'_>,
        trace_ctx: Option<Arc<agent_tracing::TraceContext>>,
    ) -> Result<TurnResponse> {
        let tools = self.tool_router.build_tool_definitions();
        let executor: Arc<dyn ToolExecutor> = Arc::new(ToolRouterExecutor {
            router: Arc::clone(&self.tool_router),
        });
        let cfg = LoopConfig {
            max_tool_rounds: self.max_tool_rounds,
            agent_label: "Lead agent".to_string(),
            track_reasoning: true,
            // Scale truncation to the model's actual context window size.
            truncation: Some(crate::agent::tool_loop::TruncationConfig::for_context_window(self.context_window)),
            // Enable context pressure awareness with the model's context window.
            context_window_tokens: Some(self.context_window),
            context_soft_limit_ratio: 0.7,
            context_hard_limit_ratio: 0.9,
            // Checkpoint path is set by the agent layer (not available here).
            checkpoint_path: None,
            user_input: None,
        };

        let tracing_hook = trace_ctx.map(agent_tracing::TracingHook::new);

        if let Some(ref hook) = tracing_hook {
            self.tool_router
                .set_shared_tracing_hook(Some(hook.context_arc()));
        }

        // Build tool pipeline from config
        let tool_pipeline = self.build_tool_pipeline(Arc::clone(&executor));

        let loop_ctx = LoopContext {
            executor: &*executor,
            messages: &request.messages,
            tools: &tools,
            on_tool_event: self.on_tool_event.as_ref(),
            on_llm_response: None,
            tracing_hook: tracing_hook.as_ref(),
            tool_pipeline: Some(&tool_pipeline),
            shared_notes: None, // Lead agent doesn't use take_note (subagents do)
        };

        let LoopResult {
            outcome,
            usage,
            tool_history,
        } = run_tool_loop(&*self.llm, &cfg, &loop_ctx).await?;

        self.tool_router.set_shared_tracing_hook(None);

        match outcome {
            LoopOutcome::Final { content, reasoning } => Ok(TurnResponse {
                chat_response: ChatResponse {
                    content,
                    reasoning_content: reasoning,
                    usage: Some(usage.clone()),
                    tool_calls: vec![],
                },
                tool_history,
                usage,
                extensions: Extensions::new(),
            }),
            LoopOutcome::ContextBudgetExceeded { content, reasoning } => {
                tracing::warn!("Tool loop ended due to context budget exceeded");
                Ok(TurnResponse {
                    chat_response: ChatResponse {
                        content,
                        reasoning_content: reasoning,
                        usage: Some(usage.clone()),
                        tool_calls: vec![],
                    },
                    tool_history,
                    usage,
                    extensions: Extensions::new(),
                })
            }
            LoopOutcome::DuplicateStop { message } => anyhow::bail!("{}", message),
            LoopOutcome::MaxRoundsExceeded => {
                anyhow::bail!(
                    "Exceeded maximum tool-calling rounds ({})",
                    self.max_tool_rounds
                )
            }
        }
    }

    /// Simple chat path: direct LLM call without tools.
    ///
    /// Supports streaming when a tool event callback is available (interactive mode).
    async fn run_simple_chat(
        &self,
        request: TurnRequest<'_>,
        trace_ctx: Option<Arc<agent_tracing::TraceContext>>,
    ) -> Result<TurnResponse> {
        let mut llm_guard = match trace_ctx {
            Some(ref ctx) if ctx.is_enabled() => Some(
                ctx.start_llm_call(
                    self.llm.model_name(),
                    self.llm.provider_name(),
                    &request.messages,
                    &[], // No tools in simple chat mode
                )
                .await,
            ),
            _ => None,
        };

        // Use streaming if a tool event callback is available (interactive mode)
        let llm_result = if let Some(ref callback) = self.on_tool_event {
            use crate::llm::{StreamAccumulator, StreamChunk};
            use crate::tools::ToolEvent;

            let stream_result = self.llm
                .chat_with_tools_stream(&request.messages, &[], &[], None)
                .await;

            match stream_result {
                Ok(mut rx) => {
                    let mut accumulator = StreamAccumulator::default();

                    while let Some(chunk) = rx.recv().await {
                        match &chunk {
                            StreamChunk::ContentDelta(text) => {
                                (callback)(ToolEvent::StreamText {
                                    text: text.clone(),
                                });
                            }
                            StreamChunk::ReasoningDelta(text) => {
                                (callback)(ToolEvent::StreamReasoning {
                                    text: text.clone(),
                                });
                            }
                            StreamChunk::Done => {
                                (callback)(ToolEvent::StreamDone);
                            }
                            _ => {}
                        }
                        accumulator.apply(&chunk);
                    }

                    Ok(accumulator.into_response())
                }
                Err(e) => Err(e),
            }
        } else {
            self.llm.chat(&request.messages, None).await
        };

        match llm_result {
            Err(e) => {
                if let Some(guard) = llm_guard {
                    guard.finish_error(e.to_string()).await;
                }
                Err(e)
            }
            Ok(response) => {
                if let Some(ref mut guard) = llm_guard {
                    guard.set_llm_response(&response);
                }
                if let Some(guard) = llm_guard {
                    guard.finish_ok().await;
                }
                let usage = response.usage.clone().unwrap_or_default();
                Ok(TurnResponse {
                    chat_response: response,
                    tool_history: vec![],
                    usage,
                    extensions: Extensions::new(),
                })
            }
        }
    }

    /// Build the tool middleware pipeline from config.
    ///
    /// Config order is **innermost first** (matching `.with()` semantics).
    /// Tracing middleware respects the global `tracing.enabled` flag via
    /// the `TraceContext` presence in extensions (no-op if absent).
    pub(crate) fn build_tool_pipeline(&self, executor: Arc<dyn ToolExecutor>) -> ToolPipeline {
        let core = Box::new(ToolExecutorCore { executor });
        let mut pipeline = ToolPipeline::new(core);

        if self.tool_middleware_config.is_empty() {
            // ── Default stack (innermost first) ──
            // event → confirmation → tool_logging → permission → tracing → hooks
            pipeline = self.add_tool_layer(pipeline, "event", &serde_json::Value::Null);
            pipeline = self.add_tool_layer(pipeline, "confirmation", &serde_json::Value::Null);
            pipeline = self.add_tool_layer(pipeline, "tool_logging", &serde_json::Value::Null);
            pipeline = self.add_tool_layer(pipeline, "permission", &serde_json::Value::Null);
            pipeline = self.add_tool_layer(pipeline, "tracing", &serde_json::Value::Null);
        } else {
            // ── Config-driven (innermost first, no reversal) ──
            for entry in &self.tool_middleware_config {
                if !entry.enabled {
                    continue;
                }
                pipeline = self.add_tool_layer(pipeline, &entry.name, &entry.config);
            }
        }

        // ── Hooks middleware (always outermost — runs first on request, last on response) ──
        if !self.hooks_config.is_empty() {
            pipeline = pipeline.with(Box::new(HooksToolMiddleware::new(
                Arc::new(self.hooks_config.clone()),
                self.session_id.clone(),
            )));
        }

        pipeline
    }

    /// Add a single tool middleware layer to the pipeline by name.
    ///
    /// Centralizes the name → middleware mapping so that both the default
    /// stack and the config-driven stack share the same construction logic,
    /// eliminating the previous code duplication.
    fn add_tool_layer(
        &self,
        pipeline: ToolPipeline,
        name: &str,
        config: &serde_json::Value,
    ) -> ToolPipeline {
        match name {
            "tracing" => {
                pipeline.with(Box::new(TracingToolMiddleware))
            }
            "permission" => {
                let policy = parse_permission_policy(config);
                pipeline.with(Box::new(PermissionToolMiddleware::new(policy)))
            }
            // Accept both old name "logging" and new name "tool_logging"
            "tool_logging" | "logging" => {
                pipeline.with(Box::new(LoggingToolMiddleware))
            }
            "confirmation" => {
                if let Some(ref tx) = self.confirmation_tx {
                    pipeline.with(Box::new(ConfirmationToolMiddleware::with_shared_state(
                        tx.clone(),
                        self.skip_permissions,
                        Arc::clone(&self.session_approved),
                        Arc::clone(&self.permission_rules),
                        self.permission_mode.clone(),
                    )))
                } else {
                    // No confirmation channel — skip (non-interactive or bypass mode)
                    pipeline
                }
            }
            "event" => {
                if let Some(ref cb) = self.on_tool_event {
                    pipeline.with(Box::new(EventToolMiddleware::new(Arc::clone(cb))))
                } else {
                    pipeline
                }
            }
            other => {
                tracing::warn!(
                    middleware = other,
                    "Unknown tool middleware in config, skipping"
                );
                pipeline
            }
        }
    }
}

/// Parse a `PermissionPolicy` from middleware config JSON.
///
/// Extracted to reduce cyclomatic complexity in `build_tool_pipeline`.
fn parse_permission_policy(config: &serde_json::Value) -> PermissionPolicy {
    let policy = config
        .get("policy")
        .and_then(|v| v.as_str())
        .unwrap_or("allow");
    let tool_list: Vec<String> = config
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    match policy {
        "deny_list" => PermissionPolicy::DenyList(tool_list),
        "allow_list" => PermissionPolicy::AllowList(tool_list),
        "allow" => PermissionPolicy::Allow,
        unknown => {
            tracing::warn!(
                policy = unknown,
                "Unknown permission policy in middleware config, \
                 falling back to 'allow'. Valid values: allow, deny_list, allow_list"
            );
            PermissionPolicy::Allow
        }
    }
}

/// Adapter: ToolRouter → ToolExecutor.
///
/// Holds `Arc<ToolRouter>` so it can be shared across pipeline layers
/// and satisfy the `'static` lifetime requirement.
pub(crate) struct ToolRouterExecutor {
    router: Arc<ToolRouter>,
}

#[async_trait]
impl ToolExecutor for ToolRouterExecutor {
    async fn execute(&self, call: &crate::llm::ToolCall) -> ToolResponse {
        self.router.execute(call).await
    }

    fn source_of(&self, tool_name: &str) -> String {
        if self.router.is_builtin(tool_name) {
            "built-in".to_string()
        } else {
            "mcp".to_string()
        }
    }
}
