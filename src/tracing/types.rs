//! Core type definitions for the tracing subsystem.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::llm::TokenUsage;

/// A unique trace represents one user message → final response cycle.
///
/// Contains the full span tree for a single agent turn, including all
/// LLM calls, tool executions, and subagent invocations.
#[derive(Debug, Clone)]
pub struct Trace {
    /// Unique identifier for this trace.
    pub trace_id: String,
    /// Session identifier (correlates multiple traces in a conversation).
    pub session_id: String,
    /// When the trace started (user message received).
    pub started_at: DateTime<Utc>,
    /// When the trace ended (final response produced).
    pub ended_at: Option<DateTime<Utc>>,
    /// All spans in this trace (flat list, parent_span_id links them).
    pub spans: Vec<Span>,
    /// Metadata about the execution environment.
    pub metadata: TraceMetadata,
    /// Accumulated token usage across all LLM calls in this trace.
    pub total_usage: Option<TokenUsage>,
    /// Total wall-clock time in milliseconds.
    pub total_elapsed_ms: Option<u64>,
}

/// Metadata about the trace environment.
#[derive(Debug, Clone)]
pub struct TraceMetadata {
    /// Agent name (e.g., "Daedalus", or subagent name).
    pub agent_name: Option<String>,
    /// Model identifier used for the primary LLM.
    pub model: String,
    /// Provider name (e.g., "Venus", "GenAI").
    pub provider: String,
}

/// A span represents a single operation in the call chain.
///
/// Spans form a tree via `parent_span_id`. The root span has `parent_span_id = None`.
#[derive(Debug, Clone)]
pub struct Span {
    /// Unique identifier for this span.
    pub span_id: String,
    /// Parent span ID (None for root spans).
    pub parent_span_id: Option<String>,
    /// Trace this span belongs to.
    pub trace_id: String,
    /// Human-readable operation name.
    pub name: String,
    /// The type of operation with type-specific data.
    pub span_type: SpanType,
    /// When this span started.
    pub started_at: DateTime<Utc>,
    /// When this span ended (None if still running).
    pub ended_at: Option<DateTime<Utc>>,
    /// Current status of the span.
    pub status: SpanStatus,
    /// Arbitrary key-value attributes for extensibility.
    pub attributes: HashMap<String, SpanValue>,
    /// Wall-clock duration in milliseconds (set when span ends).
    pub elapsed_ms: Option<u64>,
}

/// The type of operation a span represents, with type-specific payload.
#[derive(Debug, Clone)]
pub enum SpanType {
    /// A complete agent turn (root span for the trace).
    AgentTurn {
        /// The user's input message.
        user_input: String,
        /// The final response content.
        output: Option<String>,
    },
    /// An LLM API call.
    LlmCall {
        /// Model used for this call.
        model: String,
        /// Provider name.
        provider: String,
        /// Summary of input messages (role + truncated content).
        input_messages: Vec<MessageSummary>,
        /// Detailed information about tools available to the LLM for this call.
        /// Empty if no tools were provided (simple chat mode).
        available_tools: Vec<ToolDetail>,
        /// The text output from the model.
        output_content: Option<String>,
        /// Reasoning/thinking content (if any).
        reasoning_content: Option<String>,
        /// Tool calls requested by the model.
        tool_calls: Vec<ToolCallSummary>,
        /// Token usage for this specific call.
        usage: Option<TokenUsage>,
    },
    /// A tool execution.
    ToolCall {
        /// Tool name.
        tool_name: String,
        /// Source of the tool ("built-in", "mcp", "subagent:xxx").
        source: String,
        /// Arguments passed to the tool.
        arguments: serde_json::Value,
        /// Result content (truncated for large outputs).
        result: Option<String>,
        /// Whether the tool call succeeded.
        success: bool,
    },
    /// A subagent execution (contains nested LLM + tool spans).
    SubagentCall {
        /// Subagent name.
        agent_name: String,
        /// Task description sent to the subagent.
        task: String,
        /// Model used by the subagent (if different from parent).
        model: Option<String>,
        /// Final result from the subagent.
        result: Option<String>,
        /// Accumulated token usage for the subagent.
        usage: Option<TokenUsage>,
        /// Number of tool-calling rounds the subagent executed.
        tool_rounds: usize,
    },
}

/// Detailed information about a tool available to the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDetail {
    /// Tool name.
    pub name: String,
    /// Tool description.
    pub description: String,
    /// JSON schema of tool parameters.
    pub parameters_schema: serde_json::Value,
}

/// Summary of a message in the LLM input (for tracing, not full content).
#[derive(Debug, Clone)]
pub struct MessageSummary {
    /// Message role (system, user, assistant, tool).
    pub role: String,
    /// Truncated content (first N characters).
    pub content_preview: String,
    /// Full content length in characters.
    pub content_len: usize,
}

/// Summary of a tool call in an LLM response.
#[derive(Debug, Clone)]
pub struct ToolCallSummary {
    /// Tool/function name.
    pub function_name: String,
    /// Arguments (may be truncated).
    pub arguments_preview: String,
}

/// Status of a span.
#[derive(Debug, Clone)]
pub enum SpanStatus {
    /// Span is currently executing.
    Running,
    /// Span completed successfully.
    Ok,
    /// Span completed with an error.
    Error(String),
}

/// Arbitrary span attribute value.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum SpanValue {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl Span {
    /// Create a new span with the given parameters.
    pub fn new(
        span_id: String,
        parent_span_id: Option<String>,
        trace_id: String,
        name: String,
        span_type: SpanType,
    ) -> Self {
        Self {
            span_id,
            parent_span_id,
            trace_id,
            name,
            span_type,
            started_at: Utc::now(),
            ended_at: None,
            status: SpanStatus::Running,
            attributes: HashMap::new(),
            elapsed_ms: None,
        }
    }

    /// Mark the span as completed successfully.
    #[allow(dead_code)]
    pub fn finish_ok(&mut self) {
        let ended = Utc::now();
        self.elapsed_ms = Some(
            (ended - self.started_at).num_milliseconds().max(0) as u64,
        );
        self.ended_at = Some(ended);
        self.status = SpanStatus::Ok;
    }

    /// Mark the span as completed with an error.
    #[allow(dead_code)]
    pub fn finish_error(&mut self, error: String) {
        let ended = Utc::now();
        self.elapsed_ms = Some(
            (ended - self.started_at).num_milliseconds().max(0) as u64,
        );
        self.ended_at = Some(ended);
        self.status = SpanStatus::Error(error);
    }
}
