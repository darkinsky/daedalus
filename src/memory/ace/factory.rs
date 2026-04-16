use std::path::PathBuf;

use crate::memory::{Memory, MemoryFactory};
use crate::memory::persistence::MemoryPersistence;

use super::memory::AceMemory;
use super::playbook::Playbook;

/// Factory for creating `AceMemory` instances.
///
/// When workspace paths are configured, the factory will automatically
/// load a persisted playbook from disk.
pub struct AceFactory {
    /// Path to the Playbook persistence file (from workspace).
    playbook_path: Option<PathBuf>,
}

impl AceFactory {
    /// Create a factory without workspace persistence (in-memory only).
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            playbook_path: None,
        }
    }

    /// Create a factory with workspace persistence path.
    pub fn with_workspace(playbook_path: PathBuf) -> Self {
        Self {
            playbook_path: Some(playbook_path),
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

        Box::new(AceMemory::with_playbook(system_prompt, playbook))
    }

    fn strategy_name(&self) -> &str {
        "ace"
    }
}
