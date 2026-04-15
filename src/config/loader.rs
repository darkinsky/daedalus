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

use super::agent_config::{AgentConfig, AgentSection};
use super::logging::LogConfig;

/// Top-level YAML configuration file structure.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
struct DaedalusConfigFile {
    /// LLM provider configuration.
    llm: LlmConfig,
    /// Agent-level configuration.
    agent: AgentSection,
    /// Logging configuration.
    logging: LogConfig,
}

/// Intermediate configuration state between YAML parsing and AgentConfig construction.
///
/// Holds the raw deserialized sections that haven't been fully processed yet
/// (e.g., soul file hasn't been loaded). This allows the caller to initialize
/// tracing first, then complete the AgentConfig build with full logging support.
pub struct RawConfig {
    llm: LlmConfig,
    agent: AgentSection,
}

impl RawConfig {
    /// Complete the AgentConfig construction.
    ///
    /// This should be called **after** tracing is initialized, because
    /// soul file loading emits tracing events.
    pub fn into_agent_config(self, workspace: &Workspace) -> AgentConfig {
        AgentConfig::build(self.llm, self.agent, Some(workspace))
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
    };

    let mut log_config = file_config.logging;
    // If no explicit log dir is set, use workspace logs directory
    if log_config.log_dir.is_none() {
        log_config.log_dir = Some(
            workspace.logs_dir().to_string_lossy().into_owned()
        );
    }

    Ok((raw_config, log_config))
}
