//! Event middleware — emits ToolEvent callbacks for CLI rendering.
//!
//! Extracts the ToolEvent emission from `tool_loop::execute_round()` into
//! a tool-level middleware. This allows the event system to be independently
//! configured or replaced (e.g., for JSON streaming output).
//!
//! ## Events emitted
//!
//! - `ToolCallStart`: fired **before** delegation with tool name, source, and arguments.
//! - `ToolCallComplete`: fired **after** delegation with tool name, success, result, and timing.
//!
//! ## Note on `RoundStart` / `RoundComplete`
//!
//! Round-level events (`RoundStart`, `RoundComplete`, `LlmResponse`) are still
//! emitted by `tool_loop.rs` because they require per-round aggregation that
//! doesn't fit the per-call middleware model.

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
pub struct EventToolMiddleware {
    callback: Arc<ToolEventCallback>,
}

impl EventToolMiddleware {
    /// Create a new event middleware with the given callback.
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
        // Capture tool name before delegation (request is moved into next.run)
        let tool_name = request.call.function_name.clone();

        // ── Before: emit ToolCallStart ──
        (self.callback)(ToolEvent::ToolCallStart {
            tool_name: tool_name.clone(),
            source: request.source.clone(),
            arguments: request.call.arguments.clone(),
        });

        let start = std::time::Instant::now();

        // ── Delegate ──
        let response = next.run(request).await;

        let elapsed_ms = start.elapsed().as_millis() as u64;

        // ── After: emit ToolCallComplete ──
        (self.callback)(ToolEvent::ToolCallComplete {
            tool_name,
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
