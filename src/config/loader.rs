//! Unified YAML configuration loader.
//!
//! Reads `config/daedalus.yaml` **once** and returns all configuration
//! sections. This eliminates the previous pattern where `AgentConfig` and
//! `LogConfig` each independently read and parsed the same file.
//!
//! ## Two-phase loading
//!
//! Because `LogConfig` is needed to initialize tracing, but `AgentConfig`
//! construction uses tracing (e.g., soul file loading logs), the loader
//! uses a two-phase approach:
//!
//! 1. `load_from_workspace()` — reads YAML once, returns `RawConfig` + `LogConfig`
//! 2. `RawConfig::into_agent_config()` — called after tracing is initialized

use anyhow::{Context, Result};

use crate::llm::LlmConfig;
use crate::workspace::Workspace;

use super::agent_config::{AgentConfig, AgentSection, EmbeddingConfig, MemorySection};
use super::logging::LogConfig;
use crate::agent_tracing::TracingConfig;
use crate::middleware::config::MiddlewareConfig;
use crate::acp::tool::AcpConfig;
use crate::tools::ToolsConfig;
use crate::middleware::builtin::permission_rules::PermissionsConfig;
use crate::hooks::config::HooksConfig;

/// Top-level YAML configuration file structure.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
struct DaedalusConfigFile {
    /// LLM provider configuration.
    llm: LlmConfig,
    /// Agent-level configuration.
    agent: AgentSection,
    /// Memory strategy configuration.
    memory: MemorySection,
    /// Embedding provider configuration (separate from memory).
    embedding: EmbeddingConfig,
    /// Logging configuration.
    logging: LogConfig,
    /// Tracing/observability configuration.
    tracing: TracingConfig,
    /// Middleware pipeline configuration.
    middleware: MiddlewareConfig,
    /// ACP (Agent Communication Protocol) configuration.
    acp: AcpConfig,
    /// Unified tool configuration section.
    tools: ToolsConfig,
    /// Permission system configuration.
    permissions: PermissionsConfig,
    /// Hooks configuration.
    hooks: HooksConfig,
    /// Legacy: top-level web_search key (deprecated, use `tools.web_search` instead).
    /// Kept for backward compatibility — merged into `tools.web_search` if present.
    web_search: Option<crate::tools::web_search::WebSearchConfig>,
}

/// Intermediate configuration state between YAML parsing and AgentConfig construction.
///
/// Holds the raw deserialized sections that haven't been fully processed yet
/// (e.g., soul file hasn't been loaded). This allows the caller to initialize
/// tracing first, then complete the AgentConfig build with full logging support.
pub struct RawConfig {
    llm: LlmConfig,
    agent: AgentSection,
    memory: MemorySection,
    embedding: EmbeddingConfig,
    /// Tracing configuration (exposed for bootstrap to initialize TracingManager).
    pub tracing: TracingConfig,
    /// Middleware pipeline configuration.
    pub middleware: MiddlewareConfig,
    /// ACP (Agent Communication Protocol) configuration.
    pub acp: AcpConfig,
    /// Unified tool configuration.
    pub tools: ToolsConfig,
    /// Permission system configuration.
    pub permissions: PermissionsConfig,
    /// Hooks configuration.
    pub hooks: HooksConfig,
}

impl RawConfig {
    /// Complete the AgentConfig construction.
    ///
    /// This should be called **after** tracing is initialized, because
    /// soul file loading emits tracing events.
    pub fn into_agent_config(self, workspace: &Workspace) -> AgentConfig {
        AgentConfig::build(self.llm, self.agent, self.memory, self.embedding, Some(workspace))
    }

    /// Override the model identifier (used by CLI `--model` flag).
    pub fn set_model(&mut self, model: String) {
        self.llm.model = model;
    }
}

/// Load all configuration from the workspace YAML config file.
///
/// Reads `config/daedalus.yaml` **once** and returns:
/// - `RawConfig` — intermediate state, call `.into_agent_config()` after tracing init
/// - `LogConfig` — ready to use immediately for tracing initialization
///
/// If the file doesn't exist, returns defaults for both.
///
/// **Note**: This function is called *before* tracing is initialized
/// (because `LogConfig` is needed to set up tracing). Therefore it must
/// not use `tracing::*` macros — those calls would be silently dropped.
pub fn load_from_workspace(workspace: &Workspace) -> Result<(RawConfig, LogConfig)> {
    let file_config = if workspace.has_config_file() {
        let path = workspace.config_file_path();
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;
        let config: DaedalusConfigFile = serde_yaml::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {}", path.display()))?;
        config
    } else {
        DaedalusConfigFile::default()
    };

    let raw_config = RawConfig {
        llm: file_config.llm,
        agent: file_config.agent,
        memory: file_config.memory,
        embedding: file_config.embedding,
        tracing: file_config.tracing,
        middleware: file_config.middleware,
        acp: file_config.acp,
        tools: {
            let mut tools = file_config.tools;
            // Backward compatibility: if the legacy top-level `web_search` key
            // is present and the new `tools.web_search` was not explicitly set
            // (still at default), use the legacy value. This allows users who
            // haven't migrated to the new `tools:` section to keep working.
            if let Some(legacy_ws) = file_config.web_search.filter(|_| {
                tools.web_search == crate::tools::web_search::WebSearchConfig::default()
            }) {
                tools.web_search = legacy_ws;
            }
            tools
        },
        permissions: file_config.permissions,
        hooks: file_config.hooks,
    };

    let mut log_config = file_config.logging;
    // Only set default log_dir if the user did NOT explicitly configure
    // the logging section at all (i.e., it's the complete default).
    // If the user wrote `logging:` with `log_dir: ~` (null), they want
    // stderr-only mode — we respect that.
    if log_config.log_dir.is_none() && !workspace.has_config_file() {
        // No config file at all → use workspace logs directory
        log_config.log_dir = Some(
            workspace.logs_dir().to_string_lossy().into_owned()
        );
    }
    // When a config file exists but log_dir is None, the user either:
    // - didn't write a logging section (serde default = None → stderr only)
    // - explicitly wrote log_dir: ~ (null → stderr only)
    // Both cases mean "stderr only" — we don't override.

    Ok((raw_config, log_config))
}
