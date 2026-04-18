//! OpenTelemetry OTLP exporter — sends traces via OTLP/HTTP JSON protocol.
//!
//! Implements the OTLP/HTTP JSON trace export protocol, sending completed
//! traces to an OpenTelemetry collector (e.g., Jaeger, Tempo, SigNoz).
//!
//! Protocol reference: https://opentelemetry.io/docs/specs/otlp/#otlphttp
//!
//! This is a lightweight implementation that doesn't depend on the full
//! OpenTelemetry SDK — it constructs the OTLP JSON payload directly from
//! our internal `Trace`/`Span` types.

use async_trait::async_trait;
use reqwest::Client;
use tokio::sync::Mutex;

use crate::agent_tracing::collector::TracingCollector;
use crate::agent_tracing::types::{Span, SpanStatus, SpanType, Trace};

/// OpenTelemetry OTLP/HTTP JSON exporter.
///
/// Batches completed traces and sends them to an OTLP-compatible endpoint.
/// Traces are buffered and sent on `on_trace_end` or `flush`.
pub struct OtelCollector {
    /// OTLP endpoint URL (e.g., "http://localhost:4318").
    endpoint: String,
    /// Service name reported in the resource attributes.
    service_name: String,
    /// HTTP client for sending requests.
    client: Client,
    /// Buffer of pending OTLP resource spans (flushed on trace_end).
    buffer: Mutex<Vec<serde_json::Value>>,
}

impl OtelCollector {
    /// Create a new OTel collector targeting the given OTLP endpoint.
    ///
    /// The endpoint should be the base URL of the OTLP receiver
    /// (e.g., `http://localhost:4318`). The `/v1/traces` path is
    /// appended automatically.
    pub fn new(endpoint: String, service_name: Option<String>) -> Self {
        let service_name = service_name.unwrap_or_else(|| "daedalus-agent".to_string());

        tracing::info!(
            endpoint = %endpoint,
            service_name = %service_name,
            "Initializing OpenTelemetry OTLP exporter"
        );

        Self {
            endpoint,
            service_name,
            client: Client::new(),
            buffer: Mutex::new(Vec::new()),
        }
    }

