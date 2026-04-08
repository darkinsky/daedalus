mod types;
mod genai_provider;

pub use types::*;
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
    /// # Arguments
    /// * `messages` - The conversation history.
    /// * `options`  - Optional generation parameters (temperature, max_tokens, etc.).
    async fn chat(
        &self,
        messages: &[ChatMessage],
        options: Option<&ChatOptions>,
    ) -> Result<ChatResponse>;

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
        tool_history: &[(Vec<ToolCall>, Vec<ToolResponse>)],
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
pub fn create_provider(config: LlmConfig) -> Result<Box<dyn LlmApi>> {
    let provider = genai_provider::GenAiProvider::new(config)?;
    Ok(Box::new(provider))
}
