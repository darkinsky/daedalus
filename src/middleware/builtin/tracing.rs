//! Tracing middleware — span lifecycle management for turns and tool calls.
//!
//! Extracts all tracing/observability logic from `chat.rs` and `tool_loop.rs`
//! into the middleware pipeline. This eliminates ~45 lines of tracing boilerplate
//! from `ChatAgent::chat()` and ~25 lines from `execute_round()`.
//!
//! ## What it does
//!
//! **Turn level**: Creates a trace + agent-turn span, injects the `TraceContext`
//! into extensions for downstream middleware/core to use, and automatically
//! finishes spans on both success and error paths (the onion model handles this).
//!
//! **Tool level**: Creates a tool-call span for each tool execution, records
//! the result, and finishes the span.

use std::sync::Arc;

use async_trait::async_trait;

use crate::agent_tracing::{TracingManager, TraceContext, TraceMetadata};
use crate::agent::tool_loop::SnapshotParentSpanId;
use crate::llm::ToolResponse;

use super::super::{
    TurnMiddleware, TurnNext, TurnRequest, TurnResponse,
    ToolMiddleware, ToolNext, ToolRequest,
};

// ════════════════════════════════════════════════════════════
// Turn-level Tracing Middleware
// ════════════════════════════════════════════════════════════

/// Turn-level tracing middleware.
///
/// Wraps each turn in a trace + agent-turn span. On success, records the
/// output content; on error, records the error message. The `TraceContext`
/// is injected into `extensions` so that the core handler and tool-level
/// tracing middleware can create nested spans.
pub struct TracingTurnMiddleware {
    manager: Arc<TracingManager>,
    session_id: String,
    agent_name: Option<String>,
    model: String,
    provider: String,
}

impl TracingTurnMiddleware {
    /// Create a new tracing turn middleware.
    pub fn new(
        manager: Arc<TracingManager>,
        session_id: String,
        agent_name: Option<String>,
        model: String,
        provider: String,
    ) -> Self {
        Self { manager, session_id, agent_name, model, provider }
    }
}

#[async_trait]
impl TurnMiddleware for TracingTurnMiddleware {
    async fn handle<'a>(
        &self,
        mut request: TurnRequest<'a>,
        next: &dyn TurnNext,
    ) -> anyhow::Result<TurnResponse> {
        if !self.manager.is_enabled() {
            return next.run(request).await;
        }

        // ── Before: start trace + agent turn span ──
        let metadata = TraceMetadata {
            agent_name: self.agent_name.clone(),
            model: self.model.clone(),
            provider: self.provider.clone(),
        };
        let ctx = self.manager.start_trace(&self.session_id, metadata);
        ctx.start().await;
        let ctx = Arc::new(ctx);

        let mut turn_guard = ctx.start_agent_turn(request.user_input).await;

        // Inject trace context for downstream use
        request.extensions.insert(Arc::clone(&ctx));

        // ── Delegate ──
        let result = next.run(request).await;

        // ── After: finish spans (automatic for both Ok and Err) ──
        match &result {
            Ok(response) => {
                turn_guard.set_agent_output(&response.chat_response.content);
                turn_guard.finish_ok().await;
            }
            Err(e) => {
                turn_guard.finish_error(e.to_string()).await;
            }
        }

        ctx.finish().await;
        result
    }

    fn name(&self) -> &str {
        "tracing"
    }
}

// ════════════════════════════════════════════════════════════
// Tool-level Tracing Middleware
// ════════════════════════════════════════════════════════════

/// Tool-level tracing middleware.
///
/// Creates a tracing span for each tool call. Reads the `Arc<TraceContext>`
/// from `extensions` (injected by `TracingTurnMiddleware`) to create properly
/// nested spans.
pub struct TracingToolMiddleware;

#[async_trait]
impl ToolMiddleware for TracingToolMiddleware {
    async fn handle(
        &self,
        request: ToolRequest,
        next: &dyn ToolNext,
    ) -> ToolResponse {
        // Read trace context from extensions (injected by TracingTurnMiddleware)
        let ctx = request.extensions.get::<Arc<TraceContext>>().cloned();

        // Read the snapshotted parent span ID (injected by execute_round_via_pipeline).
        // When present, use it to ensure parallel tool calls share the same parent
        // instead of accidentally nesting under each other.
        let snapshot_parent = request.extensions.get::<SnapshotParentSpanId>().cloned();

        let mut tool_span = if let Some(ref ctx) = ctx {
            if ctx.is_enabled() {
                if let Some(SnapshotParentSpanId(parent_id)) = snapshot_parent {
                    Some(ctx.start_tool_call_with_parent(
                        &request.call.function_name,
                        &request.source,
                        &request.call.arguments,
                        parent_id,
                    ).await)
                } else {
                    Some(ctx.start_tool_call(
                        &request.call.function_name,
                        &request.source,
                        &request.call.arguments,
                    ).await)
                }
            } else {
                None
            }
        } else {
            None
        };

        // ── Delegate ──
        let response = next.run(request).await;

        // ── After: finish tool span ──
        if let Some(ref mut span) = tool_span {
            span.set_tool_result(&response.content, response.success);
        }
        if let Some(span) = tool_span {
            if response.success {
                span.finish_ok().await;
            } else {
                span.finish_error(response.content.clone()).await;
            }
        }

        response
    }

    fn name(&self) -> &str {
        "tracing"
    }
}
