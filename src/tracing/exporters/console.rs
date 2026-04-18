//! Console exporter — prints trace summaries to stderr for development.
//!
//! Useful during development to see the trace tree in real time without
//! needing to inspect files or external systems.

use async_trait::async_trait;

use crate::agent_tracing::collector::TracingCollector;
use crate::agent_tracing::config::ConsoleVerbosity;
use crate::agent_tracing::types::{Span, SpanStatus, SpanType, Trace};

/// Console-based tracing collector.
///
/// Prints trace summaries to stderr using ANSI colors for readability.
pub struct ConsoleCollector {
    verbosity: ConsoleVerbosity,
}

impl ConsoleCollector {
    /// Create a new console collector with the given verbosity.
    pub fn new(verbosity: ConsoleVerbosity) -> Self {
        Self { verbosity }
    }
}

#[async_trait]
impl TracingCollector for ConsoleCollector {
    async fn on_trace_start(&self, trace: &Trace) {
        eprintln!(
            "\x1b[36m[TRACE START]\x1b[0m trace_id={} session={} model={} provider={}",
            &trace.trace_id[..8],
            &trace.session_id[..8.min(trace.session_id.len())],
            trace.metadata.model,
            trace.metadata.provider,
        );
    }

    async fn on_span_start(&self, span: &Span) {
        if matches!(self.verbosity, ConsoleVerbosity::Full) {
            let indent = if span.parent_span_id.is_some() { "  " } else { "" };
            let type_label = span_type_label(&span.span_type);
            eprintln!(
                "{}\x1b[33m[SPAN START]\x1b[0m {} \"{}\"",
                indent, type_label, span.name,
            );
        }
    }

    async fn on_span_end(&self, span: &Span) {
        if matches!(self.verbosity, ConsoleVerbosity::Full) {
            let indent = if span.parent_span_id.is_some() { "  " } else { "" };
            let type_label = span_type_label(&span.span_type);
            let status_icon = match &span.status {
                SpanStatus::Ok => "\x1b[32m✓\x1b[0m",
                SpanStatus::Error(_) => "\x1b[31m✗\x1b[0m",
                SpanStatus::Running => "\x1b[33m…\x1b[0m",
            };
            let elapsed = span.elapsed_ms.map(|ms| format!(" ({}ms)", ms)).unwrap_or_default();
            let extra = span_end_extra(span);
            eprintln!(
                "{}{} \x1b[33m[SPAN END]\x1b[0m {} \"{}\"{} {}",
                indent, status_icon, type_label, span.name, elapsed, extra,
            );
        }
    }

    async fn on_trace_end(&self, trace: &Trace) {
        let elapsed = trace.total_elapsed_ms.map(|ms| format!("{}ms", ms)).unwrap_or_else(|| "?".to_string());
        let tokens = trace
            .total_usage
            .as_ref()
            .and_then(|u| u.total_tokens)
            .map(|t| format!("{} tokens", t))
            .unwrap_or_else(|| "? tokens".to_string());
        let span_count = trace.spans.len();

        eprintln!(
            "\x1b[36m[TRACE END]\x1b[0m trace_id={} elapsed={} usage={} spans={}",
            &trace.trace_id[..8],
            elapsed,
            tokens,
            span_count,
        );
    }

    async fn flush(&self) {
        // Nothing to flush for console output
    }

    fn name(&self) -> &str {
        "console"
    }
}

/// Get a short label for the span type.
fn span_type_label(span_type: &SpanType) -> &'static str {
    match span_type {
        SpanType::AgentTurn { .. } => "AGENT",
        SpanType::LlmCall { .. } => "LLM",
        SpanType::ToolCall { .. } => "TOOL",
        SpanType::SubagentCall { .. } => "SUBAGENT",
    }
}

/// Get extra info to display at span end.
fn span_end_extra(span: &Span) -> String {
    match &span.span_type {
        SpanType::LlmCall { usage, tool_calls, .. } => {
            let tokens = usage
                .as_ref()
                .and_then(|u| u.total_tokens)
                .map(|t| format!("{}tok", t))
                .unwrap_or_default();
            let tools = if tool_calls.is_empty() {
                String::new()
            } else {
                format!(" → {} tool calls", tool_calls.len())
            };
            format!("{}{}", tokens, tools)
        }
        SpanType::ToolCall { tool_name, success, .. } => {
            if *success {
                format!("{} ✓", tool_name)
            } else {
                format!("{} ✗", tool_name)
            }
        }
        SpanType::SubagentCall { agent_name, tool_rounds, .. } => {
            format!("{} ({} rounds)", agent_name, tool_rounds)
        }
        _ => String::new(),
    }
}
