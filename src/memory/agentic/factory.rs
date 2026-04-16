use std::path::PathBuf;
use std::sync::Arc;

use crate::embedding::Embedding;
use crate::memory::{Memory, MemoryFactory};
use crate::memory::persistence::MemoryPersistence;

use super::memory::AgenticMemory;
use super::store::AgenticMemoryStore;

/// Factory for creating `AgenticMemory` instances.
///
/// Requires an embedding provider (passed at construction time).
/// When workspace paths are configured, the factory will automatically
/// load persisted A-MEM notes from disk.
pub struct AgenticFactory {
    /// Path to the A-MEM notes persistence file (from workspace).
    notes_path: Option<PathBuf>,
    /// Shared embedding provider.
    embedder: Arc<dyn Embedding>,
}

impl AgenticFactory {
    /// Create a factory without workspace persistence (in-memory only).
    #[allow(dead_code)]
    pub fn new(embedder: Arc<dyn Embedding>) -> Self {
        Self {
            notes_path: None,
            embedder,
        }
    }

    /// Create a factory with workspace persistence path.
    pub fn with_workspace(notes_path: PathBuf, embedder: Arc<dyn Embedding>) -> Self {
        Self {
            notes_path: Some(notes_path),
            embedder,
        }
    }
}

impl MemoryFactory for AgenticFactory {
    fn create_memory(&self, system_prompt: &str) -> Box<dyn Memory> {
        let store = match &self.notes_path {
            Some(path) => match AgenticMemoryStore::load(path) {
                Ok(s) if !s.is_empty() => s,
                Ok(_) => AgenticMemoryStore::new(),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Failed to load agentic memory from workspace, starting fresh"
                    );
                    AgenticMemoryStore::new()
                }
            },
            None => AgenticMemoryStore::new(),
        };

        Box::new(AgenticMemory::with_store(
            system_prompt,
            store,
            self.embedder.clone(),
        ))
    }

    fn strategy_name(&self) -> &str {
        "agentic"
    }
}
