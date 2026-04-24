use std::sync::Arc;

use crate::embedding::Embedding;
use crate::llm::{ChatMessage, LlmApi};
use crate::memory::{Memory, MessageBuffer, PersistentState, DEFAULT_MAX_MESSAGES};

use super::store::AgenticMemoryStore;

/// Persistent state for `AgenticMemory` across session migrations.
struct AgenticPersistentState {
    store: AgenticMemoryStore,
}

/// Standalone memory strategy backed by the A-MEM knowledge graph.
///
/// This is a **full `Memory` implementation** that manages its own
/// conversation messages and injects relevant memory context (retrieved
/// via graph-expanded agentic search) into the system prompt.
///
/// Best suited for long-term knowledge accumulation across sessions,
/// where semantic relationships between memories provide value.
///
/// ## How it works
///
/// 1. **After each turn** (`reflect_on_turn`):
///    - The **combined user+assistant interaction** is stored as a new memory
///      note, triggering the A-MEM lifecycle (note construction → unified
///      process_memory with linking + selective evolution).
///    - The user's input is used to pre-retrieve relevant memories for
///      the *next* turn using `search_agentic()` (graph-expanded retrieval).
/// 2. **On `build_messages`**: The pre-cached context (with injection
///    preamble/epilogue) is injected into the system prompt.
///
/// ## Key differences from the old implementation
///
/// - Stores **both user input and assistant response** (not just response)
/// - Uses **`search_agentic()`** for graph-expanded retrieval (traverses links)
/// - Context injection includes **preamble** guiding the LLM to use memories
pub struct AgenticMemory {
    /// The original system prompt (without memory injection).
    base_system_prompt: String,
    /// Conversation message buffer with sliding window.
    buffer: MessageBuffer,
    /// The A-MEM knowledge graph store.
    store: AgenticMemoryStore,
    /// Embedding provider for vector search.
    embedder: Arc<dyn Embedding>,
    /// Cached context from the most recent retrieval (injected into system prompt).
    /// Pre-populated at the end of `reflect_on_turn` for the *next* turn.
    cached_context: Option<String>,
}

impl AgenticMemory {
    /// Create a new agentic memory with the given embedding provider.
    #[allow(dead_code)]
    pub fn new(system_prompt: &str, embedder: Arc<dyn Embedding>) -> Self {
        Self {
            base_system_prompt: system_prompt.to_string(),
            buffer: MessageBuffer::new(DEFAULT_MAX_MESSAGES),
            store: AgenticMemoryStore::new(),
            embedder,
            cached_context: None,
        }
    }

    /// Create with an existing store (e.g., loaded from disk).
    pub fn with_store(
        system_prompt: &str,
        store: AgenticMemoryStore,
        embedder: Arc<dyn Embedding>,
    ) -> Self {
        Self {
            base_system_prompt: system_prompt.to_string(),
            buffer: MessageBuffer::new(DEFAULT_MAX_MESSAGES),
            store,
            embedder,
            cached_context: None,
        }
    }

    /// Build the effective system prompt by injecting retrieved memory context.
    ///
    /// The context already includes the preamble/epilogue tags from
    /// `retrieve_context()`, so we just append it to the base prompt.
    fn effective_system_prompt(&self) -> String {
        match &self.cached_context {
            Some(ctx) => format!("{}\n\n{}", self.base_system_prompt, ctx),
            None => self.base_system_prompt.clone(),
        }
    }
}

impl Memory for AgenticMemory {
    fn add_user_message(&mut self, content: &str) {
        self.buffer.add_user(content);
    }

    fn add_assistant_message(&mut self, content: &str) {
        self.buffer.add_assistant(content);
    }

    fn build_messages(&self) -> Vec<ChatMessage> {
        self.buffer.build_messages_with_system(self.effective_system_prompt())
    }

    fn clear(&mut self) {
        self.buffer.clear();
        self.cached_context = None;
    }

    fn turn_count(&self) -> usize {
        self.buffer.turn_count()
    }

    fn strategy_name(&self) -> &str {
        "agentic"
    }

    fn take_persistent_state(&mut self) -> Option<PersistentState> {
        let state = AgenticPersistentState {
            store: std::mem::replace(&mut self.store, AgenticMemoryStore::new()),
        };
        Some(PersistentState::new(state))
    }

    fn restore_persistent_state(&mut self, state: PersistentState) {
        match state.downcast::<AgenticPersistentState>() {
            Ok(s) => {
                self.store = s.store;
            }
            Err(_) => {
                tracing::warn!("Persistent state type mismatch, state discarded");
            }
        }
    }

    fn persist(&self, workspace: &crate::workspace::Workspace) -> anyhow::Result<()> {
        use crate::memory::persistence::MemoryPersistence;
        self.store.save(&workspace.agentic_notes_path())?;
        Ok(())
    }

    fn reflect_on_turn<'a>(
        &'a mut self,
        user_input: &'a str,
        assistant_response: &'a str,
        llm: &'a dyn LlmApi,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            // Step 1: Store the combined interaction as a memory note.
            //
            // The paper stores any important information — not just responses.
            // By combining user input + assistant response, we capture:
            // - User preferences and constraints (from user_input)
            // - Factual information and decisions (from assistant_response)
            let combined_content = format!(
                "User: {}\n\nAssistant: {}",
                user_input, assistant_response
            );

            // Truncate very long responses to avoid oversized memory notes.
            let content = if combined_content.len() > 4000 {
                format!("{}...(truncated)", &combined_content[..4000])
            } else {
                combined_content
            };

            if let Err(e) = self.store.add_memory(
                &content, llm, &*self.embedder,
            ).await {
                tracing::warn!(
                    error = %e,
                    "Failed to add memory note from conversation turn"
                );
            }

            // Step 2: Pre-retrieve context for the *next* turn.
            // Using search_agentic for graph-expanded retrieval.
            match self.store.retrieve_context(
                user_input, &*self.embedder, None,
            ).await {
                Ok(ctx) => {
                    self.cached_context = ctx;
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Failed to pre-retrieve agentic memory context"
                    );
                }
            }
        })
    }
}
