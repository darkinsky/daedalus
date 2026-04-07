use chrono::Local;
use uuid::Uuid;

use crate::llm::ChatMessage;
use crate::memory::Memory;

/// A conversation session with a unique ID, title, request counter, and memory.
///
/// Each session owns its own `Memory` instance, which manages the conversation
/// history for that session. When a new session is created, a fresh memory is
/// initialized automatically.
pub struct Session {
    /// Unique session identifier.
    pub id: String,
    /// Human-readable session title.
    pub title: String,
    /// Auto-incrementing request counter (starts from 1).
    pub request_id: u64,
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
            request_id: 0,
            created_at: now.format("%Y-%m-%d %H:%M:%S").to_string(),
            memory,
        }
    }

    /// Increment and return the next request ID.
    pub fn next_request_id(&mut self) -> u64 {
        self.request_id += 1;
        self.request_id
    }

    /// Return a reference to the session's memory.
    pub fn memory(&self) -> &dyn Memory {
        self.memory.as_ref()
    }

    /// Return a mutable reference to the session's memory.
    pub fn memory_mut(&mut self) -> &mut dyn Memory {
        self.memory.as_mut()
    }

    /// Build the message list to send to the LLM (delegated to memory).
    pub fn build_messages(&self) -> Vec<ChatMessage> {
        self.memory.build_messages()
    }
}
