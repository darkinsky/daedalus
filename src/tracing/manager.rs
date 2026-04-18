//! TracingManager — multi-backend event dispatcher.

use std::sync::Arc;

use tokio::sync::Mutex;

use super::collector::TracingCollector;
use super::context::TraceContext;
use super::types::{Span, Trace, TraceMetadata};

/// Manages multiple tracing collectors and dispatches events to all of them.
///
/// Thread-safe via `Arc<Mutex<...>>` for the collector list. The manager
/// itself is designed to be shared via `Arc<TracingManager>` across async
/// boundaries (agent, tool loop, subagent runner).
pub struct TracingManager {
    collectors: Mutex<Vec<Box<dyn TracingCollector>>>,
    enabled: bool,
    /// Whether to record full content without truncation.
    full_content: bool,
}

impl TracingManager {
    /// Create a new TracingManager with no collectors.
    pub fn new(enabled: bool, full_content: bool) -> Self {
        Self {
            collectors: Mutex::new(Vec::new()),
            enabled,
            full_content,
        }
    }

    /// Create a disabled (no-op) manager.
    #[allow(dead_code)]
    pub fn disabled() -> Self {
        Self::new(false, false)
    }

    /// Whether tracing is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Whether full content recording is enabled.
    #[allow(dead_code)]
    pub fn full_content(&self) -> bool {
        self.full_content
    }

    /// Register a new backend collector.
    pub async fn add_collector(&self, collector: Box<dyn TracingCollector>) {
        tracing::info!(
            collector = collector.name(),
            "Registered tracing collector"
        );
        self.collectors.lock().await.push(collector);
    }

    /// Start a new trace, returning a `TraceContext` for building the span tree.
    ///
    /// If tracing is disabled, returns a no-op context that silently
    /// discards all span events.
    pub fn start_trace(
        self: &Arc<Self>,
        session_id: &str,
        metadata: TraceMetadata,
    ) -> TraceContext {
        TraceContext::new(Arc::clone(self), session_id, metadata, self.full_content)
    }

    /// Dispatch `on_trace_start` to all collectors.
    pub(super) async fn notify_trace_start(&self, trace: &Trace) {
        if !self.enabled {
            return;
        }
        let collectors = self.collectors.lock().await;
        for c in collectors.iter() {
            c.on_trace_start(trace).await;
        }
    }

    /// Dispatch `on_span_start` to all collectors.
    pub(super) async fn notify_span_start(&self, span: &Span) {
        if !self.enabled {
            return;
        }
        let collectors = self.collectors.lock().await;
        for c in collectors.iter() {
            c.on_span_start(span).await;
        }
    }

    /// Dispatch `on_span_end` to all collectors.
    pub(super) async fn notify_span_end(&self, span: &Span) {
        if !self.enabled {
            return;
        }
        let collectors = self.collectors.lock().await;
        for c in collectors.iter() {
            c.on_span_end(span).await;
        }
    }

    /// Dispatch `on_trace_end` to all collectors.
    pub(super) async fn notify_trace_end(&self, trace: &Trace) {
        if !self.enabled {
            return;
        }
        let collectors = self.collectors.lock().await;
        for c in collectors.iter() {
            c.on_trace_end(trace).await;
        }
    }

    /// Flush all collectors (called on shutdown).
    pub async fn flush(&self) {
        let collectors = self.collectors.lock().await;
        for c in collectors.iter() {
            c.flush().await;
        }
    }
}
