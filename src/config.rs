use anyhow::Result;

use crate::llm::LlmConfig;

// ── Shared constants ──

/// The built-in default system prompt.
///
/// This constant is the single source of truth for the default prompt,
/// used by `AgentConfig::from_env()` to detect custom overrides.
pub const DEFAULT_SYSTEM_PROMPT: &str =
    "You are Daedalus, a helpful AI assistant. \
     Be concise and accurate in your responses.";

// ── Agent configuration ──

/// Agent configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// LLM provider configuration (api_key, model, api_base, adapter_kind).
    pub llm: LlmConfig,
    /// System prompt for the agent (legacy, used as fallback when prompt builder is bypassed).
    pub system_prompt: String,
    /// Whether the user explicitly set a custom system prompt via `DAEDALUS_SYSTEM_PROMPT`.
    ///
    /// When `true`, the custom prompt takes priority over the PromptBuilder.
    pub is_custom_prompt: bool,
    /// Custom agent name (defaults to "Daedalus").
    pub agent_name: Option<String>,
    /// Loaded soul content (read from SOUL.md file at startup).
    pub soul: Option<String>,
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
    /// - `DAEDALUS_SYSTEM_PROMPT`: Custom system prompt (legacy fallback)
    /// - `DAEDALUS_AGENT_NAME`: Custom agent name (default: "Daedalus")
    /// - `DAEDALUS_SOUL_FILE`: Path to SOUL.md personality file
    /// - `DAEDALUS_THINKING_ENABLED`: Enable thinking mode ("true"/"false")
    /// - `DAEDALUS_THINKING_TOKENS`: Max tokens for thinking (e.g., "2048")
    /// - `DAEDALUS_REASONING_EFFORT`: Reasoning effort level ("low"/"medium"/"high")
    pub fn from_env() -> Result<Self> {
        let llm = LlmConfig::from_env()?;

        // Detect whether the user explicitly set a custom system prompt
        let (system_prompt, is_custom_prompt) = match std::env::var("DAEDALUS_SYSTEM_PROMPT") {
            Ok(custom) if custom != DEFAULT_SYSTEM_PROMPT => (custom, true),
            _ => (DEFAULT_SYSTEM_PROMPT.to_string(), false),
        };

        let agent_name = std::env::var("DAEDALUS_AGENT_NAME").ok();

        let soul = Self::load_soul_file();

        Ok(Self {
            llm,
            system_prompt,
            is_custom_prompt,
            agent_name,
            soul,
        })
    }

    /// Load soul content from SOUL.md file if configured via `DAEDALUS_SOUL_FILE`.
    fn load_soul_file() -> Option<String> {
        std::env::var("DAEDALUS_SOUL_FILE").ok().and_then(|path| {
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    let trimmed = content.trim().to_string();
                    if trimmed.is_empty() {
                        None
                    } else {
                        tracing::info!(path = %path, "Loaded SOUL personality file");
                        Some(trimmed)
                    }
                }
                Err(e) => {
                    tracing::warn!(path = %path, error = %e, "Failed to load SOUL file, skipping");
                    None
                }
            }
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
