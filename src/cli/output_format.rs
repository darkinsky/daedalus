//! Output format types and serialization for non-interactive (print) mode.
//!
//! Supports three output formats:
//! - `text`: Plain text to stdout (human-readable)
//! - `json`: Single JSON object after completion (script-friendly)
//! - `stream-json`: NDJSON event stream (real-time, IDE integration)

use serde::Serialize;

// ── Stream JSON events (NDJSON) ──

/// Events emitted in `stream-json` output mode.
///
/// Each event is serialized as a single JSON line (NDJSON format).
/// The `type` field is used as the discriminant tag.
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum StreamEvent {
    /// Initial system information (always the first event).
    #[serde(rename = "system")]
    System {
        message: String,
        session_id: String,
        model: String,
        provider: String,
    },

    /// Assistant text output (intermediate or final).
    #[serde(rename = "assistant")]
    Assistant {
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
    },

    /// A tool is being invoked.
    #[serde(rename = "tool_use")]
    ToolUse {
        tool: String,
        source: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        input: Option<serde_json::Value>,
    },

    /// Tool execution result.
    #[serde(rename = "tool_result")]
    ToolResult {
        tool: String,
        content: String,
        success: bool,
    },

    /// A tool-calling round has started.
    #[serde(rename = "tool_round_start")]
    ToolRoundStart {
        round: usize,
    },

    /// A tool-calling round has completed.
    #[serde(rename = "tool_round_complete")]
    ToolRoundComplete {
        tool_count: usize,
    },

    /// A subagent has started execution.
    #[serde(rename = "subagent_start")]
    SubagentStart {
        agent_name: String,
        task_preview: String,
    },

    /// A subagent has completed execution.
    #[serde(rename = "subagent_complete")]
    SubagentComplete {
        agent_name: String,
        success: bool,
        tool_rounds: usize,
        result_preview: String,
    },

    /// Final result (always the last event).
    #[serde(rename = "result")]
    Result(ResultPayload),
}

// ── JSON output (single object) ──

/// The final result payload, used in both `json` and `stream-json` modes.
#[derive(Debug, Serialize)]
pub struct ResultPayload {
    /// The assistant's final response text.
    pub result: String,
    /// Session identifier.
    pub session_id: String,
    /// Whether the result represents an error.
    pub is_error: bool,
    /// Token usage summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageSummary>,
    /// Total elapsed time in milliseconds.
    pub duration_ms: u64,
    /// Number of tool-calling rounds executed.
    pub tool_rounds: u64,
}

/// Token usage summary for JSON output.
#[derive(Debug, Serialize)]
pub struct UsageSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
}

// ── Helpers ──

/// Emit a single NDJSON line to stdout.
///
/// Each event is serialized as a compact JSON object followed by a newline.
pub fn emit_stream_event(event: &StreamEvent) {
    if let Ok(json) = serde_json::to_string(event) {
        println!("{}", json);
    }
}

/// Emit the final JSON result to stdout (for `--output-format json`).
pub fn emit_json_result(payload: &ResultPayload) {
    if let Ok(json) = serde_json::to_string_pretty(payload) {
        println!("{}", json);
    }
}
