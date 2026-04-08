mod sliding_window;

pub use sliding_window::SlidingWindowMemory;

use crate::llm::ChatMessage;

/// The Memory trait — unified interface for conversation memory strategies.
///
/// A memory implementation is responsible for:
/// - Storing conversation messages (user inputs and assistant outputs).
/// - Building the message list to send to the LLM on each request.
///
/// Currently we have:
/// - `SlidingWindowMemory`: Configurable sliding window that supports both
///   full history (unlimited window) and bounded history (last N turns).
///
/// In the future, more strategies can be added, such as:
/// - Summary-based memory (compress older messages into a summary).
/// - RAG-based memory (retrieve relevant past context).
#[allow(dead_code)]
pub trait Memory: Send + Sync {
    /// Add a user message to memory.
    fn add_user_message(&mut self, content: &str);

    /// Add an assistant message to memory.
    fn add_assistant_message(&mut self, content: &str);

    /// Add tool context to memory (tool call summaries from the current turn).
    ///
    /// This stores tool usage information as an assistant message without
    /// injecting fake user messages. The default implementation prepends
    /// the context to the next assistant message by storing it as-is.
    fn add_tool_context(&mut self, context: &str) {
        self.add_assistant_message(context);
    }

    /// Build the full message list to send to the LLM.
    ///
    /// This includes the system prompt and whatever conversation history
    /// the memory strategy decides to include.
    fn build_messages(&self) -> Vec<ChatMessage>;

    /// Clear all conversation history (but keep the system prompt).
    fn clear(&mut self);

    /// Return the number of conversation turns (user + assistant pairs) stored.
    fn turn_count(&self) -> usize;

    /// Return the memory strategy name (e.g., "sliding_window").
    fn strategy_name(&self) -> &str;
}
