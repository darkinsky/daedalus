use anyhow::Result;

use crate::llm::LlmConfig;
use crate::workspace::Workspace;

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
    #[allow(dead_code)]
    pub fn from_env() -> Result<Self> {
        Self::build(Self::load_soul_file())
    }

    /// Load configuration from environment variables with workspace support.
    ///
    /// Same as `from_env()` but uses the workspace for SOUL file fallback.
    pub fn from_env_with_workspace(workspace: &Workspace) -> Result<Self> {
        Self::build(Self::load_soul_file_with_workspace(workspace))
    }

    /// Shared configuration builder — loads all env vars and assembles the config.
    ///
    /// The `soul` parameter is the only part that differs between
    /// `from_env()` and `from_env_with_workspace()`.
    fn build(soul: Option<String>) -> Result<Self> {
        let llm = LlmConfig::from_env()?;

        // Detect whether the user explicitly set a custom system prompt
        let (system_prompt, is_custom_prompt) = match std::env::var("DAEDALUS_SYSTEM_PROMPT") {
            Ok(custom) if custom != DEFAULT_SYSTEM_PROMPT => (custom, true),
            _ => (DEFAULT_SYSTEM_PROMPT.to_string(), false),
        };

        let agent_name = std::env::var("DAEDALUS_AGENT_NAME").ok();

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
        let path = std::env::var("DAEDALUS_SOUL_FILE").ok()?;
        Self::read_trimmed_file(&path)
            .map(|content| {
                tracing::info!(path = %path, "Loaded SOUL personality file");
                content
            })
            .or_else(|| {
                // Only warn if the file was explicitly configured but unreadable.
                // read_trimmed_file returns None for both missing and empty files.
                if std::path::Path::new(&path).exists() {
                    tracing::warn!(path = %path, "SOUL file is empty, skipping");
                } else {
                    tracing::warn!(path = %path, "Failed to load SOUL file, skipping");
                }
                None
            })
    }

    /// Load soul content with workspace fallback.
    ///
    /// Priority: `DAEDALUS_SOUL_FILE` env var > workspace `config/soul.md`
    fn load_soul_file_with_workspace(workspace: &Workspace) -> Option<String> {
        // 1. Try env var first (backward compatible)
        if let Some(soul) = Self::load_soul_file() {
            return Some(soul);
        }

        // 2. Try workspace soul file
        if workspace.has_soul_file() {
            let path = workspace.soul_file_path();
            let content = Self::read_trimmed_file(&path.to_string_lossy())?;
            tracing::info!(path = %path.display(), "Loaded SOUL file from workspace");
            return Some(content);
        }

        None
    }

    /// Read a file and return its trimmed content, or `None` if the file
    /// doesn't exist, can't be read, or is empty after trimming.
    fn read_trimmed_file(path: &str) -> Option<String> {
        let content = std::fs::read_to_string(path).ok()?;
        let trimmed = content.trim().to_string();
        if trimmed.is_empty() { None } else { Some(trimmed) }
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
