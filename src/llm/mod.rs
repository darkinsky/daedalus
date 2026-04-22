mod types;
mod genai_provider;
mod venus_provider;

// Explicit re-exports instead of glob (`pub use types::*`).
// This makes it clear which symbols come from the llm module and
// prevents accidental namespace pollution as the type set grows.
pub use types::{
    CacheControl, ChatMessage, ChatOptions, ChatRole, ChatResponse,
    LlmConfig, ReasoningEffort, VenusExtensions,
    TokenUsage, ToolCall, ToolResponse, ToolRound,
    format_messages_for_log,
};

// NOTE: ToolInfo has been moved to `crate::tools::ToolInfo`.
// It is no longer re-exported from `crate::llm` to avoid stale aliases.
// All code should use `crate::tools::ToolInfo` directly.

use anyhow::Result;
use async_trait::async_trait;

/// The core LLM API trait — provider-agnostic interface for chat completion.
///
/// All provider-specific details (genai, OpenAI REST, etc.) are hidden behind
/// this trait. The trait uses only our own types (`ChatMessage`, `ToolCall`,
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
    ///   appropriate wire format (e.g., genai `ChatMessage::from(ToolCall)` /
    ///   `ChatMessage::from(ToolResponse)`).
    /// * `options` - Optional generation parameters.
    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        tool_history: &[ToolRound],
        options: Option<&ChatOptions>,
    ) -> Result<ChatResponse>;

    /// Return true if this provider supports tool/function calling.
    fn supports_tools(&self) -> bool {
        false
    }

    /// Return the model identifier this provider is configured to use.
    fn model_name(&self) -> &str;

    /// Return a human-readable name for this provider (e.g., "GenAI").
    fn provider_name(&self) -> &str;
}

/// Factory function: create an LLM provider from configuration.
///
/// Selects the provider implementation based on configuration:
///
/// - **VenusProvider**: Used when Venus-specific advanced parameters are
///   configured (`thinking_enabled` or `thinking_tokens`). This provider
///   sends raw HTTP requests, giving full control over the request body
///   to support Venus proxy extensions.
/// - **GenAiProvider** (default): Uses the `genai` crate's adapter system.
///   Supports standard OpenAI, Anthropic, Gemini, and compatible APIs.
///   Also handles `reasoning_effort` natively via genai.
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

    if config.venus.needs_venus_provider() {
        tracing::info!("Using VenusProvider (thinking parameters detected)");
        let provider = venus_provider::VenusProvider::new(config)?;
        Ok(Box::new(provider))
    } else {
        let provider = genai_provider::GenAiProvider::new(config)?;
        Ok(Box::new(provider))
    }
}
