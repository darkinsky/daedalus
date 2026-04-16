use std::sync::Arc;

use crate::embedding::Embedding;
use crate::llm::{ChatMessage, LlmApi};
use crate::memory::{Memory, PersistentState, DEFAULT_MAX_MESSAGES};

use super::store::AgenticMemoryStore;

/// Persistent state for `AgenticMemory` across session migrations.
struct AgenticPersistentState {
    store: AgenticMemoryStore,
}

/// Standalone memory strategy backed by the A-MEM knowledge graph.
///
/// This is a **full `Memory` implementation** that manages its own
/// conversation messages and injects relevant memory context (retrieved
/// via embedding similarity) into the system prompt.
///
/// Best suited for long-term knowledge accumulation across sessions,
/// where semantic relationships between memories provide value.
///
/// ## How it works
///
/// 1. **After each turn** (`reflect_on_turn`): The assistant's response
///    is stored as a new memory note, triggering the A-MEM lifecycle
///    (note construction → link generation → memory evolution). Then,
///    the user's input is used to pre-retrieve relevant memories for
///    the *next* turn.
/// 2. **On `build_messages`**: The pre-cached context is injected into
///    the system prompt, so the LLM sees relevant past knowledge.
///
/// This design avoids calling async code from sync `add_user_message`,
/// which would require `block_on` and risk deadlocking the tokio runtime.
pub struct AgenticMemory {
    /// The original system prompt (without memory injection).
    base_system_prompt: String,
    /// All conversation messages (user + assistant), in chronological order.
    messages: Vec<ChatMessage>,
    /// The A-MEM knowledge graph store.
    store: AgenticMemoryStore,
    /// Embedding provider for vector search.
    ///
    /// Wrapped in `Arc` because the `AgenticFactory` holds a shared reference
    /// and distributes clones to each memory instance it creates.
    embedder: Arc<dyn Embedding>,
    /// Cached context from the most recent retrieval (injected into system prompt).
    /// Pre-populated at the end of `reflect_on_turn` for the *next* turn.
    cached_context: Option<String>,
    /// Maximum number of messages to include in `build_messages()`.
    max_messages: usize,
}

impl AgenticMemory {
    /// Create a new agentic memory with the given embedding provider.
    #[allow(dead_code)]
    pub fn new(system_prompt: &str, embedder: Arc<dyn Embedding>) -> Self {
        Self {
            base_system_prompt: system_prompt.to_string(),
            messages: Vec::new(),
            store: AgenticMemoryStore::new(),
            embedder,
            cached_context: None,
            max_messages: DEFAULT_MAX_MESSAGES,
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
            messages: Vec::new(),
            store,
            embedder,
            cached_context: None,
            max_messages: DEFAULT_MAX_MESSAGES,
        }
    }

    /// Build the effective system prompt by injecting retrieved memory context.
    fn effective_system_prompt(&self) -> String {
        match &self.cached_context {
            Some(ctx) => format!("{}\n\n{}", self.base_system_prompt, ctx),
            None => self.base_system_prompt.clone(),
        }
    }

    /// Get the windowed slice of messages to send to the LLM.
    fn windowed_messages(&self) -> &[ChatMessage] {
        if self.messages.len() <= self.max_messages {
            &self.messages[..]
        } else {
            &self.messages[self.messages.len() - self.max_messages..]
        }
    }
}

impl Memory for AgenticMemory {
    fn add_user_message(&mut self, content: &str) {
        self.messages.push(ChatMessage::user(content));
    }

    fn add_assistant_message(&mut self, content: &str) {
        self.messages.push(ChatMessage::assistant(content));
    }

    fn build_messages(&self) -> Vec<ChatMessage> {
        let system_prompt = self.effective_system_prompt();
        let window = self.windowed_messages();
        let mut messages = Vec::with_capacity(1 + window.len());
        messages.push(ChatMessage::system(system_prompt));
        messages.extend(window.iter().cloned());
        messages
    }

    fn clear(&mut self) {
        self.messages.clear();
        self.cached_context = None;
    }

    fn turn_count(&self) -> usize {
        self.messages.len() / 2
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
            // Step 1: Store the assistant's response as a new memory note.
            // This triggers the full A-MEM lifecycle:
            // note construction → link generation → memory evolution.
            if let Err(e) = self.store.add_memory(
                assistant_response, llm, &*self.embedder,
            ).await {
                tracing::warn!(
                    error = %e,
                    "Failed to add memory note from assistant response"
                );
            }

            // Step 2: Pre-retrieve context for the *next* turn.
            // Using the current user_input as the query gives the best
            // approximation of what the next query might relate to.
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

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
