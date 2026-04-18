use std::sync::Arc;

use chrono::Local;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::memory::Memory;

/// Shared memory handle — allows middleware and core handler to share memory.
pub type SharedMemory = Arc<Mutex<Box<dyn Memory>>>;

/// A conversation session with a unique ID, title, request counter, and memory.
///
/// Each session owns its own `Memory` instance (behind `Arc<Mutex>` for sharing
/// with the middleware pipeline). Callers access memory via `memory()` /
/// `memory_mut()` for exclusive sync access, or `shared_memory()` for the
/// shareable handle.
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
    /// Conversation memory for this session (shared via Arc<Mutex>).
    memory: SharedMemory,
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
            memory: Arc::new(Mutex::new(memory)),
        }
    }

    /// Increment and return the next request ID.
    #[allow(dead_code)]
    pub fn next_request_id(&mut self) -> u64 {
        self.request_count += 1;
        self.request_count
    }

    /// Return a short prefix of the session ID (first 8 characters).
    pub fn short_id(&self) -> &str {
        self.id.get(..8).unwrap_or(&self.id)
    }

    /// Return the shared memory handle for passing to middleware.
    pub fn shared_memory(&self) -> SharedMemory {
        Arc::clone(&self.memory)
    }

    /// Run a synchronous closure with exclusive access to memory.
    ///
    /// Uses `try_lock()` which succeeds immediately when no async task
    /// holds the lock. This is safe for CLI display and other sync contexts
    /// where the memory is not concurrently accessed.
    ///
    /// Panics if the lock is currently held (should never happen in
    /// normal usage — sync callers only run between async turns).
    #[allow(dead_code)]
    pub fn with_memory<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&dyn Memory) -> R,
    {
        let mem = self.memory.try_lock()
            .expect("Memory lock should not be held during sync access");
        f(&**mem)
    }

    /// Run a synchronous closure with exclusive mutable access to memory.
    #[allow(dead_code)]
    pub fn with_memory_mut<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut dyn Memory) -> R,
    {
        let mut mem = self.memory.try_lock()
            .expect("Memory lock should not be held during sync access");
        f(&mut **mem)
    }

    /// Async version: get exclusive access to memory.
    #[allow(dead_code)]
    pub async fn memory_locked(&self) -> tokio::sync::MutexGuard<'_, Box<dyn Memory>> {
        self.memory.lock().await
    }
}
