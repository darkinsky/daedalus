//! File exporter — writes traces as JSON Lines to disk.
//!
//! Each trace is serialized as a single JSON object and appended to a
//! date-partitioned file (e.g., `traces/2024-01-15.jsonl`). This format
//! is easy to grep, tail, and ingest into log aggregation systems.

use std::path::PathBuf;

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::llm::TokenUsage;
use crate::agent_tracing::collector::TracingCollector;
use crate::agent_tracing::config::{ContentFlags, FileFormat};
use crate::agent_tracing::types::{Span, SpanStatus, SpanType, Trace};

/// File-based tracing collector.
///
/// Writes completed traces to JSON Lines files, one file per day.
/// Spans are buffered in memory until the trace completes, then the
/// entire trace is written as a single JSON object.
pub struct FileCollector {
    /// Directory to write trace files to.
    output_dir: PathBuf,
    /// Output format (jsonl or pretty).
    format: FileFormat,
    /// Buffer of pending writes (flushed on trace_end or explicit flush).
    #[allow(dead_code)]
    buffer: Mutex<Vec<String>>,
    /// Resolved content recording flags.
    flags: ContentFlags,
}

impl FileCollector {
    /// Create a new file collector writing to the given directory.
    pub fn new(output_dir: PathBuf, format: FileFormat, flags: ContentFlags) -> Self {
        // Ensure the output directory exists
        if let Err(e) = std::fs::create_dir_all(&output_dir) {
            tracing::warn!(
                path = %output_dir.display(),
                error = %e,
                "Failed to create trace output directory"
            );
        }

        Self {
            output_dir,
            format,
            buffer: Mutex::new(Vec::new()),
            flags,
        }
    }

    /// Get the output file path for the current date.
    fn output_path(&self) -> PathBuf {
        let date = chrono::Utc::now().format("%Y-%m-%d");
        match self.format {
            FileFormat::Jsonl => self.output_dir.join(format!("{}.jsonl", date)),
            FileFormat::Pretty => self.output_dir.join(format!("{}.json", date)),
            FileFormat::Yaml => self.output_dir.join(format!("{}.yaml", date)),
        }
    }

    /// Write a line to the output file.
    fn write_line(&self, content: &str) {
        let path = self.output_path();
        use std::io::Write;
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(mut file) => {
                if let Err(e) = writeln!(file, "{}", content) {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "Failed to write trace to file"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Failed to open trace file for writing"
                );
            }
        }
    }
}

#[async_trait]
impl TracingCollector for FileCollector {
    async fn on_trace_start(&self, _trace: &Trace) {
        // No-op: we write the complete trace on trace_end
    }

    async fn on_span_start(&self, _span: &Span) {
        // No-op: spans are included in the trace on trace_end
    }

    async fn on_span_end(&self, _span: &Span) {
        // No-op: spans are included in the trace on trace_end
    }

    async fn on_trace_end(&self, trace: &Trace) {
        match self.format {
            FileFormat::Jsonl => {
                let json = serialize_trace(trace);
                // Compact single-line JSON
                self.write_line(&json);
            }
            FileFormat::Pretty => {
                let json = serialize_trace(trace);
                // Pretty-printed JSON (still appended to the same file)
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&json) {
                    if let Ok(pretty) = serde_json::to_string_pretty(&value) {
                        self.write_line(&pretty);
                    } else {
                        self.write_line(&json);
                    }
                } else {
                    self.write_line(&json);
                }
            }
            FileFormat::Yaml => {
                // Human-readable YAML-like indented tree format
                let yaml = serialize_trace_yaml(trace, self.flags);
                self.write_line(&yaml);
            }
        }
    }

    async fn flush(&self) {
        // All writes are synchronous and immediate, nothing to flush.
    }

    fn name(&self) -> &str {
        "file"
    }
}

