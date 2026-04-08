use anyhow::{Context, Result};

// ── LLM provider configuration types ──

/// Reasoning effort level for models that support it.
///
/// Maps to OpenAI's `reasoning_effort` and Venus proxy's `thinking_level`/`reasoning_effort`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
}

impl std::fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReasoningEffort::Low => write!(f, "low"),
            ReasoningEffort::Medium => write!(f, "medium"),
            ReasoningEffort::High => write!(f, "high"),
        }
    }
}

impl std::str::FromStr for ReasoningEffort {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "low" => Ok(ReasoningEffort::Low),
            "medium" => Ok(ReasoningEffort::Medium),
            "high" => Ok(ReasoningEffort::High),
            _ => Err(format!("Invalid reasoning effort: '{}'. Expected: low, medium, high", s)),
        }
    }
}

/// Venus API proxy advanced parameters.
///
/// Shared between `LlmConfig` (instance-level defaults) and `ChatOptions`
/// (per-request overrides). This eliminates field duplication and provides
/// a single merge operation for the "request overrides config" pattern.
#[derive(Debug, Clone, Default)]
pub struct VenusExtensions {
    /// Enable thinking/reasoning mode for supported models.
    /// Maps to Venus `thinking_enabled` (Claude, Gemini, VenusLLMServing).
    pub thinking_enabled: Option<bool>,
    /// Maximum tokens for the thinking/reasoning process.
    /// Maps to Venus `thinking_tokens` (Claude, Gemini).
    /// Must be > 1024 and <= max_tokens.
    pub thinking_tokens: Option<u32>,
    /// Reasoning effort level.
    /// Maps to OpenAI `reasoning_effort` (o-series) and
    /// Gemini `thinking_level`/`reasoning_effort` (gemini-3 series).
    pub reasoning_effort: Option<ReasoningEffort>,
}

impl VenusExtensions {
    /// Return true if any Venus extension parameter is configured.
    ///
    /// Useful for provider selection logic and diagnostics.
    #[allow(dead_code)]
    pub fn is_active(&self) -> bool {
        self.thinking_enabled.is_some()
            || self.thinking_tokens.is_some()
            || self.reasoning_effort.is_some()
    }

    /// Merge with another `VenusExtensions`, using `other` as the
    /// higher-priority source (request-level overrides config-level).
    ///
    /// For each field, if `other` has a value it wins; otherwise `self`
    /// (the config default) is used.
    pub fn merge_with_overrides(&self, overrides: &VenusExtensions) -> VenusExtensions {
        VenusExtensions {
            thinking_enabled: overrides.thinking_enabled.or(self.thinking_enabled),
            thinking_tokens: overrides.thinking_tokens.or(self.thinking_tokens),
            reasoning_effort: overrides.reasoning_effort.clone().or(self.reasoning_effort.clone()),
        }
    }
}

/// Configuration for an LLM provider.
#[derive(Debug, Clone)]
pub struct LlmConfig {
    /// API key for authentication.
    pub api_key: String,
    /// Model identifier (e.g., "gpt-4o", "claude-sonnet-4-6").
    pub model: String,
    /// Optional custom API base URL.
    pub api_base: Option<String>,
    /// Adapter kind hint (e.g., "openai", "anthropic", "gemini").
    /// Defaults to "openai" if not specified.
    pub adapter_kind: Option<String>,
    /// Venus API proxy advanced options (thinking, reasoning_effort).
    pub venus: VenusExtensions,
}

// ── Agent configuration ──

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
                venus: VenusExtensions {
                    thinking_enabled,
                    thinking_tokens,
                    reasoning_effort,
                },
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
