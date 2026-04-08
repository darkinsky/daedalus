use anyhow::{Context, Result};

use crate::llm::LlmConfig;

/// Agent configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// LLM provider configuration (api_key, model, api_base, adapter_kind).
    pub llm: LlmConfig,
    /// System prompt for the agent
    pub system_prompt: String,
}

impl AgentConfig {
    /// Load configuration from environment variables.
    ///
    /// Required env vars:
    /// - `OPENAI_API_KEY`: Your API key
    ///
    /// Optional env vars:
    /// - `DAEDALUS_MODEL`: Model to use (default: "gpt-4o")
    /// - `OPENAI_BASE_URL`: Custom API base URL
    /// - `DAEDALUS_ADAPTER_KIND`: LLM adapter kind ("openai", "anthropic", "gemini", "groq", "cohere")
    /// - `DAEDALUS_SYSTEM_PROMPT`: Custom system prompt
    /// - `DAEDALUS_THINKING_ENABLED`: Enable thinking mode ("true"/"false")
    /// - `DAEDALUS_THINKING_TOKENS`: Max tokens for thinking (e.g., "2048")
    /// - `DAEDALUS_REASONING_EFFORT`: Reasoning effort level ("low"/"medium"/"high")
    pub fn from_env() -> Result<Self> {
        use crate::llm::ReasoningEffort;

        let api_key = std::env::var("OPENAI_API_KEY")
            .context("OPENAI_API_KEY environment variable is required")?;

        let model = std::env::var("DAEDALUS_MODEL").unwrap_or_else(|_| "gpt-4o".to_string());

        let api_base = std::env::var("OPENAI_BASE_URL").ok();

        let adapter_kind = std::env::var("DAEDALUS_ADAPTER_KIND").ok();

        let thinking_enabled = std::env::var("DAEDALUS_THINKING_ENABLED")
            .ok()
            .map(|v| v.to_lowercase() == "true");

        let thinking_tokens = std::env::var("DAEDALUS_THINKING_TOKENS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok());

        let reasoning_effort = std::env::var("DAEDALUS_REASONING_EFFORT")
            .ok()
            .and_then(|v| v.parse::<ReasoningEffort>().ok());

        let system_prompt = std::env::var("DAEDALUS_SYSTEM_PROMPT").unwrap_or_else(|_| {
            "You are Daedalus, a helpful AI assistant. \
             Be concise and accurate in your responses."
                .to_string()
        });

        Ok(Self {
            llm: LlmConfig {
                api_key,
                model,
                api_base,
                adapter_kind,
                thinking_enabled,
                thinking_tokens,
                reasoning_effort,
            },
            system_prompt,
        })
    }

    /// Convenience accessor for the model name.
    pub fn model(&self) -> &str {
        &self.llm.model
    }

    /// Convenience accessor for the API base URL.
    pub fn api_base(&self) -> Option<&str> {
        self.llm.api_base.as_deref()
    }
}