/// Serialize a Trace into a JSON string.
fn serialize_trace(trace: &Trace) -> String {
    let spans_json: Vec<serde_json::Value> = trace
        .spans
        .iter()
        .map(serialize_span)
        .collect();

    let json = serde_json::json!({
        "trace_id": trace.trace_id,
        "session_id": trace.session_id,
        "started_at": trace.started_at.to_rfc3339(),
        "ended_at": trace.ended_at.map(|t| t.to_rfc3339()),
        "total_elapsed_ms": trace.total_elapsed_ms,
        "total_usage": serialize_usage(trace.total_usage.as_ref()),
        "metadata": {
            "agent_name": trace.metadata.agent_name,
            "model": trace.metadata.model,
            "provider": trace.metadata.provider,
        },
        "spans": spans_json,
    });

    serde_json::to_string(&json).unwrap_or_else(|_| "{}".to_string())
}

/// Serialize a Span into a JSON value.
fn serialize_span(span: &Span) -> serde_json::Value {
    let status_str = match &span.status {
        SpanStatus::Running => "running",
        SpanStatus::Ok => "ok",
        SpanStatus::Error(_) => "error",
    };
    let error_msg = match &span.status {
        SpanStatus::Error(msg) => Some(msg.as_str()),
        _ => None,
    };

    let mut json = serde_json::json!({
        "span_id": span.span_id,
        "parent_span_id": span.parent_span_id,
        "trace_id": span.trace_id,
        "name": span.name,
        "started_at": span.started_at.to_rfc3339(),
        "ended_at": span.ended_at.map(|t| t.to_rfc3339()),
        "elapsed_ms": span.elapsed_ms,
        "status": status_str,
        "error": error_msg,
    });

    // Add type-specific fields
    match &span.span_type {
        SpanType::AgentTurn { user_input, output } => {
            json["type"] = serde_json::json!("agent_turn");
            json["user_input"] = serde_json::json!(user_input);
            json["output"] = serde_json::json!(output);
        }
        SpanType::LlmCall {
            model,
            provider,
            input_messages,
            available_tools,
            output_content,
            reasoning_content,
            tool_calls,
            usage,
        } => {
            json["type"] = serde_json::json!("llm_call");
            json["model"] = serde_json::json!(model);
            json["provider"] = serde_json::json!(provider);
            json["input_message_count"] = serde_json::json!(input_messages.len());
            json["input_messages"] = serde_json::json!(
                input_messages.iter().map(|m| serde_json::json!({
                    "role": m.role,
                    "content_preview": m.content_preview,
                    "content_len": m.content_len,
                })).collect::<Vec<_>>()
            );
            if !available_tools.is_empty() {
                json["available_tools"] = serde_json::json!(
                    available_tools.iter().map(|t| serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "parameters_schema": t.parameters_schema,
                    })).collect::<Vec<_>>()
                );
            }
            json["output_content"] = serde_json::json!(output_content);
            json["reasoning_content"] = serde_json::json!(reasoning_content);
            json["tool_calls"] = serde_json::json!(
                tool_calls.iter().map(|tc| serde_json::json!({
                    "function_name": tc.function_name,
                    "arguments_preview": tc.arguments_preview,
                })).collect::<Vec<_>>()
            );
            json["usage"] = serialize_usage(usage.as_ref());
        }
        SpanType::ToolCall {
            tool_name,
            source,
            arguments,
            result,
            success,
        } => {
            json["type"] = serde_json::json!("tool_call");
            json["tool_name"] = serde_json::json!(tool_name);
            json["source"] = serde_json::json!(source);
            json["arguments"] = arguments.clone();
            json["result"] = serde_json::json!(result);
            json["success"] = serde_json::json!(success);
        }
        SpanType::SubagentCall {
            agent_name,
            task,
            model,
            result,
            usage,
            tool_rounds,
        } => {
            json["type"] = serde_json::json!("subagent_call");
            json["agent_name"] = serde_json::json!(agent_name);
            json["task"] = serde_json::json!(task);
            json["model"] = serde_json::json!(model);
            json["result"] = serde_json::json!(result);
            json["usage"] = serialize_usage(usage.as_ref());
            json["tool_rounds"] = serde_json::json!(tool_rounds);
        }
    }

    // Add custom attributes if any
    if !span.attributes.is_empty() {
        let attrs: serde_json::Map<String, serde_json::Value> = span
            .attributes
            .iter()
            .map(|(k, v)| {
                let val = match v {
                    super::super::types::SpanValue::String(s) => serde_json::json!(s),
                    super::super::types::SpanValue::Int(i) => serde_json::json!(i),
                    super::super::types::SpanValue::Float(f) => serde_json::json!(f),
                    super::super::types::SpanValue::Bool(b) => serde_json::json!(b),
                };
                (k.clone(), val)
            })
            .collect();
        json["attributes"] = serde_json::Value::Object(attrs);
    }

    json
}

