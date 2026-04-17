use std::path::PathBuf;

use crate::memory::{Memory, MemoryFactory};
use crate::memory::dynamic_cheatsheet::DynamicCheatsheet;
use crate::memory::persistence::MemoryPersistence;
use super::memory::SlidingWindowMemory;

/// Factory for creating `SlidingWindowMemory` instances.
///
/// This is the default memory factory used by `ChatAgent`. It creates
/// sliding window memories with the default consolidation settings.
///
/// When workspace paths are configured, the factory will automatically
/// load persisted LongTermMemory and HistoryLog from disk.
pub struct SlidingWindowFactory {
    /// Path to the LongTermMemory persistence file (from workspace).
    ltm_path: Option<PathBuf>,
    /// Path to the HistoryLog persistence file (from workspace).
    history_path: Option<PathBuf>,
    /// Path to the DynamicCheatsheet persistence file (from workspace).
    cheatsheet_path: Option<PathBuf>,
}

impl SlidingWindowFactory {
    /// Create a factory without workspace persistence (in-memory only).
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            ltm_path: None,
            history_path: None,
            cheatsheet_path: None,
        }
    }

    /// Create a factory with workspace persistence paths.
    ///
    /// When set, newly created memories will automatically load
    /// persisted state from these paths.
    #[allow(dead_code)]
    pub fn with_workspace(
        ltm_path: PathBuf,
        history_path: PathBuf,
    ) -> Self {
        Self {
            ltm_path: Some(ltm_path),
            history_path: Some(history_path),
            cheatsheet_path: None,
        }
    }

    /// Create a factory with workspace persistence paths including cheatsheet.
    ///
    /// When set, newly created memories will automatically load
    /// persisted state from these paths, including the dynamic cheatsheet.
    pub fn with_workspace_and_cheatsheet(
        ltm_path: PathBuf,
        history_path: PathBuf,
        cheatsheet_path: PathBuf,
    ) -> Self {
        Self {
            ltm_path: Some(ltm_path),
            history_path: Some(history_path),
            cheatsheet_path: Some(cheatsheet_path),
        }
    }
}

impl MemoryFactory for SlidingWindowFactory {
    fn create_memory(&self, system_prompt: &str) -> Box<dyn Memory> {
        let mut memory = SlidingWindowMemory::with_defaults(system_prompt);

        // Load persisted state from workspace if paths are configured
        if let (Some(ltm_path), Some(history_path)) = (&self.ltm_path, &self.history_path) {
            if let Err(e) = memory.load_from_workspace(ltm_path, history_path) {
                tracing::warn!(
                    error = %e,
                    "Failed to load persisted memory from workspace, starting fresh"
                );
            }
        }

        // Load persisted cheatsheet if path is configured
        if let Some(cs_path) = &self.cheatsheet_path {
            let cs = match DynamicCheatsheet::load(cs_path) {
                Ok(cs) if !cs.is_empty() => cs,
                Ok(_) => {
                    // Empty cheatsheet — enable with defaults so reflection can populate it.
                    DynamicCheatsheet::with_defaults()
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Failed to load cheatsheet from workspace, starting fresh"
                    );
                    DynamicCheatsheet::with_defaults()
                }
            };
            memory.set_cheatsheet(cs);
        }

        Box::new(memory)
    }

    fn strategy_name(&self) -> &str {
        "sliding_window"
    }
}
