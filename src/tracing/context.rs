//! TraceContext and SpanGuard — ergonomic span lifecycle management.

use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use tokio::sync::Mutex;

use crate::llm::{ChatMessage, ChatResponse, TokenUsage};

use super::config::ContentFlags;
use super::manager::TracingManager;
use super::types::{
    MessageSummary, Span, SpanStatus, SpanType, ToolCallSummary, ToolDetail, Trace, TraceMetadata,
};

/// Maximum characters to include in message content previews (when truncation is enabled).
const MESSAGE_PREVIEW_LEN: usize = 200;
/// Maximum characters for tool argument previews (when truncation is enabled).
const ARGS_PREVIEW_LEN: usize = 300;
/// Maximum characters for tool result previews (when truncation is enabled).
const RESULT_PREVIEW_LEN: usize = 500;

/// A context handle for an in-progress trace.
///
/// Provides ergonomic methods to start/end spans without manual ID management.
/// Maintains a stack of active span IDs for automatic parent-child linking.
///
/// The context is designed to be passed through the agent execution pipeline
/// (chat → tool_loop → tool execution → subagent) so all spans are correctly
/// nested under the trace.
pub struct TraceContext {
    manager: Arc<TracingManager>,
    trace_id: String,
    session_id: String,
    metadata: TraceMetadata,
    /// Stack of active parent span IDs. The last element is the current parent.
    span_stack: Arc<Mutex<Vec<String>>>,
    /// All completed spans (accumulated for the final trace export).
    spans: Arc<Mutex<Vec<Span>>>,
    /// When the trace started (for elapsed calculation).
    started_at: Instant,
    /// When the trace was created (wall-clock, for serialization).
    created_at: chrono::DateTime<Utc>,
    /// Accumulated token usage across all LLM calls.
    total_usage: Arc<Mutex<TokenUsage>>,
    /// Resolved content recording flags for fine-grained truncation control.
    flags: ContentFlags,
}

impl TraceContext {
    /// Create a new trace context.
    pub(super) fn new(
        manager: Arc<TracingManager>,
        session_id: &str,
        metadata: TraceMetadata,
        flags: ContentFlags,
    ) -> Self {
        let trace_id = uuid::Uuid::new_v4().to_string();

        Self {
            manager,
            trace_id,
            session_id: session_id.to_string(),
            metadata,
            span_stack: Arc::new(Mutex::new(Vec::new())),
            spans: Arc::new(Mutex::new(Vec::new())),
            started_at: Instant::now(),
            created_at: Utc::now(),
            total_usage: Arc::new(Mutex::new(TokenUsage::default())),
            flags,
        }
    }

    /// Get the trace ID.
    #[allow(dead_code)]
    pub fn trace_id(&self) -> &str {
        &self.trace_id
    }

    /// Whether tracing is actually enabled (delegates to manager).
    pub fn is_enabled(&self) -> bool {
        self.manager.is_enabled()
    }

    /// Create a forked context that shares the same trace but has an independent span stack.
    ///
    /// This is essential for **parallel execution paths** (e.g., multiple subagents
    /// running concurrently). Each fork gets its own span stack pre-seeded with
    /// `initial_parent_id`, so spans created within the fork correctly nest under
    /// the given parent without interfering with sibling forks.
    ///
    /// Shared state (`spans`, `total_usage`, `manager`) is still shared via `Arc`,
    /// so all forks contribute to the same final trace output.
    pub fn fork(&self, initial_parent_id: Option<String>) -> Self {
        let initial_stack = match initial_parent_id {
            Some(id) => vec![id],
            None => Vec::new(),
        };
        Self {
            manager: Arc::clone(&self.manager),
            trace_id: self.trace_id.clone(),
            session_id: self.session_id.clone(),
            metadata: self.metadata.clone(),
            span_stack: Arc::new(Mutex::new(initial_stack)),
            spans: Arc::clone(&self.spans),
            started_at: self.started_at,
            created_at: self.created_at,
            total_usage: Arc::clone(&self.total_usage),
            flags: self.flags,
        }
    }

    /// Notify the manager that the trace has started.
    pub async fn start(&self) {
        if !self.is_enabled() {
            return;
        }
        let trace = self.build_trace(None);
        self.manager.notify_trace_start(&trace).await;
    }

