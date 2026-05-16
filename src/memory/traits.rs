//! Core memory traits and types.
//!
//! Defines the `Memory` trait (unified interface for all memory strategies),
//! `MemoryFactory` trait, and `PersistentState` opaque container.

use std::any::Any;

use crate::llm::{ChatMessage, LlmApi};
use super::ContextPressureLevel;

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
/// Currently we have six implementations:
///
/// - **`SlidingWindowMemory`**: Dual-layer with sliding window + long-term memory.
/// - **`CheatsheetMemory`**: Lightweight adaptive memory backed by Dynamic Cheatsheet.
/// - **`AgenticMemory`**: Knowledge graph memory (A-MEM) with embedding retrieval.
/// - **`WikiMemory`**: LLM Wiki memory (Karpathy pattern) with Markdown pages.
/// - **`AceMemory`**: ACE memory with an evolving Playbook of structured sections.
/// - **`MemPalaceMemory`**: Memory Palace with spatial organization and vector storage.
pub trait Memory: Send + Sync {
    fn add_user_message(&mut self, content: &str);
    fn add_assistant_message(&mut self, content: &str);

    fn add_tool_context(&mut self, context: &str) {
        self.add_assistant_message(context);
    }

    fn build_messages(&self) -> Vec<ChatMessage>;

    fn notify_cache_status(&mut self, _cached_tokens: u64) {}

    #[allow(dead_code)]
    fn clear(&mut self);

    #[allow(dead_code)]
    fn should_consolidate(&self) -> bool { false }

    fn search_history(&self, _query: &str, _limit: Option<usize>) -> Vec<String> {
        Vec::new()
    }

    #[allow(dead_code)]
    fn turn_count(&self) -> usize;

    fn strategy_name(&self) -> &str;

    fn take_persistent_state(&mut self) -> Option<PersistentState> { None }

    fn restore_persistent_state(&mut self, _state: PersistentState) {
        tracing::warn!(
            strategy = self.strategy_name(),
            "Persistent state discarded — memory strategy does not support migration"
        );
    }

    fn persist(&self, _workspace: &crate::workspace::Workspace) -> anyhow::Result<()> {
        Ok(())
    }

    fn reflect_on_turn<'a>(
        &'a mut self,
        _user_input: &'a str,
        _assistant_response: &'a str,
        _llm: &'a dyn LlmApi,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }

    fn maybe_consolidate<'a>(
        &'a mut self,
        _llm: &'a dyn LlmApi,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }

    #[allow(dead_code)]
    fn should_compact(&self) -> bool { false }

    fn context_pressure_level(&self) -> ContextPressureLevel {
        ContextPressureLevel::Normal
    }

    fn maybe_compact<'a>(
        &'a mut self,
        _llm: &'a dyn LlmApi,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }

    fn compact<'a>(
        &'a mut self,
        _llm: &'a dyn LlmApi,
        _instruction: Option<&'a str>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<String>> + Send + 'a>> {
        Box::pin(async {
            Ok("Compact is not supported by this memory strategy.".to_string())
        })
    }

    fn compact_range<'a>(
        &'a mut self,
        _llm: &'a dyn LlmApi,
        _instruction: Option<&'a str>,
        _range: (usize, usize),
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<String>> + Send + 'a>> {
        Box::pin(async {
            Ok("Partial compact is not supported by this memory strategy.".to_string())
        })
    }
}

/// Factory trait for creating `Memory` instances.
///
/// The factory pattern decouples `ChatAgent` from specific memory
/// implementations. Each memory strategy provides its own factory
/// that knows how to create and configure memory instances.
pub trait MemoryFactory: Send + Sync {
    fn create_memory(&self, system_prompt: &str) -> Box<dyn Memory>;

    #[allow(dead_code)]
    fn strategy_name(&self) -> &str;
}
