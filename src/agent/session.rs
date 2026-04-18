use chrono::Local;
use uuid::Uuid;

use crate::memory::Memory;

/// A conversation session with a unique ID, title, request counter, and memory.
///
/// Each session owns its own `Memory` instance, which manages the conversation
/// history for that session. Callers access memory directly via `memory()` /
/// `memory_mut()` accessors rather than through thin delegation methods.
pub struct Session {
    /// Unique session identifier.
    pub id: String,
    /// Human-readable session title.
    pub title: String,
    /// Auto-incrementing request counter (number of completed requests).
    pub request_count: u64,
    /// Timestamp when the session was created.
    ///
    /// Reserved for future use (e.g., session persistence, display in UI).
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

    /// Return a short prefix of the session ID (first 8 characters).
    pub fn short_id(&self) -> &str {
        self.id.get(..8).unwrap_or(&self.id)
    }

    /// Return a reference to the underlying memory strategy.
    pub fn memory(&self) -> &dyn Memory {
        &*self.memory
    }

    /// Return a mutable reference to the underlying memory strategy.
    pub fn memory_mut(&mut self) -> &mut dyn Memory {
        &mut *self.memory
    }
}
