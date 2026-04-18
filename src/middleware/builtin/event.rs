//! Event middleware — emits ToolEvent callbacks for CLI rendering.
//!
//! Extracts the ToolEvent emission from `tool_loop::execute_round()` into
//! a tool-level middleware. This allows the event system to be independently
//! configured or replaced (e.g., for JSON streaming output).

use std::sync::Arc;

use async_trait::async_trait;

use crate::llm::ToolResponse;
use crate::tools::{ToolEvent, ToolEventCallback};

use super::super::{ToolMiddleware, ToolNext, ToolRequest};

/// Tool-level event middleware.
///
/// Emits `ToolCallStart` before delegation and `ToolCallComplete` after.
/// These events drive the CLI's real-time progress display (spinners,
/// tool output rendering, etc.).
///
/// Note: Currently event emission is handled inline in `tool_loop.rs`
/// because events need per-round aggregation (`RoundComplete`) that
/// doesn't fit the per-call middleware model. This struct is available
/// for future use cases that need per-call event middleware.
#[allow(dead_code)]
pub struct EventToolMiddleware {
    callback: Arc<ToolEventCallback>,
}

impl EventToolMiddleware {
    /// Create a new event middleware with the given callback.
    #[allow(dead_code)]
    pub fn new(callback: ToolEventCallback) -> Self {
        Self {
            callback: Arc::new(callback),
        }
    }
}

#[async_trait]
impl ToolMiddleware for EventToolMiddleware {
    async fn handle(
        &self,
        request: ToolRequest,
        next: &dyn ToolNext,
    ) -> ToolResponse {
        // ── Before: emit ToolCallStart ──
        (self.callback)(ToolEvent::ToolCallStart {
            tool_name: request.call.function_name.clone(),
            source: request.source.clone(),
            arguments: request.call.arguments.clone(),
        });

        let start = std::time::Instant::now();

        // ── Delegate ──
        let response = next.run(request).await;

        let elapsed_ms = start.elapsed().as_millis() as u64;

        // ── After: emit ToolCallComplete ──
        (self.callback)(ToolEvent::ToolCallComplete {
            tool_name: response.call_id.clone(), // Will be the tool name from context
            success: response.success,
            result_content: response.content.clone(),
            elapsed_ms,
        });

        response
    }

    fn name(&self) -> &str {
        "event"
    }
}
