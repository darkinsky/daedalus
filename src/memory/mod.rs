pub mod agentic;
pub mod dynamic_cheatsheet;
pub mod persistence;
pub mod sliding_window;
pub mod wiki;

// Re-exports for public API.
// These types are used by other modules (agent, config) and may be
// used by future external consumers. Kept as pub re-exports for
// convenience even if not all are currently referenced.
pub use sliding_window::SlidingWindowFactory;
pub use dynamic_cheatsheet::CheatsheetFactory;
pub use agentic::AgenticFactory;
pub use wiki::WikiFactory;

use std::any::Any;

use crate::llm::{ChatMessage, LlmApi};

/// Default maximum number of messages to send to the LLM.
///
/// Shared by memory strategies that manage their own message list
/// (`CheatsheetMemory`, `AgenticMemory`). Prevents unbounded token
/// growth in long conversations — only the most recent messages
/// within this window are included in `build_messages()`.
pub(crate) const DEFAULT_MAX_MESSAGES: usize = 100;

/// Opaque container for persistent memory state during session migration.
///
/// Each memory strategy can define its own persistent state type.
/// The `Box<dyn Any>` allows type-safe transfer between sessions
/// without coupling the agent layer to any specific memory implementation.
pub struct PersistentState(pub(crate) Box<dyn Any + Send>);

impl PersistentState {
    /// Wrap a value into a persistent state container.
    pub(crate) fn new<T: Any + Send + 'static>(value: T) -> Self {
        Self(Box::new(value))
    }

    /// Attempt to downcast the inner value to a concrete type.
    ///
    /// Returns `Ok(T)` on success, or `Err(Self)` if the type doesn't match.
    pub(crate) fn downcast<T: 'static>(self) -> Result<T, Self> {
        match self.0.downcast::<T>() {
            Ok(boxed) => Ok(*boxed),
            Err(inner) => Err(Self(inner)),
        }
    }
}

/// The Memory trait — unified interface for conversation memory strategies.
///
/// A memory implementation is responsible for:
/// - Storing conversation messages (user inputs and assistant outputs).
/// - Building the message list to send to the LLM on each request.
/// - Reporting whether consolidation is needed (for strategies that support it).
/// - Performing post-turn reflection (for strategies with adaptive memory).
/// - Providing `Any`-based downcasting for advanced operations.
///
/// Currently we have four implementations:
///
/// - **`SlidingWindowMemory`**: Dual-layer memory with sliding window,
///   long-term memory (auto-injected into system prompt), history event
///   log (searchable on demand), and optional Dynamic Cheatsheet.
///   Supports automatic consolidation and post-turn reflection.
///   Best for general use.
///
/// - **`CheatsheetMemory`**: Lightweight adaptive memory backed by a
///   Dynamic Cheatsheet. Accumulates problem-solving insights via LLM
///   reflection after each turn. Best for repetitive task patterns.
///
/// - **`AgenticMemory`**: Knowledge graph memory (A-MEM) with
///   embedding-based retrieval and memory evolution. Stores each
///   response as a memory note and pre-retrieves relevant context
///   for the next turn. Best for long-term knowledge accumulation.
///
/// - **`WikiMemory`**: LLM Wiki memory (Karpathy pattern) with
///   structured Markdown pages, YAML frontmatter, wikilinks, and
///   periodic lint. Compiles conversation knowledge into an
///   Obsidian-compatible wiki. Best for deep knowledge compilation.
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

    /// Export persistent state for migration to a new session.
    ///
    /// Memory strategies that maintain cross-session state (e.g., long-term
    /// memory, history logs) should override this to export that state.
    /// Returns `None` if the strategy has no persistent state to migrate.
    fn take_persistent_state(&mut self) -> Option<PersistentState> {
        None
    }

    /// Import persistent state from a previous session.
    ///
    /// Called after `take_persistent_state` on the old session's memory.
    /// The implementation should downcast the `PersistentState` to its
    /// expected type and restore the data.
    ///
    /// The default implementation logs a warning and discards the state.
    /// Memory strategies that support migration should override this.
    fn restore_persistent_state(&mut self, _state: PersistentState) {
        tracing::warn!(
            strategy = self.strategy_name(),
            "Persistent state discarded — memory strategy does not support migration"
        );
    }

    /// Persist memory state to disk.
    ///
    /// Called during shutdown to save any persistent state (long-term memory,
    /// history logs, etc.) to the workspace. Memory strategies without
    /// persistence support should use the default no-op implementation.
    ///
    /// # Arguments
    /// * `workspace` - The workspace providing canonical file paths.
    fn persist(&self, _workspace: &crate::workspace::Workspace) -> anyhow::Result<()> {
        Ok(())
    }

    /// Perform post-turn reflection to extract reusable insights.
    ///
    /// Called by the agent after each conversation turn. Memory strategies
    /// with adaptive memory (e.g., Dynamic Cheatsheet) override this to
    /// analyze the interaction and accumulate problem-solving knowledge.
    ///
    /// The default implementation is a no-op. Reflection failures should
    /// be handled gracefully (logged, not propagated).
    ///
    /// # Arguments
    /// * `user_input` - The user's message from this turn.
    /// * `assistant_response` - The assistant's response from this turn.
    /// * `llm` - The LLM provider for making reflection calls.
    fn reflect_on_turn<'a>(
        &'a mut self,
        _user_input: &'a str,
        _assistant_response: &'a str,
        _llm: &'a dyn LlmApi,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }

    /// Downcast to a concrete type for advanced operations.
    ///
    /// This enables the agent layer to access strategy-specific features
    /// (e.g., consolidation, history search) without polluting the base trait.
    ///
    /// Reserved for future use when consolidation is triggered externally.
    #[allow(dead_code)]
    fn as_any(&self) -> &dyn Any;

    /// Downcast to a mutable concrete type for advanced operations.
    ///
    /// Reserved for future use when consolidation is triggered externally.
    #[allow(dead_code)]
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// Factory trait for creating `Memory` instances.
///
/// The factory pattern decouples `ChatAgent` from specific memory
/// implementations. Each memory strategy provides its own factory
/// that knows how to create and configure memory instances.
///
/// ## Usage
///
/// ```ignore
/// // Use the default sliding window factory
/// let factory = SlidingWindowFactory;
/// let memory = factory.create_memory("You are a helpful assistant.");
///
/// // Or create a custom factory
/// let agent = ChatAgent::with_memory_factory(llm, config, Box::new(factory));
/// ```
pub trait MemoryFactory: Send + Sync {
    /// Create a new memory instance with the given system prompt.
    fn create_memory(&self, system_prompt: &str) -> Box<dyn Memory>;

    /// Return the strategy name this factory produces (for logging/diagnostics).
    #[allow(dead_code)]
    fn strategy_name(&self) -> &str;
}
