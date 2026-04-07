mod types;
mod genai_provider;

pub use types::*;
pub use genai_provider::GenAiProvider;

use anyhow::Result;
use async_trait::async_trait;

/// The core LLM API trait — the "base class" for all LLM providers.
///
/// Each provider (OpenAI, Anthropic, local models, etc.) implements this trait
/// to provide a unified interface for chat completion.
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

    /// Return the model identifier this provider is configured to use.
    fn model_name(&self) -> &str;

    /// Return a human-readable name for this provider (e.g., "GenAI/OpenAI").
    fn provider_name(&self) -> &str;
}

/// Factory function: create an LLM provider from configuration.
///
/// Currently only supports the GenAI provider. In the future, this can be
/// extended to select providers based on config (e.g., "openai", "anthropic", "ollama").
pub fn create_provider(config: LlmConfig) -> Result<Box<dyn LlmApi>> {
    let provider = GenAiProvider::new(config)?;
    Ok(Box::new(provider))
}