    /// Finalize the trace and notify the manager.
    ///
    /// Collects all accumulated spans and includes them in the final trace
    /// export. This must be called after all span guards have been finished.
    pub async fn finish(&self) {
        if !self.is_enabled() {
            return;
        }
        let elapsed_ms = self.started_at.elapsed().as_millis() as u64;
        let total_usage = self.total_usage.lock().await.clone();
        let spans = self.spans.lock().await.clone();
        let trace = Trace {
            total_usage: Some(total_usage),
            total_elapsed_ms: Some(elapsed_ms),
            ended_at: Some(Utc::now()),
            spans,
            ..self.build_trace(None)
        };
        self.manager.notify_trace_end(&trace).await;
    }

    /// Start an LLM call span.
    ///
    /// Returns a `SpanGuard` that must be finished with the LLM response.
    pub async fn start_llm_call(
        &self,
        model: &str,
        provider: &str,
        messages: &[ChatMessage],
        available_tools: &[ToolDetail],
    ) -> SpanGuard {
        let input_messages: Vec<MessageSummary> = messages
            .iter()
            .map(|m| MessageSummary {
                role: m.role.to_string(),
                content_preview: maybe_truncate(&m.content, MESSAGE_PREVIEW_LEN, self.flags.llm_input),
                content_len: m.content.len(),
            })
            .collect();

        let span_type = SpanType::LlmCall {
            model: model.to_string(),
            provider: provider.to_string(),
            input_messages,
            available_tools: available_tools.to_vec(),
            output_content: None,
            reasoning_content: None,
            tool_calls: Vec::new(),
            usage: None,
        };

        self.start_span(format!("llm.{}", model), span_type).await
    }

    /// Start a tool call span.
    pub async fn start_tool_call(
        &self,
        tool_name: &str,
        source: &str,
        arguments: &serde_json::Value,
    ) -> SpanGuard {
        let span_type = SpanType::ToolCall {
            tool_name: tool_name.to_string(),
            source: source.to_string(),
            arguments: arguments.clone(),
            result: None,
            success: true,
        };

        self.start_span(format!("tool.{}", tool_name), span_type).await
    }

    /// Start a subagent call span.
    pub async fn start_subagent_call(
        &self,
        agent_name: &str,
        task: &str,
    ) -> SpanGuard {
        let span_type = SpanType::SubagentCall {
            agent_name: agent_name.to_string(),
            task: maybe_truncate(task, ARGS_PREVIEW_LEN, self.flags.llm_input),
            model: None,
            result: None,
            usage: None,
            tool_rounds: 0,
        };

        self.start_span(format!("subagent.{}", agent_name), span_type).await
    }

    /// Start an agent turn span (root span for the trace).
    pub async fn start_agent_turn(&self, user_input: &str) -> SpanGuard {
        let span_type = SpanType::AgentTurn {
            user_input: maybe_truncate(user_input, MESSAGE_PREVIEW_LEN, self.flags.llm_input),
            output: None,
        };

        self.start_span("agent.turn".to_string(), span_type).await
    }

    /// Accumulate token usage from an LLM response.
    #[allow(dead_code)]
    pub async fn accumulate_usage(&self, usage: &TokenUsage) {
        if !self.is_enabled() {
            return;
        }
        self.total_usage.lock().await.accumulate(usage);
    }

    /// Snapshot the current parent span ID from the stack.
    ///
    /// Call this **before** spawning parallel futures so that all parallel
    /// spans share the same parent instead of accidentally nesting under
    /// each other.
    pub async fn current_parent_id(&self) -> Option<String> {
        self.span_stack.lock().await.last().cloned()
    }

    /// Start a tool call span with an explicit parent (for parallel dispatch).
    ///
    /// Unlike `start_tool_call`, this does **not** peek the span stack for
    /// the parent — it uses the caller-supplied `parent_id` directly. This
    /// prevents parallel tool calls from accidentally nesting under each other.
    pub async fn start_tool_call_with_parent(
        &self,
        tool_name: &str,
        source: &str,
        arguments: &serde_json::Value,
        parent_id: Option<String>,
    ) -> SpanGuard {
        let span_type = SpanType::ToolCall {
            tool_name: tool_name.to_string(),
            source: source.to_string(),
            arguments: arguments.clone(),
            result: None,
            success: true,
        };

        self.start_span_with_parent(format!("tool.{}", tool_name), span_type, parent_id).await
    }

    /// Start a subagent call span with an explicit parent (for parallel dispatch).
    ///
    /// Same rationale as `start_tool_call_with_parent`.
    #[allow(dead_code)]
    pub async fn start_subagent_call_with_parent(
        &self,
        agent_name: &str,
        task: &str,
        parent_id: Option<String>,
    ) -> SpanGuard {
        let span_type = SpanType::SubagentCall {
            agent_name: agent_name.to_string(),
            task: maybe_truncate(task, ARGS_PREVIEW_LEN, self.flags.llm_input),
            model: None,
            result: None,
            usage: None,
            tool_rounds: 0,
        };

        self.start_span_with_parent(format!("subagent.{}", agent_name), span_type, parent_id).await
    }

