//! Langfuse exporter — sends traces to Langfuse for LLM observability.
//!
//! Langfuse is a popular open-source LLM observability platform that provides
//! cost tracking, quality monitoring, and prompt management.
//!
//! API reference: https://langfuse.com/docs/api
//!
//! This exporter maps our internal trace/span model to Langfuse's
//! trace → generation/span hierarchy:
//!
//! - `Trace` → Langfuse Trace (one per user turn)
//! - `LlmCall` span → Langfuse Generation (with model, usage, etc.)
//! - `ToolCall` span → Langfuse Span (with tool metadata)
//! - `SubagentCall` span → Langfuse Span (with subagent metadata)
//! - `AgentTurn` span → Langfuse Span (root span)

use async_trait::async_trait;
use reqwest::Client;

use crate::agent_tracing::collector::TracingCollector;
use crate::agent_tracing::types::{Span, SpanStatus, SpanType, Trace};

/// Langfuse tracing collector.
///
/// Sends completed traces to the Langfuse API using the ingestion endpoint.
/// Each trace is sent as a batch of events (trace + observations) in a
/// single HTTP request.
pub struct LangfuseCollector {
    /// Langfuse API host (e.g., "https://cloud.langfuse.com").
    host: String,
    /// Public API key for authentication.
    public_key: String,
    /// Secret API key for authentication.
    secret_key: String,
    /// HTTP client for sending requests.
    client: Client,
}

impl LangfuseCollector {
    /// Create a new Langfuse collector.
    ///
    /// - `public_key`: Langfuse project public key (pk-...)
    /// - `secret_key`: Langfuse project secret key (sk-...)
    /// - `host`: Optional custom host (defaults to "https://cloud.langfuse.com")
    pub fn new(public_key: String, secret_key: String, host: Option<String>) -> Self {
        let host = host.unwrap_or_else(|| "https://cloud.langfuse.com".to_string());

        tracing::info!(
            host = %host,
            "Initializing Langfuse tracing exporter"
        );

        Self {
            host,
            public_key,
            secret_key,
            client: Client::new(),
        }
    }

