use anyhow::Result;

use crate::config::AgentConfig;
use crate::llm::{ChatMessage, LlmApi};

/// The Daedalus agent — orchestrates conversation with an LLM provider.
pub struct Agent {
    /// The LLM provider (trait object, provider-agnostic).
    llm: Box<dyn LlmApi>,
    /// Conversation history.
    messages: Vec<ChatMessage>,
}

impl Agent {
    /// Create a new agent with the given LLM provider and configuration.
    pub fn new(llm: Box<dyn LlmApi>, config: &AgentConfig) -> Self {
        let mut messages = Vec::new();
        // Prepend system message
        messages.push(ChatMessage::system(&config.system_prompt));

        tracing::info!(
            "Agent initialized with provider: {}, model: {}",
            llm.provider_name(),
            llm.model_name()
        );

        Self {
            llm,
            messages,
        }
    }

    /// Send a user message and get the assistant's response.
    pub async fn chat(&mut self, user_input: &str) -> Result<String> {
        // Append user message to history
        self.messages.push(ChatMessage::user(user_input));

        // Log the user input
        tracing::info!(
            provider = self.llm.provider_name(),
            model = self.llm.model_name(),
            role = "user",
            message = user_input,
            history_len = self.messages.len(),
            "LLM request: user input"
        );

        // Call the LLM
        let response = self.llm.chat(&self.messages, None).await?;

        // Log the assistant output
        tracing::info!(
            provider = self.llm.provider_name(),
            model = self.llm.model_name(),
            role = "assistant",
            message = response.content.as_str(),
            content_len = response.content.len(),
            prompt_tokens = response.usage.as_ref().and_then(|u| u.prompt_tokens),
            completion_tokens = response.usage.as_ref().and_then(|u| u.completion_tokens),
            total_tokens = response.usage.as_ref().and_then(|u| u.total_tokens),
            "LLM response: assistant output"
        );

        // Append assistant response to history
        self.messages.push(ChatMessage::assistant(&response.content));

        Ok(response.content)
    }

    /// Return the provider name (delegated to the LLM provider).
    pub fn provider_name(&self) -> &str {
        self.llm.provider_name()
    }

    /// Return the model name (delegated to the LLM provider).
    pub fn model_name(&self) -> &str {
        self.llm.model_name()
    }
}
