use anyhow::{Context, Result};

/// Agent configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// OpenAI API key
    pub api_key: String,
    /// Model name (e.g., "gpt-4", "gpt-4o", "gpt-3.5-turbo")
    pub model: String,
    /// Optional API base URL for custom endpoints
    pub api_base: Option<String>,
    /// System prompt for the agent
    pub system_prompt: String,
}

impl AgentConfig {
    /// Load configuration from environment variables.
    ///
    /// Required env vars:
    /// - `OPENAI_API_KEY`: Your OpenAI API key
    ///
    /// Optional env vars:
    /// - `DAEDALUS_MODEL`: Model to use (default: "gpt-4o")
    /// - `OPENAI_BASE_URL`: Custom API base URL
    /// - `DAEDALUS_SYSTEM_PROMPT`: Custom system prompt
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .context("OPENAI_API_KEY environment variable is required")?;

        let model = std::env::var("DAEDALUS_MODEL").unwrap_or_else(|_| "gpt-4o".to_string());

        let api_base = std::env::var("OPENAI_BASE_URL").ok();

        let system_prompt = std::env::var("DAEDALUS_SYSTEM_PROMPT").unwrap_or_else(|_| {
            "You are Daedalus, a helpful AI assistant. \
             You can use tools to help answer questions and complete tasks. \
             Be concise and accurate in your responses."
                .to_string()
        });

        Ok(Self {
            api_key,
            model,
            api_base,
            system_prompt,
        })
    }
}