    /// Build the Langfuse ingestion batch payload from a completed trace.
    ///
    /// The batch contains:
    /// 1. A `trace-create` event for the overall trace
    /// 2. A `generation-create` event for each LLM call span
    /// 3. A `span-create` event for each tool/subagent/agent-turn span
    fn build_ingestion_batch(&self, trace: &Trace) -> serde_json::Value {
        let mut batch = Vec::new();

        // 1. Create the trace event
        let trace_input = trace.spans.iter().find_map(|s| {
            if let SpanType::AgentTurn { ref user_input, .. } = s.span_type {
                Some(user_input.clone())
            } else {
                None
            }
        });
        let trace_output = trace.spans.iter().find_map(|s| {
            if let SpanType::AgentTurn { ref output, .. } = s.span_type {
                output.clone()
            } else {
                None
            }
        });

        let mut trace_metadata = serde_json::json!({
            "model": trace.metadata.model,
            "provider": trace.metadata.provider,
        });
        if let Some(ref name) = trace.metadata.agent_name {
            trace_metadata["agent_name"] = serde_json::json!(name);
        }
        if let Some(ref usage) = trace.total_usage {
            if let Some(tt) = usage.total_tokens {
                trace_metadata["total_tokens"] = serde_json::json!(tt);
            }
        }
        if let Some(ms) = trace.total_elapsed_ms {
            trace_metadata["total_elapsed_ms"] = serde_json::json!(ms);
        }

        batch.push(serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "type": "trace-create",
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "body": {
                "id": trace.trace_id,
                "name": format!("agent-turn"),
                "sessionId": trace.session_id,
                "input": trace_input,
                "output": trace_output,
                "metadata": trace_metadata,
            }
        }));

        // 2. Create observation events for each span
        for span in &trace.spans {
            match &span.span_type {
                SpanType::LlmCall { .. } => {
                    batch.push(self.build_generation_event(span, &trace.trace_id));
                }
                _ => {
                    batch.push(self.build_span_event(span, &trace.trace_id));
                }
            }
        }

        serde_json::json!({
            "batch": batch,
        })
    }

    /// Build a Langfuse `generation-create` event from an LLM call span.
    fn build_generation_event(&self, span: &Span, trace_id: &str) -> serde_json::Value {
        let (model, input, output, usage, reasoning) = match &span.span_type {
            SpanType::LlmCall {
                model,
                input_messages,
                output_content,
                reasoning_content,
                usage,
                ..
            } => {
                let input: Vec<serde_json::Value> = input_messages
                    .iter()
                    .map(|m| {
                        serde_json::json!({
                            "role": m.role,
                            "content": m.content_preview,
                        })
                    })
                    .collect();

                let usage_obj = usage.as_ref().map(|u| {
                    serde_json::json!({
                        "input": u.prompt_tokens,
                        "output": u.completion_tokens,
                        "total": u.total_tokens,
                    })
                });

                (
                    model.clone(),
                    serde_json::json!(input),
                    output_content.clone().map(|c| serde_json::json!(c)),
                    usage_obj,
                    reasoning_content.clone(),
                )
            }
            _ => return self.build_span_event(span, trace_id),
        };

        let level = match &span.status {
            SpanStatus::Ok => "DEFAULT",
            SpanStatus::Error(_) => "ERROR",
            SpanStatus::Running => "DEBUG",
        };
        let status_message = match &span.status {
            SpanStatus::Error(msg) => Some(msg.as_str()),
            _ => None,
        };

        let mut metadata = serde_json::json!({});
        if let Some(ref r) = reasoning {
            metadata["reasoning_content"] = serde_json::json!(r);
        }
        if let Some(ms) = span.elapsed_ms {
            metadata["elapsed_ms"] = serde_json::json!(ms);
        }

        let mut body = serde_json::json!({
            "id": span.span_id,
            "traceId": trace_id,
            "type": "GENERATION",
            "name": span.name,
            "startTime": span.started_at.to_rfc3339(),
            "model": model,
            "input": input,
            "level": level,
            "metadata": metadata,
        });

        if let Some(ref parent) = span.parent_span_id {
            body["parentObservationId"] = serde_json::json!(parent);
        }
        if let Some(ref end_time) = span.ended_at {
            body["endTime"] = serde_json::json!(end_time.to_rfc3339());
        }
        if let Some(ref out) = output {
            body["output"] = out.clone();
        }
        if let Some(ref u) = usage {
            body["usage"] = u.clone();
        }
        if let Some(msg) = status_message {
            body["statusMessage"] = serde_json::json!(msg);
        }

        serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "type": "generation-create",
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "body": body,
        })
    }

    /// Build a Langfuse `span-create` event from a non-LLM span.
    fn build_span_event(&self, span: &Span, trace_id: &str) -> serde_json::Value {
        let level = match &span.status {
            SpanStatus::Ok => "DEFAULT",
            SpanStatus::Error(_) => "ERROR",
            SpanStatus::Running => "DEBUG",
        };
        let status_message = match &span.status {
            SpanStatus::Error(msg) => Some(msg.as_str()),
            _ => None,
        };

        let (input, output, metadata) = match &span.span_type {
            SpanType::AgentTurn { user_input, output } => (
                Some(serde_json::json!(user_input)),
                output.as_ref().map(|o| serde_json::json!(o)),
                serde_json::json!({ "type": "agent_turn" }),
            ),
            SpanType::ToolCall {
                tool_name,
                source,
                arguments,
                result,
                success,
            } => (
                Some(arguments.clone()),
                result.as_ref().map(|r| serde_json::json!(r)),
                serde_json::json!({
                    "type": "tool_call",
                    "tool_name": tool_name,
                    "source": source,
                    "success": success,
                    "elapsed_ms": span.elapsed_ms,
                }),
            ),
            SpanType::SubagentCall {
                agent_name,
                task,
                model,
                result,
                tool_rounds,
                usage,
            } => (
                Some(serde_json::json!(task)),
                result.as_ref().map(|r| serde_json::json!(r)),
                serde_json::json!({
                    "type": "subagent_call",
                    "agent_name": agent_name,
                    "model": model,
                    "tool_rounds": tool_rounds,
                    "total_tokens": usage.as_ref().and_then(|u| u.total_tokens),
                    "elapsed_ms": span.elapsed_ms,
                }),
            ),
            // LlmCall should go through build_generation_event, but handle gracefully
            SpanType::LlmCall { .. } => (
                None,
                None,
                serde_json::json!({ "type": "llm_call" }),
            ),
        };

        let mut body = serde_json::json!({
            "id": span.span_id,
            "traceId": trace_id,
            "name": span.name,
            "startTime": span.started_at.to_rfc3339(),
            "level": level,
            "metadata": metadata,
        });

        if let Some(ref parent) = span.parent_span_id {
            body["parentObservationId"] = serde_json::json!(parent);
        }
        if let Some(ref end_time) = span.ended_at {
            body["endTime"] = serde_json::json!(end_time.to_rfc3339());
        }
        if let Some(ref inp) = input {
            body["input"] = inp.clone();
        }
        if let Some(ref out) = output {
            body["output"] = out.clone();
        }
        if let Some(msg) = status_message {
            body["statusMessage"] = serde_json::json!(msg);
        }

        serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "type": "span-create",
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "body": body,
        })
    }

    /// Send the ingestion batch to Langfuse.
    async fn send_batch(&self, payload: serde_json::Value) {
        let url = format!("{}/api/public/ingestion", self.host.trim_end_matches('/'));

        match self
            .client
            .post(&url)
            .basic_auth(&self.public_key, Some(&self.secret_key))
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await
        {
            Ok(resp) => {
                if resp.status().is_success() {
                    tracing::debug!(
                        url = %url,
                        "Langfuse trace export successful"
                    );
                } else {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    tracing::warn!(
                        url = %url,
                        status = %status,
                        body = %body,
                        "Langfuse trace export failed"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    url = %url,
                    error = %e,
                    "Langfuse trace export request failed"
                );
            }
        }
    }
}

#[async_trait]
impl TracingCollector for LangfuseCollector {
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
        let payload = self.build_ingestion_batch(trace);
        self.send_batch(payload).await;
    }

    async fn flush(&self) {
        // All sends are immediate on trace_end, nothing to flush.
    }

    fn name(&self) -> &str {
        "langfuse"
    }
}
