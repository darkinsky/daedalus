use std::path::PathBuf;

use crate::memory::{Memory, MemoryFactory};
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
}

impl SlidingWindowFactory {
    /// Create a factory without workspace persistence (in-memory only).
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            ltm_path: None,
            history_path: None,
        }
    }

    /// Create a factory with workspace persistence paths.
    ///
    /// When set, newly created memories will automatically load
    /// persisted state from these paths.
    pub fn with_workspace(
        ltm_path: PathBuf,
        history_path: PathBuf,
    ) -> Self {
        Self {
            ltm_path: Some(ltm_path),
            history_path: Some(history_path),
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

        Box::new(memory)
    }

    fn strategy_name(&self) -> &str {
        "sliding_window"
    }
}
