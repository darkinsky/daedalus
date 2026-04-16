use crate::llm::{ChatMessage, LlmApi};
use crate::memory::{Memory, MessageBuffer, PersistentState, DEFAULT_MAX_MESSAGES};

use super::cheatsheet::DynamicCheatsheet;

/// Persistent state for `CheatsheetMemory` across session migrations.
struct CheatsheetPersistentState {
    cheatsheet: DynamicCheatsheet,
}

/// Standalone memory strategy backed by a Dynamic Cheatsheet.
///
/// Unlike the `SlidingWindowMemory` integration (where DC is an optional
/// component), this is a **full `Memory` implementation** that manages
/// its own conversation messages and injects the cheatsheet into the
/// system prompt.
///
/// Best suited for repetitive task patterns where accumulating
/// problem-solving insights provides the most value.
pub struct CheatsheetMemory {
    /// The original system prompt (without cheatsheet injection).
    base_system_prompt: String,
    /// Conversation message buffer with sliding window.
    buffer: MessageBuffer,
    /// The dynamic cheatsheet engine.
    cheatsheet: DynamicCheatsheet,
}

impl CheatsheetMemory {
    /// Create a new cheatsheet memory with default configuration.
    #[allow(dead_code)]
    pub fn new(system_prompt: &str) -> Self {
        Self {
            base_system_prompt: system_prompt.to_string(),
            buffer: MessageBuffer::new(DEFAULT_MAX_MESSAGES),
            cheatsheet: DynamicCheatsheet::with_defaults(),
        }
    }

    /// Create with an existing cheatsheet (e.g., loaded from disk).
    pub fn with_cheatsheet(system_prompt: &str, cheatsheet: DynamicCheatsheet) -> Self {
        Self {
            base_system_prompt: system_prompt.to_string(),
            buffer: MessageBuffer::new(DEFAULT_MAX_MESSAGES),
            cheatsheet,
        }
    }

    /// Build the effective system prompt by injecting the cheatsheet.
    fn effective_system_prompt(&self) -> String {
        match self.cheatsheet.to_markdown() {
            Some(cs_md) => format!("{}\n\n{}", self.base_system_prompt, cs_md),
            None => self.base_system_prompt.clone(),
        }
    }
}

impl Memory for CheatsheetMemory {
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
        "dynamic_cheatsheet"
    }

    fn take_persistent_state(&mut self) -> Option<PersistentState> {
        let state = CheatsheetPersistentState {
            cheatsheet: std::mem::replace(
                &mut self.cheatsheet,
                DynamicCheatsheet::with_defaults(),
            ),
        };
        Some(PersistentState::new(state))
    }

    fn restore_persistent_state(&mut self, state: PersistentState) {
        match state.downcast::<CheatsheetPersistentState>() {
            Ok(s) => {
                self.cheatsheet = s.cheatsheet;
            }
            Err(_) => {
                tracing::warn!("Persistent state type mismatch, state discarded");
            }
        }
    }

    fn persist(&self, workspace: &crate::workspace::Workspace) -> anyhow::Result<()> {
        use crate::memory::persistence::MemoryPersistence;
        self.cheatsheet.save(&workspace.cheatsheet_path())?;
        Ok(())
    }

    fn reflect_on_turn<'a>(
        &'a mut self,
        user_input: &'a str,
        assistant_response: &'a str,
        llm: &'a dyn LlmApi,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            self.cheatsheet.reflect(user_input, assistant_response, llm).await;
        })
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
