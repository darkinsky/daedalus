//! Tracing subsystem — structured observability for LLM agent execution.
//!
//! Provides a complete call-chain tracing system that hooks into:
//! - **LLM calls**: Input messages, output content, reasoning, token usage, latency
//! - **Tool calls**: Tool name, arguments, results, success/failure, latency
//! - **Subagent calls**: Agent name, task, nested spans, accumulated usage
//!
//! ## Architecture
//!
//! The tracing system is built around three core abstractions:
//!
//! - [`TracingCollector`]: Backend trait — each exporter implements this to
//!   receive span lifecycle events and export them (file, OTLP, Langfuse, etc.)
//! - [`TracingManager`]: Multi-backend dispatcher — holds all collectors and
//!   fans out events to each one.
//! - [`TraceContext`]: Per-trace handle — provides ergonomic span start/end
//!   methods with automatic parent-child relationship tracking.
//!
//! ## Integration
//!
//! The tracing system integrates with the existing `ToolEvent` callback
//! mechanism but operates independently. `ToolEvent` serves CLI rendering;
//! `TracingCollector` serves observability backends.

mod collector;
pub mod config;
mod context;
pub mod exporters;
mod hook;
mod manager;
mod types;

#[allow(unused_imports)]
pub use collector::TracingCollector;
pub use config::TracingConfig;
pub use context::{SpanGuard, TraceContext};
pub use hook::{SharedTracingHook, TracingHook};
pub use manager::TracingManager;
#[allow(unused_imports)]
pub use types::{
    MessageSummary, Span, SpanStatus, SpanType, SpanValue, ToolCallSummary, Trace,
    TraceMetadata,
};
