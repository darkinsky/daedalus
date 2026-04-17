use std::path::PathBuf;
use std::sync::Arc;

use crate::embedding::Embedding;
use crate::memory::{Memory, MemoryFactory};
use crate::memory::persistence::MemoryPersistence;

use super::config::MemPalaceConfig;
use super::memory::MemPalaceMemory;
use super::store::MemPalaceStore;

/// Factory for creating `MemPalaceMemory` instances.
///
/// Requires an embedding provider (mandatory — no fallback).
/// When workspace paths are configured, the factory will automatically
/// load persisted palace data from disk.
///
/// ChromaDB must be running and accessible at the configured URL.
pub struct MemPalaceFactory {
    /// Path to the MemPalace persistence directory (from workspace).
    mempalace_dir: Option<PathBuf>,
    /// Shared embedding provider (required).
    embedder: Arc<dyn Embedding>,
    /// Configuration.
    config: MemPalaceConfig,
}

impl MemPalaceFactory {
    /// Create a factory without workspace persistence (in-memory only).
    #[allow(dead_code)]
    pub fn new(embedder: Arc<dyn Embedding>) -> Self {
        Self {
            mempalace_dir: None,
            embedder,
            config: MemPalaceConfig::default(),
        }
    }

    /// Create a factory with workspace persistence path.
    pub fn with_workspace(
        mempalace_dir: PathBuf,
        embedder: Arc<dyn Embedding>,
    ) -> Self {
        Self {
            mempalace_dir: Some(mempalace_dir),
            embedder,
            config: MemPalaceConfig::default(),
        }
    }

    /// Create a factory with workspace persistence and custom config.
    #[allow(dead_code)]
    pub fn with_workspace_and_config(
        mempalace_dir: PathBuf,
        embedder: Arc<dyn Embedding>,
        config: MemPalaceConfig,
    ) -> Self {
        Self {
            mempalace_dir: Some(mempalace_dir),
            embedder,
            config,
        }
    }
}

impl MemoryFactory for MemPalaceFactory {
    fn create_memory(&self, system_prompt: &str) -> Box<dyn Memory> {
        let store = match &self.mempalace_dir {
            Some(dir) => match MemPalaceStore::load(dir) {
                Ok(s) if !s.is_empty() => {
                    tracing::info!(
                        path = %dir.display(),
                        wings = s.palace.wing_count(),
                        drawers = s.total_drawers(),
                        "Loaded MemPalace from disk"
                    );
                    s
                }
                Ok(_) => {
                    tracing::debug!(
                        path = %dir.display(),
                        "MemPalace directory is empty, starting fresh"
                    );
                    MemPalaceStore::new()
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Failed to load MemPalace from workspace, starting fresh"
                    );
                    MemPalaceStore::new()
                }
            },
            None => MemPalaceStore::new(),
        };

        Box::new(MemPalaceMemory::with_store(
            system_prompt,
            store,
            self.embedder.clone(),
            self.config.clone(),
        ))
    }

    fn strategy_name(&self) -> &str {
        "mempalace"
    }
}
