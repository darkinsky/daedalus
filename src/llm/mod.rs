mod types;
mod adapter;
mod provider;
pub mod model_registry;
pub mod cache_monitor;

// Explicit re-exports instead of glob (`pub use types::*`).
// This makes it clear which symbols come from the llm module and
// prevents accidental namespace pollution as the type set grows.
pub use types::{
    CacheControl, ChatMessage, ChatOptions, ChatRole, ChatResponse,
    ContentPart, ImageSource,
    LlmConfig, VenusExtensions,
    TokenUsage, ToolCall, ToolResponse, ToolRound,
    StreamChunk, StreamAccumulator,
    format_messages_for_log,
};

// Re-export ReasoningEffort for use in config deserialization.
#[allow(unused_imports)]
pub use types::ReasoningEffort;

// NOTE: ToolInfo has been moved to `crate::tools::ToolInfo`.
// It is no longer re-exported from `crate::llm` to avoid stale aliases.
// All code should use `crate::tools::ToolInfo` directly.

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc;

/// The core LLM API trait — provider-agnostic interface for chat completion.
///
/// All provider-specific details (OpenAI, Anthropic, Gemini, etc.) are hidden
/// behind this trait. The trait uses only our own types (`ChatMessage`, `ToolCall`,
/// `ToolResponse`, `ChatResponse`), never leaking provider internals.
#[async_trait]
pub trait LlmApi: Send + Sync {
    /// Send a chat completion request and return the response.
    ///
    /// Default implementation delegates to `chat_with_tools` with empty tools
    /// and history. Override only if the provider needs a separate code path.
    async fn chat(
        &self,
        messages: &[ChatMessage],
        options: Option<&ChatOptions>,
    ) -> Result<ChatResponse> {
        self.chat_with_tools(messages, &[], &[], options).await
    }

    /// Send a chat completion request with tool definitions and prior tool context.
    ///
    /// # Arguments
    /// * `messages` - The conversation history (system + user + assistant messages).
    /// * `tools` - Tool definitions (as JSON, in OpenAI function-calling format).
    /// * `tool_history` - Prior tool calls and responses from earlier rounds in
    ///   the current tool-calling loop. The provider converts these into the
    ///   appropriate wire format.
    /// * `options` - Optional generation parameters.
    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        tool_history: &[ToolRound],
        options: Option<&ChatOptions>,
    ) -> Result<ChatResponse>;

    /// Send a streaming chat completion request with tool definitions.
    ///
    /// Returns a receiver that yields `StreamChunk`s as they arrive from the
    /// LLM. The stream ends with a `StreamChunk::Done` sentinel.
    ///
    /// The default implementation falls back to the non-streaming
    /// `chat_with_tools`, emitting the full response as a single chunk.
    /// Providers that support native streaming should override this.
    async fn chat_with_tools_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        tool_history: &[ToolRound],
        options: Option<&ChatOptions>,
    ) -> Result<mpsc::Receiver<StreamChunk>> {
        // Fallback: call non-streaming and emit as single chunks
        let response = self.chat_with_tools(messages, tools, tool_history, options).await?;
        let (tx, rx) = mpsc::channel(16);
        tokio::spawn(async move {
            if let Some(reasoning) = response.reasoning_content {
                let _ = tx.send(StreamChunk::ReasoningDelta(reasoning)).await;
            }
            if !response.content.is_empty() {
                let _ = tx.send(StreamChunk::ContentDelta(response.content)).await;
            }
            for tc in response.tool_calls {
                let _ = tx.send(StreamChunk::ToolCall(tc)).await;
            }
            if let Some(usage) = response.usage {
                let _ = tx.send(StreamChunk::Usage(usage)).await;
            }
            let _ = tx.send(StreamChunk::Done).await;
        });
        Ok(rx)
    }

    /// Return true if this provider supports tool/function calling.
    fn supports_tools(&self) -> bool {
        false
    }

    /// Return the model identifier this provider is configured to use.
    fn model_name(&self) -> &str;

    /// Return a human-readable name for this provider (e.g., "OpenAI", "Anthropic").
    fn provider_name(&self) -> &str;
}

/// Factory function: create an LLM provider from configuration.
///
/// Creates a unified `LlmProvider` that uses the appropriate adapter
/// based on the `adapter_kind` configuration:
///
/// - `"openai"` (default) — OpenAI, Venus proxy, DeepSeek, compatible APIs
/// - `"anthropic"` — Anthropic Messages API (direct)
/// - `"gemini"` or `"google"` — Google Gemini API (direct)
///
/// The adapter handles all format-specific logic while the provider
/// manages HTTP transport, streaming, and error handling.
pub fn create_provider(mut config: LlmConfig) -> Result<Box<dyn LlmApi>> {
    // Resolve API key with env var fallback (avoids forcing plaintext in YAML)
    if config.api_key.is_empty() {
        config.api_key = std::env::var("DAEDALUS_API_KEY")
            .or_else(|_| std::env::var("OPENAI_API_KEY"))
            .map_err(|_| anyhow::anyhow!(
                "LLM API key not configured. Set `llm.api_key` in daedalus.yaml, \
                 or DAEDALUS_API_KEY / OPENAI_API_KEY environment variable."
            ))?;
    }

    let provider = provider::LlmProvider::new(config)?;
    Ok(Box::new(provider))
}
