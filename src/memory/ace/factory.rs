use std::path::PathBuf;

use crate::memory::{Memory, MemoryFactory};
use crate::memory::persistence::MemoryPersistence;

use super::config::AceConfig;
use super::memory::AceMemory;
use super::playbook::Playbook;

/// Factory for creating `AceMemory` instances.
///
/// When workspace paths are configured, the factory will automatically
/// load a persisted playbook from disk. The `AceConfig` from YAML is
/// passed through to the `AceMemory` engine.
pub struct AceFactory {
    /// Path to the Playbook persistence file (from workspace).
    playbook_path: Option<PathBuf>,
    /// Configuration from YAML (max_sections, max_token_budget, etc.).
    config: AceConfig,
}

impl AceFactory {
    /// Create a factory without workspace persistence (in-memory only).
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            playbook_path: None,
            config: AceConfig::default(),
        }
    }

    /// Create a factory with workspace persistence path and YAML config.
    pub fn with_workspace(playbook_path: PathBuf, config: AceConfig) -> Self {
        Self {
            playbook_path: Some(playbook_path),
            config,
        }
    }
}

impl MemoryFactory for AceFactory {
    fn create_memory(&self, system_prompt: &str) -> Box<dyn Memory> {
        let playbook = match &self.playbook_path {
            Some(path) => match Playbook::load(path) {
                Ok(pb) if !pb.is_empty() => pb,
                Ok(_) => Playbook::new(),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Failed to load playbook from workspace, starting fresh"
                    );
                    Playbook::new()
                }
            },
            None => Playbook::new(),
        };

        Box::new(AceMemory::with_playbook_and_config(system_prompt, playbook, self.config.clone()))
    }

    fn strategy_name(&self) -> &str {
        "ace"
    }
}
