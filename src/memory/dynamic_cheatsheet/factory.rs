use std::path::PathBuf;

use crate::memory::{Memory, MemoryFactory};
use crate::memory::persistence::MemoryPersistence;

use super::cheatsheet::DynamicCheatsheet;
use super::config::CheatsheetConfig;
use super::memory::CheatsheetMemory;

/// Factory for creating `CheatsheetMemory` instances.
///
/// When workspace paths are configured, the factory will automatically
/// load a persisted cheatsheet from disk. The `CheatsheetConfig` from
/// YAML is passed through to the `DynamicCheatsheet` engine.
pub struct CheatsheetFactory {
    /// Path to the DynamicCheatsheet persistence file (from workspace).
    cheatsheet_path: Option<PathBuf>,
    /// Configuration from YAML (curator_mode, max_entries, etc.).
    config: CheatsheetConfig,
}

impl CheatsheetFactory {
    /// Create a factory without workspace persistence (in-memory only).
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            cheatsheet_path: None,
            config: CheatsheetConfig::default(),
        }
    }

    /// Create a factory with workspace persistence path and YAML config.
    pub fn with_workspace(cheatsheet_path: PathBuf, config: CheatsheetConfig) -> Self {
        Self {
            cheatsheet_path: Some(cheatsheet_path),
            config,
        }
    }
}

impl MemoryFactory for CheatsheetFactory {
    fn create_memory(&self, system_prompt: &str) -> Box<dyn Memory> {
        let mut cheatsheet = match &self.cheatsheet_path {
            Some(path) => match DynamicCheatsheet::load(path) {
                Ok(cs) if !cs.is_empty() => cs,
                Ok(_) => DynamicCheatsheet::new(self.config.clone()),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Failed to load cheatsheet from workspace, starting fresh"
                    );
                    DynamicCheatsheet::new(self.config.clone())
                }
            },
            None => DynamicCheatsheet::new(self.config.clone()),
        };

        // Apply YAML config to loaded cheatsheet (persistence doesn't store config).
        cheatsheet.set_config(self.config.clone());

        Box::new(CheatsheetMemory::with_cheatsheet(system_prompt, cheatsheet))
    }

    fn strategy_name(&self) -> &str {
        "dynamic_cheatsheet"
    }
}
