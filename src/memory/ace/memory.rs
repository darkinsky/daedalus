use crate::llm::{ChatMessage, LlmApi};
use crate::memory::{Memory, MessageBuffer, PersistentState, DEFAULT_MAX_MESSAGES};

use super::config::AceConfig;
use super::playbook::Playbook;
use super::reflector::Reflector;

/// Persistent state for `AceMemory` across session migrations.
struct AcePersistentState {
    playbook: Playbook,
}

/// ACE (Agentic Context Engineering) memory strategy.
///
/// Implements the ACE framework from arxiv:2510.04618. The core idea is
/// to treat the context as an evolving "Playbook" that accumulates,
/// refines, and organizes strategies through a modular process of
/// generation, reflection, and curation.
///
/// ## Key Innovation
///
/// Unlike `DynamicCheatsheet` (flat entry list with LLM-driven updates),
/// ACE uses:
/// - **Hierarchical structure**: Playbook → Sections → Bullets
/// - **Deterministic Curator**: Delta entries are merged without LLM calls
/// - **Anti-collapse design**: LLM only produces small deltas, never rewrites
///
/// ## Lifecycle
///
/// 1. **Inject**: `build_messages()` renders the Playbook as Markdown
///    and injects it into the system prompt.
/// 2. **Reflect**: After each turn, the Reflector calls the LLM to
///    produce delta entries (ADD/UPDATE/REINFORCE/REMOVE).
/// 3. **Curate**: The Curator applies deltas using deterministic logic
///    (no LLM calls), preventing context collapse.
///
/// ## No Embedding Required
///
/// ACE is a pure LLM reflection strategy — it does not require
/// embedding providers or vector search.
///
/// Reference: ACE (arxiv:2510.04618), kayba-ai/agentic-context-engine
pub struct AceMemory {
    /// The original system prompt (without playbook injection).
    base_system_prompt: String,
    /// Conversation message buffer with sliding window.
    buffer: MessageBuffer,
    /// The evolving playbook — structured collection of strategies and insights.
    playbook: Playbook,
    /// Configuration parameters.
    config: AceConfig,
}

impl AceMemory {
    /// Create a new ACE memory with default configuration.
    #[allow(dead_code)]
    pub fn new(system_prompt: &str) -> Self {
        Self {
            base_system_prompt: system_prompt.to_string(),
            buffer: MessageBuffer::new(DEFAULT_MAX_MESSAGES),
            playbook: Playbook::new(),
            config: AceConfig::default(),
        }
    }

    /// Create with an existing playbook (e.g., loaded from disk).
    pub fn with_playbook(system_prompt: &str, playbook: Playbook) -> Self {
        Self {
            base_system_prompt: system_prompt.to_string(),
            buffer: MessageBuffer::new(DEFAULT_MAX_MESSAGES),
            playbook,
            config: AceConfig::default(),
        }
    }

    /// Build the effective system prompt by injecting the playbook.
    fn effective_system_prompt(&self) -> String {
        match self.playbook.to_markdown(self.config.max_token_budget) {
            Some(pb_md) => format!("{}\n\n{}", self.base_system_prompt, pb_md),
            None => self.base_system_prompt.clone(),
        }
    }
}

impl Memory for AceMemory {
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
    }

    fn turn_count(&self) -> usize {
        self.buffer.turn_count()
    }

    fn strategy_name(&self) -> &str {
        "ace"
    }

    fn take_persistent_state(&mut self) -> Option<PersistentState> {
        let state = AcePersistentState {
            playbook: std::mem::replace(&mut self.playbook, Playbook::new()),
        };
        Some(PersistentState::new(state))
    }

    fn restore_persistent_state(&mut self, state: PersistentState) {
        match state.downcast::<AcePersistentState>() {
            Ok(s) => {
                self.playbook = s.playbook;
            }
            Err(_) => {
                tracing::warn!("Persistent state type mismatch, state discarded");
            }
        }
    }

    fn persist(&self, workspace: &crate::workspace::Workspace) -> anyhow::Result<()> {
        use crate::memory::persistence::MemoryPersistence;
        self.playbook.save(&workspace.ace_playbook_path())?;
        Ok(())
    }

    fn reflect_on_turn<'a>(
        &'a mut self,
        user_input: &'a str,
        assistant_response: &'a str,
        llm: &'a dyn LlmApi,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            // Advance the turn counter before reflection.
            self.playbook.advance_turn();

            Reflector::reflect_and_curate(
                &mut self.playbook,
                user_input,
                assistant_response,
                llm,
                &self.config,
            ).await;
        })
    }
}
