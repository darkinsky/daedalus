use anyhow::{Context, Result};

use crate::llm::LlmConfig;
use crate::workspace::Workspace;

// ── Shared constants ──

/// The built-in default system prompt.
///
/// This constant is the single source of truth for the default prompt,
/// used to detect custom overrides.
pub const DEFAULT_SYSTEM_PROMPT: &str =
    "You are Daedalus, a helpful AI assistant. \
     Be concise and accurate in your responses.";

// ── YAML section structure ──

/// Agent section in the YAML config file.
///
/// This type is `pub(super)` so the unified loader can deserialize it
/// and pass it to `AgentConfig::build()`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub(super) struct AgentSection {
    /// Custom agent name (defaults to "Daedalus").
    name: Option<String>,
    /// Custom system prompt (overrides PromptBuilder when set).
    system_prompt: Option<String>,
    /// Path to SOUL.md personality file.
    soul_file: Option<String>,
}

// ── Agent configuration ──

/// Agent configuration loaded from a YAML config file.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// LLM provider configuration (api_key, model, api_base, adapter_kind).
    pub llm: LlmConfig,
    /// System prompt for the agent (legacy, used as fallback when prompt builder is bypassed).
    pub system_prompt: String,
    /// Whether the user explicitly set a custom system prompt.
    ///
    /// When `true`, the custom prompt takes priority over the PromptBuilder.
    pub is_custom_prompt: bool,
    /// Custom agent name (defaults to "Daedalus").
    pub agent_name: Option<String>,
    /// Loaded soul content (read from SOUL.md file at startup).
    pub soul: Option<String>,
}

impl AgentConfig {
    /// Build AgentConfig from pre-parsed YAML sections.
    ///
    /// Called by the unified loader (`config::loader`) after parsing the
    /// YAML file once. This method handles soul file resolution and
    /// custom prompt detection.
    pub(super) fn build(
        llm: LlmConfig,
        agent: AgentSection,
        workspace: Option<&Workspace>,
    ) -> Self {
        // Detect whether the user explicitly set a custom system prompt
        let (system_prompt, is_custom_prompt) = match &agent.system_prompt {
            Some(custom) if !custom.is_empty() && custom != DEFAULT_SYSTEM_PROMPT => {
                (custom.clone(), true)
            }
            _ => (DEFAULT_SYSTEM_PROMPT.to_string(), false),
        };

        let agent_name = agent.name;

        // Load soul content
        let soul = Self::load_soul(agent.soul_file.as_deref(), workspace);

        Self {
            llm,
            system_prompt,
            is_custom_prompt,
            agent_name,
            soul,
        }
    }

    /// Load configuration from a specific YAML file path (standalone, for testing).
    #[allow(dead_code)]
    pub fn from_file(path: &str) -> Result<Self> {
        /// Standalone YAML structure for `from_file()`.
        #[derive(Debug, Clone, Default, serde::Deserialize)]
        #[serde(default)]
        struct StandaloneConfig {
            llm: LlmConfig,
            agent: AgentSection,
        }

        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path))?;
        let file_config: StandaloneConfig = serde_yaml::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {}", path))?;
        Ok(Self::build(file_config.llm, file_config.agent, None))
    }

    /// Load soul content from the configured path or workspace fallback.
    ///
    /// Priority:
    /// 1. Explicit `soul_file` path from config
    /// 2. Workspace `config/soul.md` (if workspace is provided)
    fn load_soul(
        soul_file: Option<&str>,
        workspace: Option<&Workspace>,
    ) -> Option<String> {
        // 1. Try explicit soul_file path
        if let Some(path) = soul_file {
            if let Some(content) = Self::read_trimmed_file(path) {
                tracing::info!(path = %path, "Loaded SOUL personality file");
                return Some(content);
            }
            if std::path::Path::new(path).exists() {
                tracing::warn!(path = %path, "SOUL file is empty, skipping");
            } else {
                tracing::warn!(path = %path, "Failed to load SOUL file, skipping");
            }
        }

        // 2. Try workspace soul file
        if let Some(ws) = workspace {
            if ws.has_soul_file() {
                let path = ws.soul_file_path();
                if let Some(content) = Self::read_trimmed_file(&path.to_string_lossy()) {
                    tracing::info!(path = %path.display(), "Loaded SOUL file from workspace");
                    return Some(content);
                }
            }
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
