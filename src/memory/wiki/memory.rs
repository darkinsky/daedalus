use std::sync::Arc;

use crate::embedding::Embedding;
use crate::llm::{ChatMessage, LlmApi};
use crate::memory::{Memory, PersistentState, DEFAULT_MAX_MESSAGES};

use super::compiler::WikiCompiler;
use super::store::WikiStore;

/// Persistent state for `WikiMemory` across session migrations.
struct WikiPersistentState {
    store: WikiStore,
}

/// LLM Wiki memory strategy — structured knowledge compilation
/// with Markdown persistence.
///
/// Implements Karpathy's LLM Wiki pattern: instead of just retrieving
/// from raw documents (traditional RAG), the LLM progressively builds
/// and maintains a persistent wiki — a structured, interlinked collection
/// of Markdown files that sits between you and the raw material.
///
/// ## How it works
///
/// 1. **On `build_messages`**: Relevant wiki pages are retrieved and
///    injected into the system prompt as context.
/// 2. **After each turn** (`reflect_on_turn`): The conversation is
///    "compiled" into the wiki — the LLM extracts entities, concepts,
///    and facts, then creates/updates wiki pages with wikilinks.
/// 3. **Periodically** (every N turns): A "lint" pass checks the wiki
///    for contradictions, broken links, and duplicates.
///
/// ## Retrieval modes
///
/// - **With embedding** (enhanced): Cosine similarity search for
///   high-quality semantic retrieval.
/// - **Without embedding** (basic): Keyword matching + wikilink
///   traversal. Zero external dependencies.
///
/// ## Persistence
///
/// Each wiki page is a `.md` file with YAML frontmatter (Obsidian-compatible).
/// Embedding vectors are stored in `_meta.json`. The wiki directory can be
/// browsed and edited by users directly.
pub struct WikiMemory {
    /// The original system prompt (without wiki context injection).
    base_system_prompt: String,
    /// All conversation messages (user + assistant), in chronological order.
    messages: Vec<ChatMessage>,
    /// The wiki store (knowledge pages + metadata).
    store: WikiStore,
    /// Optional embedding provider for vector search.
    /// When `None`, retrieval falls back to keyword matching + wikilinks.
    embedder: Option<Arc<dyn Embedding>>,
    /// Cached context from the most recent retrieval (injected into system prompt).
    cached_context: Option<String>,
    /// Maximum number of messages to include in `build_messages()`.
    max_messages: usize,
}

impl WikiMemory {
    /// Create a new wiki memory with an optional embedding provider.
    ///
    /// When `embedder` is `Some`, retrieval uses cosine similarity (enhanced mode).
    /// When `embedder` is `None`, retrieval falls back to keyword matching + wikilinks.
    #[allow(dead_code)]
    pub fn new(
        system_prompt: &str,
        store: WikiStore,
        embedder: Option<Arc<dyn Embedding>>,
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

    /// Build the effective system prompt by injecting retrieved wiki context.
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

impl Memory for WikiMemory {
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
        "wiki"
    }

    fn take_persistent_state(&mut self) -> Option<PersistentState> {
        let wiki_dir = self.store.wiki_dir().to_path_buf();
        let state = WikiPersistentState {
            store: std::mem::replace(
                &mut self.store,
                WikiStore::new(&wiki_dir),
            ),
        };
        Some(PersistentState::new(state))
    }

    fn restore_persistent_state(&mut self, state: PersistentState) {
        match state.downcast::<WikiPersistentState>() {
            Ok(s) => {
                self.store = s.store;
            }
            Err(_) => {
                tracing::warn!("Wiki persistent state type mismatch, state discarded");
            }
        }
    }

    fn persist(&self, workspace: &crate::workspace::Workspace) -> anyhow::Result<()> {
        use crate::memory::persistence::MemoryPersistence;
        self.store.save(&workspace.wiki_dir())?;
        Ok(())
    }

    fn reflect_on_turn<'a>(
        &'a mut self,
        user_input: &'a str,
        assistant_response: &'a str,
        llm: &'a dyn LlmApi,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            // NOTE: `self.embedder.as_deref()` must be re-bound before each
            // usage because the mutable borrow of `self.store` in compile()
            // invalidates any prior shared reference derived from `self`.
            // This is a Rust borrow checker constraint, not a logic issue.

            // Step 1: Compile the conversation turn into wiki updates.
            // This triggers the Ingest + Compile workflow:
            // extract entities/concepts → create/update pages → establish links.
            let embedder_ref = self.embedder.as_deref();
            if let Err(e) = WikiCompiler::compile(
                &mut self.store,
                user_input,
                assistant_response,
                llm,
                embedder_ref,
            )
            .await
            {
                tracing::warn!(
                    error = %e,
                    "Wiki compile failed, knowledge not persisted for this turn"
                );
            }

            // Step 2: Increment turn counter and check if lint is needed.
            self.store.increment_turn();
            if self.store.should_lint() {
                match WikiCompiler::lint(&self.store, llm).await {
                    Ok(()) => {
                        self.store.reset_lint_counter();
                        tracing::debug!("Wiki lint completed successfully");
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "Wiki lint failed, will retry on next interval"
                        );
                    }
                }
            }

            // Step 3: Pre-retrieve context for the *next* turn.
            // Re-bind embedder_ref after the mutable borrow of self.store above.
            let embedder_ref = self.embedder.as_deref();
            match self
                .store
                .query_context(user_input, embedder_ref, None)
                .await
            {
                Ok(ctx) => {
                    self.cached_context = ctx;
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Failed to pre-retrieve wiki context"
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
