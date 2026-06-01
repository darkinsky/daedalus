//! Memory factory selection — maps strategy config to the appropriate factory.

use super::{
    MemoryFactory, SlidingWindowFactory, CheatsheetFactory,
    AgenticFactory, MemPalaceFactory, WikiFactory, AceFactory,
};
use super::sliding_window::config::SlidingWindowConfig;

/// Create the appropriate memory factory based on the configured strategy and workspace.
///
/// Each strategy has its own factory that knows how to load persisted state from
/// the workspace and create configured memory instances.
///
/// Strategies that require an embedding provider (Agentic, MemPalace) will
/// gracefully fall back to `SlidingWindow` if the embedding configuration is
/// missing or invalid, rather than panicking.
pub fn create_memory_factory(
    strategy: &crate::config::MemoryStrategy,
    memory_config: &crate::config::agent_config::MemorySection,
    embedding_config: &crate::config::EmbeddingConfig,
    workspace: &crate::workspace::Workspace,
) -> Box<dyn MemoryFactory> {
    use crate::config::MemoryStrategy;

    match strategy {
        MemoryStrategy::SlidingWindow => sliding_window_factory(workspace, &memory_config.sliding_window),
        MemoryStrategy::DynamicCheatsheet => {
            let factory = CheatsheetFactory::with_workspace(
                workspace.cheatsheet_path(),
                memory_config.dynamic_cheatsheet.clone(),
            );
            Box::new(factory)
        }
        MemoryStrategy::Agentic => {
            match embedding_config.create_provider() {
                Ok(embedder) => {
                    let factory = AgenticFactory::with_workspace(
                        workspace.agentic_notes_path(),
                        embedder,
                    );
                    Box::new(factory)
                }
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "Failed to create embedding provider for agentic memory, \
                         falling back to sliding_window"
                    );
                    sliding_window_factory(workspace, &memory_config.sliding_window)
                }
            }
        }
        MemoryStrategy::Wiki => {
            match embedding_config.create_provider() {
                Ok(embedder) => {
                    tracing::info!(
                        "Wiki memory initialized with embedding provider (enhanced retrieval)"
                    );
                    let factory = WikiFactory::with_workspace(
                        workspace.wiki_dir(),
                        embedder,
                    );
                    Box::new(factory)
                }
                Err(e) => {
                    tracing::info!(
                        error = %e,
                        "No embedding provider configured for wiki memory, \
                         using keyword-only retrieval mode"
                    );
                    let factory = WikiFactory::with_workspace_only(
                        workspace.wiki_dir(),
                    );
                    Box::new(factory)
                }
            }
        }
        MemoryStrategy::Ace => {
            let factory = AceFactory::with_workspace(
                workspace.ace_playbook_path(),
                memory_config.ace.clone(),
            );
            Box::new(factory)
        }
        MemoryStrategy::MemPalace => {
            match embedding_config.create_provider() {
                Ok(embedder) => {
                    tracing::info!(
                        "MemPalace memory initialized with embedding provider and ChromaDB"
                    );
                    let factory = MemPalaceFactory::with_workspace(
                        workspace.mempalace_dir(),
                        embedder,
                    );
                    Box::new(factory)
                }
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "MemPalace memory requires embedding configuration, \
                         falling back to sliding_window. \
                         Please configure `embedding` section in daedalus.yaml."
                    );
                    sliding_window_factory(workspace, &memory_config.sliding_window)
                }
            }
        }
    }
}

/// Create a sliding-window memory factory with workspace persistence and config.
fn sliding_window_factory(
    workspace: &crate::workspace::Workspace,
    config: &SlidingWindowConfig,
) -> Box<dyn MemoryFactory> {
    Box::new(SlidingWindowFactory::with_workspace(
        workspace.long_term_memory_path(),
        workspace.history_log_path(),
    )
    .with_session_messages_path(workspace.session_messages_path())
    .with_config(config.clone()))
}
