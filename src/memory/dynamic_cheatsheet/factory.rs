use std::path::PathBuf;

use crate::memory::{Memory, MemoryFactory};
use crate::memory::persistence::MemoryPersistence;

use super::cheatsheet::DynamicCheatsheet;
use super::memory::CheatsheetMemory;

/// Factory for creating `CheatsheetMemory` instances.
///
/// When workspace paths are configured, the factory will automatically
/// load a persisted cheatsheet from disk.
pub struct CheatsheetFactory {
    /// Path to the DynamicCheatsheet persistence file (from workspace).
    cheatsheet_path: Option<PathBuf>,
}

impl CheatsheetFactory {
    /// Create a factory without workspace persistence (in-memory only).
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            cheatsheet_path: None,
        }
    }

    /// Create a factory with workspace persistence path.
    pub fn with_workspace(cheatsheet_path: PathBuf) -> Self {
        Self {
            cheatsheet_path: Some(cheatsheet_path),
        }
    }
}

impl MemoryFactory for CheatsheetFactory {
    fn create_memory(&self, system_prompt: &str) -> Box<dyn Memory> {
        let cheatsheet = match &self.cheatsheet_path {
            Some(path) => match DynamicCheatsheet::load(path) {
                Ok(cs) if !cs.is_empty() => cs,
                Ok(_) => DynamicCheatsheet::with_defaults(),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Failed to load cheatsheet from workspace, starting fresh"
                    );
                    DynamicCheatsheet::with_defaults()
                }
            },
            None => DynamicCheatsheet::with_defaults(),
        };

        Box::new(CheatsheetMemory::with_cheatsheet(system_prompt, cheatsheet))
    }

    fn strategy_name(&self) -> &str {
        "dynamic_cheatsheet"
    }
}
