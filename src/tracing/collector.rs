//! TracingCollector trait — the backend interface for tracing exporters.

use async_trait::async_trait;

use super::types::{Span, Trace};

/// The core trait that all tracing backends must implement.
///
/// Each backend receives span lifecycle events and decides how to
/// export/store them. Implementations might write to files, send to
/// OpenTelemetry collectors, post to Langfuse, etc.
///
/// All methods are async to support network-based exporters without
/// blocking the agent's main execution loop.
#[async_trait]
pub trait TracingCollector: Send + Sync {
    /// Called when a new trace begins (user message received).
    ///
    /// The trace will have `started_at` set but `ended_at` will be None.
    async fn on_trace_start(&self, trace: &Trace);

    /// Called when a span starts (LLM call, tool call, or subagent begins).
    ///
    /// The span will have `started_at` set but `ended_at` will be None.
    async fn on_span_start(&self, span: &Span);

    /// Called when a span ends (operation completed).
    ///
    /// The span will have both `started_at` and `ended_at` set, along
    /// with the final `status` and any type-specific result data.
    async fn on_span_end(&self, span: &Span);

    /// Called when the entire trace completes.
    ///
    /// The trace will have `ended_at` set and `total_usage` / `total_elapsed_ms`
    /// populated. All spans in `trace.spans` are finalized.
    async fn on_trace_end(&self, trace: &Trace);

    /// Flush any buffered data to the backend.
    ///
    /// Called on agent shutdown to ensure no data is lost.
    async fn flush(&self);

    /// Human-readable name for this collector (for logging/diagnostics).
    fn name(&self) -> &str;
}
