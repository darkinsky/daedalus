//! Logging middleware — structured request/response logging for turns and tool calls.
//!
//! Extracts the `log_request()` and `log_response()` methods from `ChatAgent`
//! into middleware so they can be independently configured or disabled.

use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;

use crate::llm::{ChatResponse, ToolResponse, format_messages_for_log};
use crate::tools::truncate_at_char_boundary;

use super::super::{
    TurnMiddleware, TurnNext, TurnRequest, TurnResponse,
    ToolMiddleware, ToolNext, ToolRequest,
};

// ════════════════════════════════════════════════════════════
// Turn-level Logging Middleware
// ════════════════════════════════════════════════════════════

/// Logs the LLM request (user input + messages) before delegation,
/// and the LLM response (content + usage) after.
pub struct LoggingTurnMiddleware {
    /// Session ID for structured log correlation.
    pub session_id: String,
    /// Provider name (e.g., "Venus").
    pub provider: String,
    /// Model name (e.g., "claude-sonnet-4-6").
    pub model: String,
    /// Auto-incrementing request counter for log correlation.
    request_counter: AtomicU64,
}

impl LoggingTurnMiddleware {
    /// Create a new logging middleware.
    pub fn new(session_id: String, provider: String, model: String) -> Self {
        Self {
            session_id,
            provider,
            model,
            request_counter: AtomicU64::new(0),
        }
    }
}

#[async_trait]
impl TurnMiddleware for LoggingTurnMiddleware {
    async fn handle<'a>(
        &self,
        request: TurnRequest<'a>,
        next: &dyn TurnNext,
    ) -> anyhow::Result<TurnResponse> {
        let request_id = self.request_counter.fetch_add(1, Ordering::Relaxed) + 1;

        // ── Before: log request ──
        let llm_input = format_messages_for_log(&request.messages);
        // Log full LLM input at DEBUG level only to prevent sensitive data leakage.
        tracing::debug!(
            session_id = %self.session_id,
            request_id = request_id,
            llm_input = llm_input.as_str(),
            "LLM request: full message context (DEBUG)"
        );
        let user_input_preview = truncate_at_char_boundary(request.user_input, 500);
        tracing::info!(
            session_id = %self.session_id,
            request_id = request_id,
            provider = %self.provider,
            model = %self.model,
            role = "user",
            message = &*user_input_preview,
            message_count = request.messages.len(),
            "LLM request: user input"
        );

        // ── Delegate ──
        let response = next.run(request).await?;

        // ── After: log response ──
        Self::log_response(&self.session_id, request_id, &self.provider, &self.model, &response.chat_response);

        Ok(response)
    }

    fn name(&self) -> &str {
        "request_logging"
    }
}

impl LoggingTurnMiddleware {
    fn log_response(session_id: &str, request_id: u64, provider: &str, model: &str, response: &ChatResponse) {
        // Log reasoning at debug level
        if let Some(ref reasoning) = response.reasoning_content {
            if !reasoning.is_empty() {
                tracing::debug!(
                    session_id = %session_id,
                    request_id = request_id,
                    reasoning_len = reasoning.len(),
                    reasoning_content = reasoning.as_str(),
                    "LLM response: reasoning/thinking"
                );
            }
        }

        // Log tool calls at debug level
        if !response.tool_calls.is_empty() {
            let tool_calls_summary: Vec<String> = response.tool_calls.iter().map(|tc| {
                format!("{}({})", tc.function_name, truncate_at_char_boundary(&tc.arguments.to_string(), 200))
            }).collect();
            tracing::debug!(
                session_id = %session_id,
                request_id = request_id,
                tool_calls = %tool_calls_summary.join(", "),
                "LLM response: tool calls requested"
            );
        }

        // Truncate content for INFO level to prevent sensitive data leakage.
        // Full content is available at DEBUG level.
        let content_preview = truncate_at_char_boundary(&response.content, 500);
        let is_truncated = response.content.len() > 500;
        if is_truncated {
            tracing::debug!(
                session_id = %session_id,
                request_id = request_id,
                full_content = response.content.as_str(),
                "LLM response: full content (DEBUG)"
            );
        }
        tracing::info!(
            session_id = %session_id,
            request_id = request_id,
            provider = %provider,
            model = %model,
            role = "assistant",
            message = &*content_preview,
            content_len = response.content.len(),
            content_truncated = is_truncated,
            has_reasoning = response.reasoning_content.as_ref().map_or(false, |r| !r.is_empty()),
            reasoning_len = response.reasoning_content.as_ref().map_or(0, |r| r.len()),
            tool_call_count = response.tool_calls.len(),
            prompt_tokens = response.usage.as_ref().and_then(|u| u.prompt_tokens),
            completion_tokens = response.usage.as_ref().and_then(|u| u.completion_tokens),
            total_tokens = response.usage.as_ref().and_then(|u| u.total_tokens),
            "LLM response: assistant output"
        );
    }
}

// ════════════════════════════════════════════════════════════
// Tool-level Logging Middleware
// ════════════════════════════════════════════════════════════

/// Emits structured log entries for each tool call (before and after execution).
pub struct LoggingToolMiddleware;

#[async_trait]
impl ToolMiddleware for LoggingToolMiddleware {
    async fn handle(
        &self,
        request: ToolRequest,
        next: &dyn ToolNext,
    ) -> ToolResponse {
        let tool_name = request.call.function_name.clone();
        let source = request.source.clone();
        let round = request.round;

        tracing::info!(
            tool = %tool_name,
            source = %source,
            round = round,
            "Tool call: starting"
        );

        let start = std::time::Instant::now();
        let response = next.run(request).await;
        let elapsed_ms = start.elapsed().as_millis() as u64;

        tracing::info!(
            tool = %tool_name,
            source = %source,
            round = round,
            success = response.success,
            result_len = response.content.len(),
            elapsed_ms = elapsed_ms,
            "Tool call: completed"
        );

        response
    }

    fn name(&self) -> &str {
        "tool_logging"
    }
}
