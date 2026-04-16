use std::path::PathBuf;
use std::sync::Arc;

use crate::embedding::Embedding;
use crate::memory::persistence::MemoryPersistence;
use crate::memory::{Memory, MemoryFactory};

use super::memory::WikiMemory;
use super::store::WikiStore;

/// Factory for creating `WikiMemory` instances.
///
/// The embedding provider is **optional**:
/// - **With embedding**: Pages get embedding vectors for cosine similarity retrieval.
/// - **Without embedding**: Retrieval falls back to keyword matching + wikilink traversal.
///
/// When workspace paths are configured, the factory will automatically
/// load persisted wiki pages from the Markdown directory.
pub struct WikiFactory {
    /// Path to the wiki directory (from workspace).
    wiki_dir: Option<PathBuf>,
    /// Optional shared embedding provider.
    /// When `None`, wiki operates in keyword-only mode.
    embedder: Option<Arc<dyn Embedding>>,
}

impl WikiFactory {
    /// Create a factory without workspace persistence and without embedding (in-memory, keyword-only).
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            wiki_dir: None,
            embedder: None,
        }
    }

    /// Create a factory with workspace persistence path but without embedding.
    ///
    /// Wiki pages will be persisted as Markdown files, but retrieval
    /// will use keyword matching + wikilink traversal only.
    pub fn with_workspace_only(wiki_dir: PathBuf) -> Self {
        Self {
            wiki_dir: Some(wiki_dir),
            embedder: None,
        }
    }

    /// Create a factory with workspace persistence path and embedding provider.
    ///
    /// Wiki pages will be persisted as Markdown files, and retrieval
    /// will use embedding cosine similarity (enhanced mode).
    pub fn with_workspace(wiki_dir: PathBuf, embedder: Arc<dyn Embedding>) -> Self {
        Self {
            wiki_dir: Some(wiki_dir),
            embedder: Some(embedder),
        }
    }
}

impl MemoryFactory for WikiFactory {
    fn create_memory(&self, system_prompt: &str) -> Box<dyn Memory> {
        let store = match &self.wiki_dir {
            Some(dir) => match WikiStore::load(dir) {
                Ok(s) if !s.is_empty() => {
                    tracing::info!(
                        path = %dir.display(),
                        pages = s.page_count(),
                        has_embedding = self.embedder.is_some(),
                        "Loaded wiki from Markdown files"
                    );
                    s
                }
                Ok(_) => {
                    tracing::debug!(
                        path = %dir.display(),
                        "Wiki directory is empty, starting fresh"
                    );
                    WikiStore::new(dir)
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Failed to load wiki from workspace, starting fresh"
                    );
                    WikiStore::new(dir)
                }
            },
            None => WikiStore::new(std::path::Path::new("")),
        };

        let retrieval_mode = if self.embedder.is_some() {
            "embedding+keywords"
        } else {
            "keywords-only"
        };
        tracing::info!(
            retrieval_mode = retrieval_mode,
            "WikiMemory created"
        );

        Box::new(WikiMemory::new(system_prompt, store, self.embedder.clone()))
    }

    fn strategy_name(&self) -> &str {
        "wiki"
    }
}
