pub mod ace;
pub mod agentic;
pub mod dynamic_cheatsheet;
pub mod mempalace;
pub mod persistence;
pub mod sliding_window;
pub mod wiki;

// Re-exports for public API.
// These types are used by other modules (agent, config) and may be
// used by future external consumers. Kept as pub re-exports for
// convenience even if not all are currently referenced.
pub use ace::AceFactory;
pub use sliding_window::SlidingWindowFactory;
pub use dynamic_cheatsheet::CheatsheetFactory;
pub use agentic::AgenticFactory;
pub use mempalace::MemPalaceFactory;
pub use wiki::WikiFactory;



use std::any::Any;

use crate::llm::{ChatMessage, LlmApi};

// ── Shared parsing utilities ──

/// Approximate characters per token for budget estimation.
///
/// Shared by memory strategies that render content for system prompt injection
/// and need to respect a token budget (Playbook and DynamicCheatsheet).
pub(crate) const CHARS_PER_TOKEN: usize = 4;

/// Truncate rendered text to fit within a token budget, cutting at a line boundary.
///
/// If the text fits within `max_tokens * CHARS_PER_TOKEN` characters, it is
/// returned as-is. Otherwise, it is truncated at the last newline before the
/// budget limit, and `truncation_suffix` is appended.
///
/// Shared by `Playbook::to_markdown` and `DynamicCheatsheet::to_markdown`.
pub(crate) fn truncate_to_token_budget(text: String, max_tokens: usize, truncation_suffix: &str) -> String {
    let max_chars = max_tokens * CHARS_PER_TOKEN;
    // Use char count (not byte length) for correct multi-byte character handling.
    if text.chars().count() <= max_chars {
        return text;
    }

    // Truncate at a line boundary to avoid cutting mid-entry.
    let truncated: String = text.chars().take(max_chars).collect();
    let cut_point = truncated.rfind('\n').unwrap_or(truncated.len());
    format!("{}\n\n{}", &truncated[..cut_point], truncation_suffix)
}

/// Strip a directive prefix (e.g., `NEW:`, `ADD:`, `UPDATE:`) case-insensitively.
///
/// Shared by memory strategies that parse structured LLM reflection responses
/// (Dynamic Cheatsheet and ACE). Returns the remainder of the line after the
/// prefix, or `None` if the prefix doesn't match.
pub(crate) fn strip_directive_prefix<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    if line.len() >= prefix.len()
        && line[..prefix.len()].eq_ignore_ascii_case(prefix)
    {
        Some(&line[prefix.len()..])
    } else {
        None
    }
}

// ── Shared message buffer ──

/// Reusable message buffer for memory strategies that manage their own
/// conversation history with a sliding window.
///
/// Encapsulates the common pattern shared by `CheatsheetMemory`, `AgenticMemory`,
/// `WikiMemory`, and `AceMemory` — all of which maintain a `Vec<ChatMessage>`
/// with a `max_messages` window limit.
///
/// Using composition (embedding `MessageBuffer` as a field) rather than
/// inheritance keeps each strategy's `Memory` impl in full control while
/// eliminating ~30 lines of duplicated boilerplate per strategy.
pub(crate) struct MessageBuffer {
    messages: Vec<ChatMessage>,
    max_messages: usize,
}

impl MessageBuffer {
    /// Create a new buffer with the given window size.
    pub fn new(max_messages: usize) -> Self {
        Self {
            messages: Vec::new(),
            max_messages,
        }
    }

    /// Append a user message.
    pub fn add_user(&mut self, content: &str) {
        self.messages.push(ChatMessage::user(content));
    }

    /// Append an assistant message.
    pub fn add_assistant(&mut self, content: &str) {
        self.messages.push(ChatMessage::assistant(content));
    }

    /// Return the windowed slice of messages (most recent `max_messages`).
    pub fn windowed(&self) -> &[ChatMessage] {
        if self.messages.len() <= self.max_messages {
            &self.messages[..]
        } else {
            &self.messages[self.messages.len() - self.max_messages..]
        }
    }

    /// Return the number of conversation turns (user + assistant pairs).
    #[allow(dead_code)]
    pub fn turn_count(&self) -> usize {
        self.messages.len() / 2
    }

