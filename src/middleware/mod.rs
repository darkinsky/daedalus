//! Middleware Pipeline — composable, onion-model processing for agent turns and tool calls.
//!
//! This module provides a flexible middleware system inspired by Tower/Axum/Koa that
//! decouples cross-cutting concerns (tracing, logging, memory, permissions, cost tracking)
//! from the core agent logic.
//!
//! ## Architecture
//!
//! Two independent pipelines handle different granularities:
//!
//! - **Turn pipeline**: wraps the entire user-input → response cycle.
//! - **Tool pipeline**: wraps each individual tool call execution.
//!
//! Each pipeline is a stack of middleware that forms an onion:
//!
//! ```text
//! Request → [Outer MW] → [Middle MW] → [Inner MW] → Core → [Inner MW] → [Middle MW] → [Outer MW] → Response
//! ```
//!
//! Every middleware can:
//! - Inspect/modify the request before delegating
//! - Delegate to the next layer via `next.run()`
//! - Inspect/modify the response after delegation
//! - Short-circuit by returning early without calling `next`
//!
//! ## Inter-middleware communication
//!
//! The [`Extensions`] type provides a type-safe, key-value store that travels with each
//! request/response. Middleware can insert typed data (e.g., `TraceContext`) that downstream
//! middleware or the core handler can read.

pub mod pipeline;
pub mod builtin;
pub mod config;

use async_trait::async_trait;

use crate::llm::{ChatMessage, ChatResponse, ToolCall, ToolResponse, ToolRound, TokenUsage};

// ════════════════════════════════════════════════════════════
// Extensions — type-safe metadata bag for inter-middleware data
// ════════════════════════════════════════════════════════════

/// A type-erased, type-safe key-value store for passing data between middleware layers.
///
/// Inspired by `http::Extensions`. Each middleware can insert typed data that
/// downstream middleware or the core handler can read.
///
/// # Example
///
/// ```ignore
/// // In TracingMiddleware (before):
/// request.extensions.insert(Arc::new(trace_context));
///
/// // In CoreHandler:
/// if let Some(ctx) = request.extensions.get::<Arc<TraceContext>>() { ... }
/// ```
pub struct Extensions {
    map: std::collections::HashMap<std::any::TypeId, Box<dyn std::any::Any + Send + Sync>>,
}

impl Extensions {
    /// Create an empty extensions map.
    pub fn new() -> Self {
        Self {
            map: std::collections::HashMap::new(),
        }
    }

    /// Insert a typed value. Replaces any previous value of the same type.
    pub fn insert<T: Send + Sync + 'static>(&mut self, val: T) {
        self.map
            .insert(std::any::TypeId::of::<T>(), Box::new(val));
    }

    /// Get a reference to a typed value, if it exists.
    pub fn get<T: 'static>(&self) -> Option<&T> {
        self.map
            .get(&std::any::TypeId::of::<T>())
            .and_then(|b| b.downcast_ref())
    }

    /// Get a mutable reference to a typed value, if it exists.
    #[allow(dead_code)]
    pub fn get_mut<T: 'static>(&mut self) -> Option<&mut T> {
        self.map
            .get_mut(&std::any::TypeId::of::<T>())
            .and_then(|b| b.downcast_mut())
    }

    /// Remove and return a typed value, if it exists.
    #[allow(dead_code)]
    pub fn remove<T: 'static>(&mut self) -> Option<T> {
        self.map
            .remove(&std::any::TypeId::of::<T>())
            .and_then(|b| b.downcast().ok())
            .map(|b| *b)
    }

    /// Check if a typed value exists.
    #[allow(dead_code)]
    pub fn contains<T: 'static>(&self) -> bool {
        self.map.contains_key(&std::any::TypeId::of::<T>())
    }
}

impl Default for Extensions {
    fn default() -> Self {
        Self::new()
    }
}

// ════════════════════════════════════════════════════════════
// Turn-level Middleware (对话轮次级)
// ════════════════════════════════════════════════════════════

/// Context passed through the turn middleware pipeline.
///
/// Middleware can read/mutate these fields before passing to the next layer.
pub struct TurnRequest<'a> {
    /// User's raw input text.
    pub user_input: &'a str,
    /// Messages built from memory (MemoryMiddleware fills this).
    pub messages: Vec<ChatMessage>,
    /// Mutable metadata bag for inter-middleware communication.
    pub extensions: Extensions,
}

/// The response flowing back through the pipeline (innermost → outermost).
pub struct TurnResponse {
    /// The LLM's final response.
    pub chat_response: ChatResponse,
    /// Tool call history from this turn (if any).
    pub tool_history: Vec<ToolRound>,
    /// Accumulated token usage across all LLM calls in this turn.
    #[allow(dead_code)]
    pub usage: TokenUsage,
    /// Mutable metadata bag (can carry data back out through the pipeline).
    #[allow(dead_code)]
    pub extensions: Extensions,
}