/// Serialize TokenUsage to JSON value.
fn serialize_usage(usage: Option<&TokenUsage>) -> serde_json::Value {
    match usage {
        Some(u) => serde_json::json!({
            "prompt_tokens": u.prompt_tokens,
            "completion_tokens": u.completion_tokens,
            "total_tokens": u.total_tokens,
            "cached_tokens": u.cached_tokens,
        }),
        None => serde_json::Value::Null,
    }
}

// ── YAML-like human-readable format ──

/// Serialize a Trace into a YAML-like indented text format.
///
/// Produces output like:
/// ```text
/// ---
/// trace: abc-123
///   session: sess-456
///   started_at: 2024-01-15T10:30:00Z
///   ended_at: 2024-01-15T10:30:12Z
///   elapsed: 12300ms
///   model: claude-sonnet-4-6
///   provider: Venus
///   usage:
///     prompt_tokens: 4200
///     completion_tokens: 800
///     total_tokens: 5000
///   spans:
///     - [agent_turn] "user asked to fix a bug" (12300ms, ok)
///       - [llm_call] claude-sonnet-4-6 (1200ms, ok)
///           input_messages: 3
///           output: "I'll read the file first..."
///           usage: 1200/300/1500
///       - [tool_call] read_file (50ms, ok)
///           source: built-in
///           arguments: {"path": "src/main.rs"}
///       - [subagent_call] code-reviewer (5200ms, ok)
///           model: claude-haiku
///           tool_rounds: 3
///           usage: 600/200/800
/// ```
fn serialize_trace_yaml(trace: &Trace, flags: ContentFlags) -> String {
    let mut out = String::new();

    out.push_str("---\n");
    out.push_str(&format!("trace: {}\n", trace.trace_id));
    out.push_str(&format!("  session: {}\n", trace.session_id));
    out.push_str(&format!("  started_at: {}\n", trace.started_at.to_rfc3339()));
    if let Some(ended) = trace.ended_at {
        out.push_str(&format!("  ended_at: {}\n", ended.to_rfc3339()));
    }
    if let Some(ms) = trace.total_elapsed_ms {
        out.push_str(&format!("  elapsed: {}ms\n", ms));
    }
    if let Some(ref name) = trace.metadata.agent_name {
        out.push_str(&format!("  agent: {}\n", name));
    }
    out.push_str(&format!("  model: {}\n", trace.metadata.model));
    out.push_str(&format!("  provider: {}\n", trace.metadata.provider));

    if let Some(ref usage) = trace.total_usage {
        out.push_str("  usage:\n");
        if let Some(pt) = usage.prompt_tokens {
            out.push_str(&format!("    prompt_tokens: {}\n", pt));
        }
        if let Some(ct) = usage.completion_tokens {
            out.push_str(&format!("    completion_tokens: {}\n", ct));
        }
        if let Some(tt) = usage.total_tokens {
            out.push_str(&format!("    total_tokens: {}\n", tt));
        }
        if let Some(cached) = usage.cached_tokens {
            out.push_str(&format!("    cached_tokens: {}\n", cached));
        }
    }

    // Build parent-child tree
    let root_spans: Vec<&Span> = trace
        .spans
        .iter()
        .filter(|s| s.parent_span_id.is_none())
        .collect();

    if !root_spans.is_empty() || !trace.spans.is_empty() {
        out.push_str("  spans:\n");
        for root in &root_spans {
            serialize_span_yaml(&mut out, root, &trace.spans, 2, flags);
        }
        // Also print orphan spans (parent not in this trace) at root level
        let root_ids: Vec<&str> = root_spans.iter().map(|s| s.span_id.as_str()).collect();
        for span in &trace.spans {
            if span.parent_span_id.is_some()
                && !root_ids.contains(&span.span_id.as_str())
                && !has_parent_in_trace(span, &trace.spans)
            {
                serialize_span_yaml(&mut out, span, &trace.spans, 2, flags);
            }
        }
    }

    out.push_str("...\n");
    out
}