    // ── Internal helpers ──

    /// Start a span with automatic parent linking (from the span stack).
    ///
    /// Suitable for **sequential** span creation (e.g., LLM calls, agent turns)
    /// where each span naturally nests inside the previous one.
    ///
    /// **Not suitable for parallel dispatch** — use `start_span_with_parent`
    /// instead to avoid the race where concurrent `start_span` calls see
    /// each other's pushed span IDs.
    async fn start_span(&self, name: String, span_type: SpanType) -> SpanGuard {
        let parent_span_id = {
            let stack = self.span_stack.lock().await;
            stack.last().cloned()
        };
        self.start_span_with_parent(name, span_type, parent_span_id).await
    }

    /// Start a span with an explicit parent span ID.
    ///
    /// This is the core span creation method. The span is pushed onto the
    /// stack so that subsequent **sequential** child spans (e.g., LLM calls
    /// inside a subagent) correctly nest under it.
    async fn start_span_with_parent(
        &self,
        name: String,
        span_type: SpanType,
        parent_span_id: Option<String>,
    ) -> SpanGuard {
        let span_id = uuid::Uuid::new_v4().to_string();

        let span = Span::new(
            span_id.clone(),
            parent_span_id,
            self.trace_id.clone(),
            name,
            span_type,
        );

        // Push this span onto the stack as the new parent for nested spans
        self.span_stack.lock().await.push(span_id.clone());

        // Notify collectors
        if self.is_enabled() {
            self.manager.notify_span_start(&span).await;
        }

        SpanGuard {
            span,
            manager: Arc::clone(&self.manager),
            span_stack: Arc::clone(&self.span_stack),
            spans: Arc::clone(&self.spans),
            total_usage: Arc::clone(&self.total_usage),
            start_time: Instant::now(),
            finished: false,
            enabled: self.is_enabled(),
            flags: self.flags,
        }
    }

    /// Build a Trace struct from current state.
    fn build_trace(&self, ended_at: Option<chrono::DateTime<Utc>>) -> Trace {
        Trace {
            trace_id: self.trace_id.clone(),
            session_id: self.session_id.clone(),
            started_at: self.created_at,
            ended_at,
            spans: Vec::new(), // filled in on finish
            metadata: self.metadata.clone(),
            total_usage: None,
            total_elapsed_ms: None,
        }
    }
}

/// RAII guard that automatically ends a span when dropped.
///
/// Provides methods to record results before the span is finalized.
/// If dropped without calling `finish_ok()` or `finish_error()`, the
/// span is automatically marked as Ok.
pub struct SpanGuard {
    span: Span,
    manager: Arc<TracingManager>,
    span_stack: Arc<Mutex<Vec<String>>>,
    spans: Arc<Mutex<Vec<Span>>>,
    total_usage: Arc<Mutex<TokenUsage>>,
    start_time: Instant,
    finished: bool,
    enabled: bool,
    /// Resolved content recording flags for fine-grained truncation control.
    flags: ContentFlags,
}

impl SpanGuard {
    /// Get the span ID of this guard.
    pub fn span_id(&self) -> &str {
        &self.span.span_id
    }

    /// Record the LLM response into this span (for LlmCall spans).
    pub fn set_llm_response(&mut self, response: &ChatResponse) {
        let output_full = self.flags.llm_output;
        if let SpanType::LlmCall {
            ref mut output_content,
            ref mut reasoning_content,
            ref mut tool_calls,
            ref mut usage,
            ..
        } = self.span.span_type
        {
            *output_content = Some(maybe_truncate(&response.content, RESULT_PREVIEW_LEN, output_full));
            *reasoning_content = response
                .reasoning_content
                .as_ref()
                .map(|r| maybe_truncate(r, RESULT_PREVIEW_LEN, output_full));
            *tool_calls = response
                .tool_calls
                .iter()
                .map(|tc| ToolCallSummary {
                    function_name: tc.function_name.clone(),
                    arguments_preview: maybe_truncate(&tc.arguments.to_string(), ARGS_PREVIEW_LEN, output_full),
                })
                .collect();
            *usage = response.usage.clone();
        }
    }