    /// Convert our Trace into an OTLP ResourceSpans JSON payload.
    fn trace_to_otlp(&self, trace: &Trace) -> serde_json::Value {
        let resource = serde_json::json!({
            "attributes": [
                { "key": "service.name", "value": { "stringValue": self.service_name } },
                { "key": "agent.name", "value": { "stringValue": trace.metadata.agent_name.as_deref().unwrap_or("daedalus") } },
                { "key": "llm.model", "value": { "stringValue": trace.metadata.model } },
                { "key": "llm.provider", "value": { "stringValue": trace.metadata.provider } },
            ]
        });

        let otlp_spans: Vec<serde_json::Value> = trace
            .spans
            .iter()
            .map(|s| self.span_to_otlp(s, trace))
            .collect();

        serde_json::json!({
            "resource": resource,
            "scopeSpans": [{
                "scope": {
                    "name": "daedalus.agent",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "spans": otlp_spans,
            }]
        })
    }

    /// Convert a single Span into an OTLP Span JSON object.
    fn span_to_otlp(&self, span: &Span, trace: &Trace) -> serde_json::Value {
        // OTLP uses 16-byte hex trace IDs and 8-byte hex span IDs.
        // We derive them deterministically from our UUID-based IDs.
        let trace_id = uuid_to_otlp_trace_id(&trace.trace_id);
        let span_id = uuid_to_otlp_span_id(&span.span_id);
        let parent_span_id = span
            .parent_span_id
            .as_ref()
            .map(|id| uuid_to_otlp_span_id(id))
            .unwrap_or_default();

        let start_time_nanos = span.started_at.timestamp_nanos_opt().unwrap_or(0);
        let end_time_nanos = span
            .ended_at
            .and_then(|t| t.timestamp_nanos_opt())
            .unwrap_or(start_time_nanos);

        // Map our SpanStatus to OTLP StatusCode
        let (status_code, status_message) = match &span.status {
            SpanStatus::Ok => (1, String::new()), // STATUS_CODE_OK
            SpanStatus::Error(msg) => (2, msg.clone()), // STATUS_CODE_ERROR
            SpanStatus::Running => (0, String::new()), // STATUS_CODE_UNSET
        };

        // Build span kind based on type
        let span_kind = match &span.span_type {
            SpanType::LlmCall { .. } => 3, // SPAN_KIND_CLIENT
            SpanType::ToolCall { .. } => 1, // SPAN_KIND_INTERNAL
            SpanType::SubagentCall { .. } => 1, // SPAN_KIND_INTERNAL
            SpanType::AgentTurn { .. } => 2, // SPAN_KIND_SERVER
        };

        // Build attributes from span type
        let mut attributes = Vec::new();
        self.add_span_type_attributes(&mut attributes, &span.span_type);

        // Add elapsed_ms as attribute
        if let Some(ms) = span.elapsed_ms {
            attributes.push(serde_json::json!({
                "key": "duration_ms",
                "value": { "intValue": ms.to_string() }
            }));
        }

        // Add custom attributes
        for (key, value) in &span.attributes {
            let otlp_value = match value {
                crate::agent_tracing::types::SpanValue::String(s) => {
                    serde_json::json!({ "stringValue": s })
                }
                crate::agent_tracing::types::SpanValue::Int(i) => {
                    serde_json::json!({ "intValue": i.to_string() })
                }
                crate::agent_tracing::types::SpanValue::Float(f) => {
                    serde_json::json!({ "doubleValue": f })
                }
                crate::agent_tracing::types::SpanValue::Bool(b) => {
                    serde_json::json!({ "boolValue": b })
                }
            };
            attributes.push(serde_json::json!({
                "key": key,
                "value": otlp_value
            }));
        }

        serde_json::json!({
            "traceId": trace_id,
            "spanId": span_id,
            "parentSpanId": parent_span_id,
            "name": span.name,
            "kind": span_kind,
            "startTimeUnixNano": start_time_nanos.to_string(),
            "endTimeUnixNano": end_time_nanos.to_string(),
            "attributes": attributes,
            "status": {
                "code": status_code,
                "message": status_message,
            },
        })
    }

    /// Add type-specific attributes to the OTLP span.
    fn add_span_type_attributes(
        &self,
        attrs: &mut Vec<serde_json::Value>,
        span_type: &SpanType,
    ) {
        match span_type {
            SpanType::AgentTurn { user_input, output } => {
                attrs.push(attr_str("daedalus.span_type", "agent_turn"));
                attrs.push(attr_str("daedalus.user_input", user_input));
                if let Some(out) = output {
                    attrs.push(attr_str("daedalus.output", out));
                }
            }
            SpanType::LlmCall {
                model,
                provider,
                output_content,
                reasoning_content,
                tool_calls,
                usage,
                input_messages,
                ..
            } => {
                attrs.push(attr_str("daedalus.span_type", "llm_call"));
                attrs.push(attr_str("gen_ai.system", provider));
                attrs.push(attr_str("gen_ai.request.model", model));
                attrs.push(attr_int("gen_ai.request.message_count", input_messages.len() as i64));
                if let Some(content) = output_content {
                    attrs.push(attr_str("gen_ai.response.content", content));
                }
                if let Some(reasoning) = reasoning_content {
                    attrs.push(attr_str("gen_ai.response.reasoning", reasoning));
                }
                if !tool_calls.is_empty() {
                    attrs.push(attr_int("gen_ai.response.tool_call_count", tool_calls.len() as i64));
                }
                if let Some(u) = usage {
                    if let Some(pt) = u.prompt_tokens {
                        attrs.push(attr_int("gen_ai.usage.prompt_tokens", pt as i64));
                    }
                    if let Some(ct) = u.completion_tokens {
                        attrs.push(attr_int("gen_ai.usage.completion_tokens", ct as i64));
                    }
                    if let Some(tt) = u.total_tokens {
                        attrs.push(attr_int("gen_ai.usage.total_tokens", tt as i64));
                    }
                }
            }
            SpanType::ToolCall {
                tool_name,
                source,
                success,
                ..
            } => {
                attrs.push(attr_str("daedalus.span_type", "tool_call"));
                attrs.push(attr_str("daedalus.tool.name", tool_name));
                attrs.push(attr_str("daedalus.tool.source", source));
                attrs.push(serde_json::json!({
                    "key": "daedalus.tool.success",
                    "value": { "boolValue": success }
                }));
            }
            SpanType::SubagentCall {
                agent_name,
                model,
                tool_rounds,
                usage,
                ..
            } => {
                attrs.push(attr_str("daedalus.span_type", "subagent_call"));
                attrs.push(attr_str("daedalus.subagent.name", agent_name));
                if let Some(m) = model {
                    attrs.push(attr_str("daedalus.subagent.model", m));
                }
                attrs.push(attr_int("daedalus.subagent.tool_rounds", *tool_rounds as i64));
                if let Some(u) = usage {
                    if let Some(tt) = u.total_tokens {
                        attrs.push(attr_int("daedalus.subagent.total_tokens", tt as i64));
                    }
                }
            }
        }
    }

    /// Send buffered resource spans to the OTLP endpoint.
    async fn send_batch(&self, resource_spans: Vec<serde_json::Value>) {
        if resource_spans.is_empty() {
            return;
        }

        let payload = serde_json::json!({
            "resourceSpans": resource_spans,
        });

        let url = format!("{}/v1/traces", self.endpoint.trim_end_matches('/'));

        match self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await
        {
            Ok(resp) => {
                if resp.status().is_success() {
                    tracing::debug!(
                        url = %url,
                        "OTLP trace export successful"
                    );
                } else {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    tracing::warn!(
                        url = %url,
                        status = %status,
                        body = %body,
                        "OTLP trace export failed"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    url = %url,
                    error = %e,
                    "OTLP trace export request failed"
                );
            }
        }
    }
}

#[async_trait]
impl TracingCollector for OtelCollector {
    async fn on_trace_start(&self, _trace: &Trace) {
        // No-op: we send the complete trace on trace_end
    }

    async fn on_span_start(&self, _span: &Span) {
        // No-op: spans are sent as part of the complete trace
    }

    async fn on_span_end(&self, _span: &Span) {
        // No-op: spans are sent as part of the complete trace
    }

    async fn on_trace_end(&self, trace: &Trace) {
        let resource_spans = self.trace_to_otlp(trace);
        // Send immediately (no batching for now — each trace is one request)
        self.send_batch(vec![resource_spans]).await;
    }

    async fn flush(&self) {
        let pending = {
            let mut buf = self.buffer.lock().await;
            std::mem::take(&mut *buf)
        };
        if !pending.is_empty() {
            self.send_batch(pending).await;
        }
    }

    fn name(&self) -> &str {
        "otel"
    }
}

// ── Helper functions ──

/// Convert a UUID string to a 32-char hex OTLP trace ID.
fn uuid_to_otlp_trace_id(uuid_str: &str) -> String {
    // Remove hyphens from UUID to get 32 hex chars
    uuid_str.replace('-', "")
}

/// Convert a UUID string to a 16-char hex OTLP span ID.
fn uuid_to_otlp_span_id(uuid_str: &str) -> String {
    // Take the first 16 hex chars (8 bytes) from the UUID
    uuid_str.replace('-', "").chars().take(16).collect()
}

/// Build an OTLP string attribute.
fn attr_str(key: &str, value: &str) -> serde_json::Value {
    serde_json::json!({
        "key": key,
        "value": { "stringValue": value }
    })
}

/// Build an OTLP integer attribute.
fn attr_int(key: &str, value: i64) -> serde_json::Value {
    serde_json::json!({
        "key": key,
        "value": { "intValue": value.to_string() }
    })
}
