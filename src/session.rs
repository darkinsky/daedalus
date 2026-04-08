use chrono::Local;
use uuid::Uuid;

use crate::llm::ChatMessage;
use crate::memory::Memory;

/// A conversation session with a unique ID, title, request counter, and memory.
///
/// Each session owns its own `Memory` instance, which manages the conversation
/// history for that session. The session fully encapsulates memory access,
/// providing delegate methods for all memory operations.
pub struct Session {
    /// Unique session identifier.
    pub id: String,
    /// Human-readable session title.
    pub title: String,
    /// Auto-incrementing request counter (number of completed requests).
    pub request_count: u64,
    /// Timestamp when the session was created.
    #[allow(dead_code)]
    pub created_at: String,
    /// Conversation memory for this session.
    memory: Box<dyn Memory>,
}

impl Session {
    /// Create a new session with the given memory strategy.
    pub fn new(memory: Box<dyn Memory>) -> Self {
        let now = Local::now();
        let id = Uuid::new_v4().to_string();
        let title = format!("Session {}", now.format("%Y-%m-%d %H:%M:%S"));

        tracing::info!(session_id = %id, title = %title, "New session created");

        Self {
            id,
            title,
            request_count: 0,
            created_at: now.format("%Y-%m-%d %H:%M:%S").to_string(),
            memory,
        }
    }

    /// Increment and return the next request ID.
    pub fn next_request_id(&mut self) -> u64 {
        self.request_count += 1;
        self.request_count
    }

    // ── Memory delegate methods ──

    /// Add a user message to the session's memory.
    pub fn add_user_message(&mut self, content: &str) {
        self.memory.add_user_message(content);
    }

    /// Add an assistant message to the session's memory.
    pub fn add_assistant_message(&mut self, content: &str) {
        self.memory.add_assistant_message(content);
    }

    /// Build the message list to send to the LLM (delegated to memory).
    pub fn build_messages(&self) -> Vec<ChatMessage> {
        self.memory.build_messages()
    }

    /// Return the number of conversation turns stored.
    pub fn turn_count(&self) -> usize {
        self.memory.turn_count()
    }

    /// Return the memory strategy name.
    pub fn strategy_name(&self) -> &str {
        self.memory.strategy_name()
    }

    /// Clear all conversation history (but keep the system prompt).
    #[allow(dead_code)]
    pub fn clear_memory(&mut self) {
        self.memory.clear();
    }
}