/// The "next" handler in the turn pipeline chain.
///
/// Each middleware receives a `&dyn TurnNext` and can:
/// - Call `next.run(request)` to delegate to the next layer
/// - Return early without calling `next` (short-circuit)
/// - Modify the request before calling, or the response after
#[async_trait]
pub trait TurnNext: Send + Sync {
    /// Execute the next layer in the pipeline.
    async fn run(&self, request: TurnRequest<'_>) -> anyhow::Result<TurnResponse>;
}

/// A turn-level middleware — processes the full user-input → response cycle.
///
/// Implements the onion model: `before → next.run() → after`.
///
/// # Example
///
/// ```ignore
/// struct LoggingMiddleware;
///
/// #[async_trait]
/// impl TurnMiddleware for LoggingMiddleware {
///     async fn handle<'a>(
///         &self,
///         request: TurnRequest<'a>,
///         next: &dyn TurnNext,
///     ) -> anyhow::Result<TurnResponse> {
///         tracing::info!(input = request.user_input, "Turn started");
///         let response = next.run(request).await?;
///         tracing::info!(output_len = response.chat_response.content.len(), "Turn done");
///         Ok(response)
///     }
///
///     fn name(&self) -> &str { "logging" }
/// }
/// ```
#[async_trait]
pub trait TurnMiddleware: Send + Sync {
    /// Process a turn request, optionally delegating to the next layer.
    async fn handle<'a>(
        &self,
        request: TurnRequest<'a>,
        next: &dyn TurnNext,
    ) -> anyhow::Result<TurnResponse>;

    /// Human-readable name for diagnostics and configuration.
    fn name(&self) -> &str;
}

// ════════════════════════════════════════════════════════════
// Tool-level Middleware (工具调用级)
// ════════════════════════════════════════════════════════════

/// Context for a single tool call flowing through the tool pipeline.
pub struct ToolRequest {
    /// The tool call from the LLM.
    pub call: ToolCall,
    /// Source of the tool ("built-in", "mcp:<server>").
    pub source: String,
    /// Round number (1-based) within the current turn.
    pub round: usize,
    /// Mutable metadata bag for inter-middleware communication.
    pub extensions: Extensions,
}

/// The "next" handler in the tool pipeline chain.
#[async_trait]
pub trait ToolNext: Send + Sync {
    /// Execute the next layer in the pipeline.
    async fn run(&self, request: ToolRequest) -> ToolResponse;
}

/// A tool-level middleware — wraps individual tool call execution.
///
/// # Use Cases
///
/// - **Permission control**: reject tool calls based on rules
/// - **Tracing**: wrap tool execution in a tracing span
/// - **Rate limiting**: throttle tool calls per second
/// - **Audit logging**: record every tool invocation
/// - **Input sanitization**: validate/transform tool arguments
/// - **Output filtering**: redact sensitive data from tool results
#[async_trait]
pub trait ToolMiddleware: Send + Sync {
    /// Process a tool call, optionally delegating to the next layer.
    async fn handle(
        &self,
        request: ToolRequest,
        next: &dyn ToolNext,
    ) -> ToolResponse;

    /// Human-readable name for diagnostics and configuration.
    fn name(&self) -> &str;
}

// ════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extensions_insert_and_get() {
        let mut ext = Extensions::new();
        ext.insert(42u32);
        ext.insert("hello".to_string());

        assert_eq!(ext.get::<u32>(), Some(&42));
        assert_eq!(ext.get::<String>(), Some(&"hello".to_string()));
        assert_eq!(ext.get::<bool>(), None);
    }

    #[test]
    fn test_extensions_overwrite() {
        let mut ext = Extensions::new();
        ext.insert(1u32);
        ext.insert(2u32);
        assert_eq!(ext.get::<u32>(), Some(&2));
    }

    #[test]
    fn test_extensions_remove() {
        let mut ext = Extensions::new();
        ext.insert(42u32);
        assert_eq!(ext.remove::<u32>(), Some(42));
        assert_eq!(ext.get::<u32>(), None);
    }

    #[test]
    fn test_extensions_contains() {
        let mut ext = Extensions::new();
        assert!(!ext.contains::<u32>());
        ext.insert(42u32);
        assert!(ext.contains::<u32>());
    }

    #[test]
    fn test_extensions_get_mut() {
        let mut ext = Extensions::new();
        ext.insert(vec![1, 2, 3]);
        if let Some(v) = ext.get_mut::<Vec<i32>>() {
            v.push(4);
        }
        assert_eq!(ext.get::<Vec<i32>>(), Some(&vec![1, 2, 3, 4]));
    }
}
