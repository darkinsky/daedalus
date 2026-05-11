//! TracingHook — the injection interface for the tool-calling loop.
//!
//! This module provides a thin wrapper around `TraceContext` that exposes
//! the specific hook points needed by `tool_loop::run_tool_loop` and
//! `ChatAgent::chat`. The hook is passed as an optional parameter,
//! keeping the tracing concern decoupled from the core loop logic.

use std::sync::Arc;

use crate::llm::{ChatMessage, TokenUsage};
use super::context::TraceContext;
use super::types::ToolDetail;

// ── SharedTracingHook ──

/// A shared container for a tracing hook that can be set at runtime.
///
/// Follows the same pattern as `SubagentEventSink`: the REPL (or `ChatAgent`)
/// sets the hook before each chat call so that `SubagentTool` can read it
/// during execution and create subagent spans nested under the main trace.
///
/// The hook is stored behind `RwLock` so it can be updated concurrently.
#[derive(Clone, Default)]
pub struct SharedTracingHook {
    inner: Arc<std::sync::RwLock<Option<Arc<TraceContext>>>>,
}

impl SharedTracingHook {
    /// Create an empty hook (no trace context bound).
    pub fn new() -> Self {
        Self {
            inner: Arc::new(std::sync::RwLock::new(None)),
        }
    }

    /// Replace the current trace context (pass `None` to clear).
    ///
    /// Silently no-ops if the lock is poisoned.
    pub fn set(&self, ctx: Option<Arc<TraceContext>>) {
        if let Ok(mut guard) = self.inner.write() {
            *guard = ctx;
        }
    }

    /// Read the currently bound trace context, if any.
    ///
    /// Returns `None` if the lock is poisoned or no context is set.
    pub fn read(&self) -> Option<Arc<TraceContext>> {
        self.inner.read().ok().and_then(|guard| guard.clone())
    }
}

// ── TracingHook ──

/// A tracing hook that can be injected into the tool-calling loop.
///
/// Wraps a `TraceContext` and provides the specific methods that the
/// loop needs to call at each instrumentation point. The hook is
/// designed to be zero-cost when tracing is disabled (all methods
/// short-circuit on `!is_enabled()`).
///
/// ## Usage
///
/// ```ignore
/// let hook = TracingHook::new(trace_context);
/// // Pass &hook into run_tool_loop or chat_with_tools
/// ```
pub struct TracingHook {
    ctx: Arc<TraceContext>,
}

impl TracingHook {
    /// Create a new tracing hook wrapping the given context.
    pub fn new(ctx: Arc<TraceContext>) -> Self {
        Self { ctx }
    }

    /// Whether tracing is enabled (short-circuit check).
    pub fn is_enabled(&self) -> bool {
        self.ctx.is_enabled()
    }

    /// Get a reference to the underlying trace context.
    #[allow(dead_code)]
    pub fn context(&self) -> &TraceContext {
        &self.ctx
    }

    /// Get a clone of the underlying Arc<TraceContext>.
    ///
    /// Used by `CoreTurnHandler` to set the shared tracing hook for
    /// subagent nested span creation.
    pub fn context_arc(&self) -> Arc<TraceContext> {
        Arc::clone(&self.ctx)
    }

    /// Record that an LLM call is about to start.
    ///
    /// Returns a span guard that should be finished after the response arrives.
    pub async fn on_llm_call_start(
        &self,
        model: &str,
        provider: &str,
        messages: &[ChatMessage],
        available_tools: &[ToolDetail],
    ) -> Option<super::SpanGuard> {
        if !self.is_enabled() {
            return None;
        }
        Some(self.ctx.start_llm_call(model, provider, messages, available_tools).await)
    }

    /// Record that a tool call is about to start.
    #[allow(dead_code)]
    pub async fn on_tool_call_start(
        &self,
        tool_name: &str,
        source: &str,
        arguments: &serde_json::Value,
    ) -> Option<super::SpanGuard> {
        if !self.is_enabled() {
            return None;
        }
        Some(self.ctx.start_tool_call(tool_name, source, arguments).await)
    }

    /// Record that a tool call is about to start, with an explicit parent.
    ///
    /// Use this in parallel dispatch paths to avoid the span stack race.
    /// Call `snapshot_parent_id()` before spawning futures, then pass the
    /// result here.
    pub async fn on_tool_call_start_with_parent(
        &self,
        tool_name: &str,
        source: &str,
        arguments: &serde_json::Value,
        parent_id: Option<String>,
    ) -> Option<super::SpanGuard> {
        if !self.is_enabled() {
            return None;
        }
        Some(self.ctx.start_tool_call_with_parent(tool_name, source, arguments, parent_id).await)
    }

    /// Snapshot the current parent span ID from the stack.
    ///
    /// Call this **before** spawning parallel futures so that all parallel
    /// spans share the same parent.
    pub async fn snapshot_parent_id(&self) -> Option<String> {
        self.ctx.current_parent_id().await
    }

    /// Record that a subagent call is about to start.
    #[allow(dead_code)]
    pub async fn on_subagent_call_start(
        &self,
        agent_name: &str,
        task: &str,
    ) -> Option<super::SpanGuard> {
        if !self.is_enabled() {
            return None;
        }
        Some(self.ctx.start_subagent_call(agent_name, task).await)
    }

    /// Accumulate token usage from an LLM response.
    #[allow(dead_code)]
    pub async fn accumulate_usage(&self, usage: &TokenUsage) {
        if !self.is_enabled() {
            return;
        }
        self.ctx.accumulate_usage(usage).await;
    }
}
