mod config;
mod consolidation;
mod history;
mod long_term;
mod sliding_window;

#[allow(unused_imports)]
pub use {
    config::SlidingWindowConfig,
    consolidation::ConsolidationResult,
    history::HistoryEntry,
    long_term::LongTermMemory,
};
pub use sliding_window::SlidingWindowMemory;

use std::any::Any;

use crate::llm::ChatMessage;

/// The Memory trait — unified interface for conversation memory strategies.
///
/// A memory implementation is responsible for:
/// - Storing conversation messages (user inputs and assistant outputs).
/// - Building the message list to send to the LLM on each request.
/// - Reporting whether consolidation is needed (for strategies that support it).
/// - Providing `Any`-based downcasting for advanced operations.
///
/// Currently we have:
/// - `SlidingWindowMemory`: Dual-layer memory with sliding window, long-term
///   memory (auto-injected into system prompt), and history event log
///   (searchable on demand). Supports automatic consolidation.
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
    /// This includes the system prompt (with long-term memory injected)
    /// and whatever conversation history the memory strategy decides to include.
    fn build_messages(&self) -> Vec<ChatMessage>;

    /// Clear all conversation history (but keep the system prompt,
    /// long-term memory, and history log).
    #[allow(dead_code)]
    fn clear(&mut self);

    /// Check whether consolidation should be triggered.
    ///
    /// Memory strategies that don't support consolidation return `false`.
    fn should_consolidate(&self) -> bool {
        false
    }

    /// Return the number of conversation turns (user + assistant pairs) stored.
    fn turn_count(&self) -> usize;

    /// Return the memory strategy name (e.g., "sliding_window").
    fn strategy_name(&self) -> &str;

    /// Downcast to a concrete type for advanced operations.
    ///
    /// This enables the agent layer to access strategy-specific features
    /// (e.g., consolidation, history search) without polluting the base trait.
    #[allow(dead_code)]
    fn as_any(&self) -> &dyn Any;

    /// Downcast to a mutable concrete type for advanced operations.
    fn as_any_mut(&mut self) -> &mut dyn Any;
}