/// Check if a span's parent exists in the trace's span list.
fn has_parent_in_trace(span: &Span, all_spans: &[Span]) -> bool {
    if let Some(ref parent_id) = span.parent_span_id {
        all_spans.iter().any(|s| s.span_id == *parent_id)
    } else {
        false
    }
}

/// Recursively serialize a span and its children in YAML-like tree format.
fn serialize_span_yaml(out: &mut String, span: &Span, all_spans: &[Span], indent: usize, flags: ContentFlags) {
    let pad = " ".repeat(indent * 2);
    let status_str = match &span.status {
        SpanStatus::Running => "running",
        SpanStatus::Ok => "ok",
        SpanStatus::Error(_) => "error",
    };
    let elapsed_str = span
        .elapsed_ms
        .map(|ms| format!("{}ms", ms))
        .unwrap_or_else(|| "?".to_string());

    // Header line: - [type] "name" (elapsed, status)
    let type_tag = span_type_tag(&span.span_type);
    out.push_str(&format!(
        "{}- [{}] \"{}\" ({}, {})\n",
        pad, type_tag, span.name, elapsed_str, status_str
    ));

    // Error message if any
    if let SpanStatus::Error(ref msg) = span.status {
        out.push_str(&format!("{}    error: {}\n", pad, maybe_truncate(msg, 200, true)));
    }

    // Type-specific details
    let detail_pad = format!("{}    ", pad);
    match &span.span_type {
        SpanType::AgentTurn { user_input, output } => {
            out.push_str(&format!(
                "{}input: \"{}\"\n",
                detail_pad,
                maybe_truncate(user_input, 100, flags.llm_input)
            ));
            if let Some(o) = output {
                out.push_str(&format!(
                    "{}output: \"{}\"\n",
                    detail_pad,
                    maybe_truncate(o, 200, flags.llm_output)
                ));
            }
        }
        SpanType::LlmCall {
            model,
            input_messages,
            available_tools,
            output_content,
            reasoning_content,
            tool_calls,
            usage,
            ..
        } => {
            out.push_str(&format!("{}model: {}\n", detail_pad, model));
            if flags.llm_input && !input_messages.is_empty() {
                out.push_str(&format!("{}input_messages: ({} messages)\n", detail_pad, input_messages.len()));
                for (i, msg) in input_messages.iter().enumerate() {
                    out.push_str(&format!("{}  [{}] role={}, len={}\n", detail_pad, i, msg.role, msg.content_len));
                    // content_preview already contains full content when llm_input is true
                    // (truncation was applied at span creation time in context.rs)
                    out.push_str(&format!("{}      content: \"{}\"\n", detail_pad, maybe_truncate(&msg.content_preview, 200, flags.llm_input)));
                }
            } else {
                out.push_str(&format!(
                    "{}input_messages: {}\n",
                    detail_pad,
                    input_messages.len()
                ));
            }
            if !available_tools.is_empty() {
                out.push_str(&format!(
                    "{}available_tools: [{}]\n",
                    detail_pad,
                    available_tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")
                ));
                if flags.llm_input {
                    out.push_str(&format!("{}tool_details:\n", detail_pad));
                    for (i, tool) in available_tools.iter().enumerate() {
                        out.push_str(&format!("{}  [{}] {}\n", detail_pad, i, tool.name));
                        out.push_str(&format!("{}      description: {}\n", detail_pad, tool.description));
                        if let Some(params) = tool.parameters_schema.get("properties") {
                            let param_names: Vec<String> = params.as_object()
                                .map(|props| props.keys().map(|k| k.to_string()).collect())
                                .unwrap_or_default();
                            if !param_names.is_empty() {
                                out.push_str(&format!("{}      parameters: {}\n", detail_pad, param_names.join(", ")));
                            }
                        }
                    }
                }
            }
            if let Some(content) = output_content {
                out.push_str(&format!(
                    "{}output: \"{}\"\n",
                    detail_pad,
                    maybe_truncate(content, 200, flags.llm_output)
                ));
            }
            if let Some(reasoning) = reasoning_content {
                out.push_str(&format!(
                    "{}reasoning: \"{}\"\n",
                    detail_pad,
                    maybe_truncate(reasoning, 150, flags.llm_output)
                ));
            }
            if !tool_calls.is_empty() {
                let names: Vec<&str> = tool_calls.iter().map(|tc| tc.function_name.as_str()).collect();
                out.push_str(&format!(
                    "{}tool_calls: [{}]\n",
                    detail_pad,
                    names.join(", ")
                ));
            }
            if let Some(u) = usage {
                let parts: Vec<String> = [
                    u.prompt_tokens.map(|v| format!("prompt={}", v)),
                    u.completion_tokens.map(|v| format!("completion={}", v)),
                    u.total_tokens.map(|v| format!("total={}", v)),
                    u.cached_tokens.map(|v| format!("cached={}", v)),
                ]
                .into_iter()
                .flatten()
                .collect();
                if !parts.is_empty() {
                    out.push_str(&format!("{}usage: {}\n", detail_pad, parts.join(", ")));
                }
            }
        }
        SpanType::ToolCall {
            tool_name,
            source,
            arguments,
            result,
            success,
        } => {
            out.push_str(&format!("{}tool: {}\n", detail_pad, tool_name));
            out.push_str(&format!("{}source: {}\n", detail_pad, source));
            out.push_str(&format!("{}success: {}\n", detail_pad, success));
            let args_str = serde_json::to_string(arguments).unwrap_or_default();
            out.push_str(&format!(
                "{}arguments: {}\n",
                detail_pad,
                maybe_truncate(&args_str, 200, flags.llm_output)
            ));
            if let Some(r) = result {
                out.push_str(&format!(
                    "{}result: \"{}\"\n",
                    detail_pad,
                    maybe_truncate(r, 200, flags.tool_result)
                ));
            }
        }
        SpanType::SubagentCall {
            agent_name,
            task,
            model,
            result,
            usage,
            tool_rounds,
        } => {
            out.push_str(&format!("{}agent: {}\n", detail_pad, agent_name));
            out.push_str(&format!(
                "{}task: \"{}\"\n",
                detail_pad,
                maybe_truncate(task, 150, flags.llm_input)
            ));
            if let Some(m) = model {
                out.push_str(&format!("{}model: {}\n", detail_pad, m));
            }
            out.push_str(&format!("{}tool_rounds: {}\n", detail_pad, tool_rounds));
            if let Some(u) = usage {
                if let Some(tt) = u.total_tokens {
                    out.push_str(&format!("{}total_tokens: {}\n", detail_pad, tt));
                }
            }
            if let Some(r) = result {
                out.push_str(&format!(
                    "{}result: \"{}\"\n",
                    detail_pad,
                    maybe_truncate(r, 200, flags.tool_result)
                ));
            }
        }
    }
    // Render children recursively
    let children: Vec<&Span> = all_spans
        .iter()
        .filter(|s| s.parent_span_id.as_deref() == Some(&span.span_id))
        .collect();
    for child in children {
        serialize_span_yaml(out, child, all_spans, indent + 1, flags);
    }
}
/// Get a short type tag for the span type.
fn span_type_tag(span_type: &SpanType) -> &'static str {
    match span_type {
        SpanType::AgentTurn { .. } => "agent_turn",
        SpanType::LlmCall { .. } => "llm_call",
        SpanType::ToolCall { .. } => "tool_call",
        SpanType::SubagentCall { .. } => "subagent_call",
    }
}

// Use shared truncation utilities from `tools::text_utils`.
use crate::tools::maybe_truncate_for_display;

/// Alias for backward compatibility within this module.
fn maybe_truncate(s: &str, max_len: usize, full_content: bool) -> String {
    maybe_truncate_for_display(s, max_len, full_content)
}