    /// Record the tool result into this span (for ToolCall spans).
    pub fn set_tool_result(&mut self, result: &str, success: bool) {
        let tool_full = self.flags.tool_result;
        if let SpanType::ToolCall {
            result: ref mut span_result,
            success: ref mut span_success,
            ..
        } = self.span.span_type
        {
            *span_result = Some(maybe_truncate(result, RESULT_PREVIEW_LEN, tool_full));
            *span_success = success;
        }
    }

    /// Record the subagent result into this span (for SubagentCall spans).
    pub fn set_subagent_result(
        &mut self,
        result: &str,
        usage: Option<&TokenUsage>,
        tool_rounds: usize,
    ) {
        let tool_full = self.flags.tool_result;
        if let SpanType::SubagentCall {
            result: ref mut span_result,
            usage: ref mut span_usage,
            tool_rounds: ref mut span_rounds,
            ..
        } = self.span.span_type
        {
            *span_result = Some(maybe_truncate(result, RESULT_PREVIEW_LEN, tool_full));
            *span_usage = usage.cloned();
            *span_rounds = tool_rounds;
        }
    }

    /// Record the agent turn output (for AgentTurn spans).
    pub fn set_agent_output(&mut self, output: &str) {
        let output_full = self.flags.llm_output;
        if let SpanType::AgentTurn {
            output: ref mut o, ..
        } = self.span.span_type
        {
            *o = Some(maybe_truncate(output, RESULT_PREVIEW_LEN, output_full));
        }
    }

    /// Set an arbitrary attribute on the span.
    #[allow(dead_code)]
    pub fn set_attribute(&mut self, key: impl Into<String>, value: super::types::SpanValue) {
        self.span.attributes.insert(key.into(), value);
    }

    /// Finish the span as successful.
    pub async fn finish_ok(mut self) {
        self.do_finish(SpanStatus::Ok).await;
    }

    /// Finish the span as failed with an error message.
    pub async fn finish_error(mut self, error: String) {
        self.do_finish(SpanStatus::Error(error)).await;
    }

    /// Internal finish logic.
    async fn do_finish(&mut self, status: SpanStatus) {
        if self.finished {
            return;
        }
        self.finished = true;

        let elapsed_ms = self.start_time.elapsed().as_millis() as u64;
        self.span.ended_at = Some(Utc::now());
        self.span.elapsed_ms = Some(elapsed_ms);
        self.span.status = status;

        // Accumulate LLM usage into trace total
        if let SpanType::LlmCall { ref usage, .. } = self.span.span_type {
            if let Some(u) = usage {
                self.total_usage.lock().await.accumulate(u);
            }
        }

        // Pop this span from the parent stack
        let mut stack = self.span_stack.lock().await;
        if let Some(pos) = stack.iter().rposition(|id| *id == self.span.span_id) {
            stack.remove(pos);
        }
        drop(stack);

        // Notify collectors
        if self.enabled {
            self.manager.notify_span_end(&self.span).await;
        }

        // Store the completed span
        self.spans.lock().await.push(self.span.clone());
    }
}

impl Drop for SpanGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.finished = true;

            // Best-effort: record elapsed time and mark as Ok (we can't know
            // the real status since the caller didn't tell us).
            let elapsed_ms = self.start_time.elapsed().as_millis() as u64;
            self.span.ended_at = Some(Utc::now());
            self.span.elapsed_ms = Some(elapsed_ms);
            self.span.status = SpanStatus::Error(
                "SpanGuard dropped without explicit finish".to_string()
            );

            // Best-effort: pop from span stack (non-async, use try_lock).
            if let Ok(mut stack) = self.span_stack.try_lock() {
                if let Some(pos) = stack.iter().rposition(|id| *id == self.span.span_id) {
                    stack.remove(pos);
                }
            }

            // Best-effort: store the span so the trace is not incomplete.
            // This is the key fix — previously the span was silently lost.
            if let Ok(mut spans) = self.spans.try_lock() {
                spans.push(self.span.clone());
            }

            tracing::warn!(
                span_id = %self.span.span_id,
                span_name = %self.span.name,
                elapsed_ms,
                "SpanGuard dropped without explicit finish — span saved with Error status. \
                 Use .finish_ok() or .finish_error() instead."
            );
        }
    }
}

/// Truncate a string to at most `max_len` characters, appending "..." if truncated.
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        // Find a valid char boundary
        let mut end = max_len;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

/// Conditionally truncate based on the `full_content` flag.
/// When `full_content` is true, returns the full string without truncation.
fn maybe_truncate(s: &str, max_len: usize, full_content: bool) -> String {
    if full_content {
        s.to_string()
    } else {
        truncate(s, max_len)
    }
}