    /// Clear all messages.
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.messages.clear();
    }

    /// Build the full message list with a system prompt prepended.
    pub fn build_messages_with_system(&self, system_prompt: String) -> Vec<ChatMessage> {
        let window = self.windowed();
        let mut messages = Vec::with_capacity(1 + window.len());
        messages.push(ChatMessage::system(system_prompt));
        messages.extend(window.iter().cloned());
        messages
    }
}

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
/// Currently we have five implementations:
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
///
/// - **`AceMemory`**: ACE (Agentic Context Engineering) memory with
///   an evolving Playbook of structured sections and bullets. Uses a
///   deterministic Curator to merge LLM-produced delta entries, preventing
///   context collapse. Best for strategy accumulation and self-improving context.
///
/// - **`MemPalaceMemory`**: Memory Palace (Method of Loci) with spatial
///   organization into Wings/Rooms/Halls, ChromaDB vector storage,
///   knowledge graph triples, and cross-wing tunnels. Stores everything
///   verbatim and makes it findable. Best for cross-project/cross-person
///   long-term memory navigation.
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
    #[allow(dead_code)]
    fn should_consolidate(&self) -> bool {
        false
    }

    /// Return the number of conversation turns (user + assistant pairs) stored.
    #[allow(dead_code)]
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

// ── Memory factory selection ──

/// Create the appropriate memory factory based on the configured strategy and workspace.
///
/// This function centralizes the strategy → factory mapping that was previously
/// embedded inside `ChatAgent`, improving separation of concerns: the agent layer
/// no longer needs to know about individual memory strategy factories.
///
/// Each strategy has its own factory that knows how to load persisted state from
/// the workspace and create configured memory instances.
///
/// # Fallback behavior
///
/// Strategies that require an embedding provider (Agentic, MemPalace) will
/// gracefully fall back to `SlidingWindow` if the embedding configuration is
/// missing or invalid, rather than panicking.
pub fn create_memory_factory(
    strategy: &crate::config::MemoryStrategy,
    embedding_config: &crate::config::EmbeddingConfig,
    workspace: &crate::workspace::Workspace,
) -> Box<dyn MemoryFactory> {
    use crate::config::MemoryStrategy;

    match strategy {
        MemoryStrategy::SlidingWindow => sliding_window_factory(workspace),
        MemoryStrategy::DynamicCheatsheet => {
            let factory = CheatsheetFactory::with_workspace(
                workspace.cheatsheet_path(),
            );
            Box::new(factory)
        }
        MemoryStrategy::Agentic => {
            match embedding_config.create_provider() {
                Ok(embedder) => {
                    let factory = AgenticFactory::with_workspace(
                        workspace.agentic_notes_path(),
                        embedder,
                    );
                    Box::new(factory)
                }
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "Failed to create embedding provider for agentic memory, \
                         falling back to sliding_window"
                    );
                    sliding_window_factory(workspace)
                }
            }
        }
        MemoryStrategy::Wiki => {
            match embedding_config.create_provider() {
                Ok(embedder) => {
                    tracing::info!(
                        "Wiki memory initialized with embedding provider (enhanced retrieval)"
                    );
                    let factory = WikiFactory::with_workspace(
                        workspace.wiki_dir(),
                        embedder,
                    );
                    Box::new(factory)
                }
                Err(e) => {
                    tracing::info!(
                        error = %e,
                        "No embedding provider configured for wiki memory, \
                         using keyword-only retrieval mode"
                    );
                    let factory = WikiFactory::with_workspace_only(
                        workspace.wiki_dir(),
                    );
                    Box::new(factory)
                }
            }
        }
        MemoryStrategy::Ace => {
            let factory = AceFactory::with_workspace(
                workspace.ace_playbook_path(),
            );
            Box::new(factory)
        }
        MemoryStrategy::MemPalace => {
            match embedding_config.create_provider() {
                Ok(embedder) => {
                    tracing::info!(
                        "MemPalace memory initialized with embedding provider and ChromaDB"
                    );
                    let factory = MemPalaceFactory::with_workspace(
                        workspace.mempalace_dir(),
                        embedder,
                    );
                    Box::new(factory)
                }
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "MemPalace memory requires embedding configuration, \
                         falling back to sliding_window. \
                         Please configure `embedding` section in daedalus.yaml."
                    );
                    sliding_window_factory(workspace)
                }
            }
        }
    }
}

/// Create a sliding-window memory factory with workspace persistence.
///
/// This is the default factory and also the fallback when other strategies
/// fail to initialize (e.g., missing embedding provider).
fn sliding_window_factory(workspace: &crate::workspace::Workspace) -> Box<dyn MemoryFactory> {
    Box::new(SlidingWindowFactory::with_workspace(
        workspace.long_term_memory_path(),
        workspace.history_log_path(),
    ))
}
