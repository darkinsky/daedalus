//! API adapter layer — abstracts protocol differences between LLM providers.
//!
//! Each adapter handles the specific wire format for a given API:
//! - Request body construction (message format, tool definitions, extensions)
//! - Response parsing (content, reasoning, tool calls, usage)
//! - Authentication headers
//! - Endpoint URL construction
//! - SSE stream chunk parsing
//!
//! The unified `LlmProvider` delegates all format-specific logic to the
//! active adapter, keeping the HTTP transport layer generic.

pub mod openai;
pub mod anthropic;
pub mod gemini;
pub mod venus;

use anyhow::Result;
use reqwest::header::HeaderMap;
use serde_json::Value;

use super::{
    ChatMessage, ChatOptions, ChatResponse, LlmConfig,
    StreamChunk, ToolRound, VenusExtensions,
};

/// Trait abstracting the wire-format differences between LLM API providers.
///
/// Each adapter converts between our internal types and the provider's
/// specific JSON format. The `LlmProvider` handles HTTP transport and
/// delegates all format logic to the adapter.
pub trait ApiAdapter: Send + Sync {
    /// Build the full API endpoint URL.
    fn endpoint(&self, base_url: &str, model: &str) -> String;

    /// Build HTTP headers for authentication and content type.
    fn headers(&self, api_key: &str) -> HeaderMap;

    /// Build the request body JSON from our internal types.
    fn build_body(
        &self,
        model: &str,
        messages: &[ChatMessage],
        tools: &[Value],
        tool_history: &[ToolRound],
        options: Option<&ChatOptions>,
        config_venus: &VenusExtensions,
    ) -> Value;

    /// Parse a non-streaming response body into our ChatResponse.
    fn parse_response(&self, body: &Value) -> Result<ChatResponse>;

    /// Parse a single SSE data line into a StreamChunk.
    ///
    /// Returns `None` if the line doesn't produce a meaningful chunk
    /// (e.g., empty deltas, metadata events).
    /// Returns `Some(StreamChunk::Done)` when the stream is complete.
    #[allow(dead_code)]
    fn parse_stream_event(&self, data: &str) -> Option<StreamChunk>;

    /// Return the terminal sentinel string that signals end of stream.
    ///
    /// For OpenAI-compatible APIs: `"[DONE]"`
    /// For Anthropic: `"event: message_stop"`
    #[allow(dead_code)]
    fn stream_done_signal(&self) -> &str {
        "[DONE]"
    }

    /// Return a human-readable name for this adapter (for logging).
    fn name(&self) -> &str;
}

/// Create an adapter based on the adapter_kind configuration.
///
/// Supported values:
/// - `"venus"` — Venus LLM Proxy (OpenAI-compatible with extensions for Claude/Gemini thinking)
/// - `"openai"` (default) — OpenAI, DeepSeek, and any plain OpenAI-compatible API
/// - `"anthropic"` — Anthropic Messages API (direct, not via Venus proxy)
/// - `"gemini"` or `"google"` — Google Gemini API (direct)
pub fn create_adapter(config: &LlmConfig) -> Box<dyn ApiAdapter> {
    match config.adapter_kind.as_deref().map(|s| s.to_lowercase()).as_deref() {
        Some("venus") => Box::new(venus::VenusAdapter),
        Some("anthropic") => Box::new(anthropic::AnthropicAdapter),
        Some("gemini") | Some("google") => Box::new(gemini::GeminiAdapter),
        // "openai", "deepseek", or anything else → OpenAI-compatible
        _ => Box::new(openai::OpenAiAdapter),
    }
}
