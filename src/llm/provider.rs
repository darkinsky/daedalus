//! Unified LLM provider — single reqwest-based implementation for all APIs.
//!
//! Replaces the previous dual-provider architecture (GenAiProvider + VenusProvider)
//! with a single provider that delegates format-specific logic to adapters.
//!
//! ## Architecture
//!
//! ```text
//! LlmProvider (HTTP transport, SSE parsing)
//!     └── ApiAdapter (format-specific logic)
//!         ├── OpenAiAdapter  — OpenAI, Venus, DeepSeek, compatible APIs
//!         ├── AnthropicAdapter — Anthropic Messages API
//!         └── GeminiAdapter — Google Gemini API
//! ```

use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;

use super::adapter::{self, ApiAdapter};
use super::{
    ChatMessage, ChatOptions, ChatResponse, LlmApi, LlmConfig,
    StreamChunk, ToolCall, ToolRound,
};

/// Maximum number of retries for transient LLM API errors (429, 5xx).
const MAX_RETRIES: u32 = 3;
/// Initial backoff delay between retries.
const INITIAL_BACKOFF: Duration = Duration::from_secs(2);
/// HTTP request timeout (covers connect + response time).
const HTTP_TIMEOUT: Duration = Duration::from_secs(300);
/// HTTP connect timeout.
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Unified LLM provider backed by reqwest HTTP client.
///
/// Supports all LLM APIs through the adapter pattern:
/// - OpenAI and compatible (Venus proxy, DeepSeek, etc.)
/// - Anthropic Messages API (direct)
/// - Google Gemini API (direct)
///
/// The adapter handles all format-specific logic (request body construction,
/// response parsing, authentication headers), while this struct handles
/// the HTTP transport, SSE stream parsing, and error handling.
pub struct LlmProvider {
    client: Client,
    config: LlmConfig,
    base_url: String,
    adapter: Box<dyn ApiAdapter>,
}

impl LlmProvider {
    /// Create a new LLM provider with the given configuration.
    pub fn new(config: LlmConfig) -> Result<Self> {
        let base_url = config
            .api_base
            .as_deref()
            .unwrap_or(Self::default_base_url(&config))
            .trim_end_matches('/')
            .to_string();

        let adapter = adapter::create_adapter(&config);
        let client = Client::builder()
            .timeout(HTTP_TIMEOUT)
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .build()
            .unwrap_or_else(|_| Client::new());

        tracing::info!(
            model = %config.model,
            adapter = adapter.name(),
            base_url = %base_url,
            session_id = ?config.session_id,
            thinking_enabled = ?config.venus.thinking_enabled,
            thinking_tokens = ?config.venus.thinking_tokens,
            reasoning_effort = ?config.venus.reasoning_effort,
            "LLM provider initialized"
        );

        Ok(Self { client, config, base_url, adapter })
    }

    /// Return the default base URL based on adapter kind.
    fn default_base_url(config: &LlmConfig) -> &'static str {
        match config.adapter_kind.as_deref().map(|s| s.to_lowercase()).as_deref() {
            Some("anthropic") => "https://api.anthropic.com/v1",
            Some("gemini") | Some("google") => "https://generativelanguage.googleapis.com/v1beta",
            _ => "https://api.openai.com/v1",
        }
    }

    /// Build the request body for a chat completion call.
    fn build_request_body(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        tool_history: &[ToolRound],
        options: Option<&ChatOptions>,
    ) -> Value {
        self.adapter.build_body(
            &self.config.model,
            messages,
            tools,
            tool_history,
            options,
            &self.config.venus,
        )
    }

    /// Build headers with optional session-based routing.
    ///
    /// Starts with adapter-provided headers, then injects `Venus-Session-Id`
    /// if the config has a session_id set. This enables per-subagent routing
    /// affinity for prompt cache isolation.
    fn build_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = self.adapter.headers(&self.config.api_key);
        if let Some(ref session_id) = self.config.session_id {
            if let Ok(val) = reqwest::header::HeaderValue::from_str(session_id) {
                headers.insert("Venus-Session-Id", val);
            }
        }
        headers
    }

    /// Send an HTTP request with automatic retry for transient errors.
    ///
    /// Retries on:
    /// - HTTP 429 (rate limit) — with exponential backoff
    /// - HTTP 5xx (server errors) — with exponential backoff
    /// - Network/timeout errors — with exponential backoff
    ///
    /// Does NOT retry on:
    /// - HTTP 4xx (client errors, except 429) — these are permanent failures
    /// - JSON parse errors — indicates a bug, not transient
    async fn send_request(&self, body: &Value) -> Result<Value> {
        let url = self.adapter.endpoint(&self.base_url, &self.config.model);

        tracing::debug!(
            url = %url,
            adapter = self.adapter.name(),
            model = %self.config.model,
            message_count = ?body.get("messages").and_then(|m| m.as_array()).map(|a| a.len())
                .or_else(|| body.get("contents").and_then(|c| c.as_array()).map(|a| a.len())),
            tool_count = ?body.get("tools").and_then(|t| t.as_array()).map(|a| a.len()),
            "LLM API request"
        );

        if std::env::var("DAEDALUS_TRACE_BODIES").as_deref() == Ok("1") {
            tracing::trace!(request_body = %body, "LLM API request body (full)");
        }

        let mut last_error = None;
        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                let backoff = INITIAL_BACKOFF * 2u32.pow(attempt - 1);
                tracing::warn!(
                    attempt,
                    backoff_ms = backoff.as_millis() as u64,
                    "Retrying LLM API request after transient error"
                );
                tokio::time::sleep(backoff).await;
            }

            let headers = self.build_headers();
            let start = std::time::Instant::now();
            let response = match self
                .client
                .post(&url)
                .headers(headers)
                .json(body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    // Network/timeout error — retryable
                    tracing::warn!(
                        attempt,
                        error = %e,
                        "LLM HTTP request failed (network/timeout)"
                    );
                    last_error = Some(anyhow::anyhow!("LLM HTTP request error: {}", e));
                    continue;
                }
            };
            let http_elapsed_ms = start.elapsed().as_millis() as u64;

            let status = response.status();
            let response_text = response
                .text()
                .await
                .map_err(|e| anyhow::anyhow!("LLM response read error: {}", e))?;

            tracing::debug!(
                status = %status,
                response_len = response_text.len(),
                http_elapsed_ms = http_elapsed_ms,
                attempt,
                "LLM API response received"
            );

            if std::env::var("DAEDALUS_TRACE_BODIES").as_deref() == Ok("1") {
                tracing::trace!(response_body = %response_text, "LLM API response body (full)");
            }

            // Check for retryable HTTP status codes
            if status.as_u16() == 429 || status.is_server_error() {
                tracing::warn!(
                    status = %status,
                    attempt,
                    "LLM API returned retryable error"
                );
                last_error = Some(anyhow::anyhow!(
                    "LLM API error (HTTP {}): {}",
                    status.as_u16(),
                    &response_text[..response_text.len().min(200)]
                ));
                continue;
            }

            let response_body: Value = serde_json::from_str(&response_text)
                .map_err(|e| {
                    let preview = if response_text.len() <= 500 {
                        &response_text
                    } else {
                        &response_text[..500]
                    };
                    anyhow::anyhow!("LLM response parse error: {} (body: {})", e, preview)
                })?;

            if !status.is_success() {
                let error_msg = response_body
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("Unknown error");
                return Err(anyhow::anyhow!(
                    "LLM API error (HTTP {}): {}",
                    status.as_u16(),
                    error_msg
                ));
            }

            return Ok(response_body);
        }

        // All retries exhausted
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("LLM API request failed after {} retries", MAX_RETRIES)))
    }

    /// Send a streaming HTTP request and return a channel of StreamChunks.
    async fn send_stream_request(
        &self,
        body: Value,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamChunk>> {
        let url = self.adapter.endpoint(&self.base_url, &self.config.model);
        let headers = self.build_headers();
        let adapter_name = self.adapter.name().to_string();

        tracing::debug!(
            url = %url,
            adapter = %adapter_name,
            model = %self.config.model,
            "LLM API streaming request"
        );

        let response = self
            .client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("LLM HTTP streaming request error: {}", e))?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            let error_body: Value = serde_json::from_str(&error_text).unwrap_or(json!({}));
            let error_msg = error_body
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error");
            return Err(anyhow::anyhow!(
                "LLM API streaming error (HTTP {}): {}",
                status.as_u16(),
                error_msg
            ));
        }

        let (tx, rx) = tokio::sync::mpsc::channel::<StreamChunk>(64);

        // Spawn a task to read the SSE stream
        tokio::spawn(async move {
            use tokio_stream::StreamExt;

            let mut byte_stream = response.bytes_stream();
            let mut buffer = String::new();
            let mut tool_call_builders: HashMap<usize, PartialToolCall> = HashMap::new();

            while let Some(chunk_result) = byte_stream.next().await {
                let bytes = match chunk_result {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!(error = %e, "SSE stream read error");
                        break;
                    }
                };

                buffer.push_str(&String::from_utf8_lossy(&bytes));

                // Process complete SSE lines
                while let Some(line_end) = buffer.find('\n') {
                    let line = buffer[..line_end].trim_end_matches('\r').to_string();
                    buffer = buffer[line_end + 1..].to_string();

                    if line.is_empty() || line.starts_with("event:") {
                        continue;
                    }

                    if let Some(data) = line.strip_prefix("data: ") {
                        if data.trim() == "[DONE]" {
                            // OpenAI-style stream termination
                            for (_, ptc) in tool_call_builders.drain() {
                                if let Some(tc) = ptc.into_tool_call() {
                                    let _ = tx.send(StreamChunk::ToolCall(tc)).await;
                                }
                            }
                            let _ = tx.send(StreamChunk::Done).await;
                            return;
                        }

                        let parsed = match serde_json::from_str::<Value>(data) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };

                        // ── Extract tool call deltas (stateful, handled here) ──
                        let tool_call_handled = extract_tool_call_deltas(
                            &parsed, &adapter_name, &mut tool_call_builders,
                        );

                        // ── Extract usage information ──
                        if let Some(usage_chunk) = extract_usage(&parsed, &adapter_name) {
                            let _ = tx.send(usage_chunk).await;
                        }

                        // ── Check for stream termination (Anthropic "message_stop") ──
                        if parsed.get("type").and_then(|t| t.as_str()) == Some("message_stop") {
                            for (_, ptc) in tool_call_builders.drain() {
                                if let Some(tc) = ptc.into_tool_call() {
                                    let _ = tx.send(StreamChunk::ToolCall(tc)).await;
                                }
                            }
                            let _ = tx.send(StreamChunk::Done).await;
                            return;
                        }

                        // ── Extract content/reasoning deltas ──
                        if !tool_call_handled {
                            if let Some(chunk) = extract_content_delta(&parsed, &adapter_name) {
                                let _ = tx.send(chunk).await;
                            }
                        }
                    }
                }
            }

            // Stream ended without explicit termination signal
            for (_, ptc) in tool_call_builders.drain() {
                if let Some(tc) = ptc.into_tool_call() {
                    let _ = tx.send(StreamChunk::ToolCall(tc)).await;
                }
            }
            let _ = tx.send(StreamChunk::Done).await;
        });

        Ok(rx)
    }
}

// ─── SSE Stream Parsing Helpers ───────────────────────────────────────────────

/// Extract incremental tool call deltas from a parsed SSE event.
/// Returns `true` if the event was a tool call delta (so content parsing can be skipped).
fn extract_tool_call_deltas(
    parsed: &Value,
    adapter_name: &str,
    builders: &mut HashMap<usize, PartialToolCall>,
) -> bool {
    if adapter_name == "Anthropic" {
        let event_type = parsed.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match event_type {
            "content_block_start" => {
                if let Some(cb) = parsed.get("content_block") {
                    if cb.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        let idx = parsed.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                        let b = builders.entry(idx).or_insert_with(PartialToolCall::new);
                        if let Some(id) = cb.get("id").and_then(|v| v.as_str()) {
                            b.call_id = Some(id.to_string());
                        }
                        if let Some(name) = cb.get("name").and_then(|v| v.as_str()) {
                            b.function_name = Some(name.to_string());
                        }
                        return true;
                    }
                }
            }
            "content_block_delta" => {
                if let Some(delta) = parsed.get("delta") {
                    if delta.get("type").and_then(|t| t.as_str()) == Some("input_json_delta") {
                        if let Some(partial) = delta.get("partial_json").and_then(|v| v.as_str()) {
                            let idx = parsed.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                            let b = builders.entry(idx).or_insert_with(PartialToolCall::new);
                            b.arguments_buffer.push_str(partial);
                            return true;
                        }
                    }
                }
            }
            _ => {}
        }
    } else {
        // OpenAI/Venus/Gemini format: tool_calls array in choices[].delta
        if let Some(choices) = parsed.get("choices").and_then(|c| c.as_array()) {
            for choice in choices {
                if let Some(delta) = choice.get("delta") {
                    if let Some(tool_calls) = delta.get("tool_calls").and_then(|tc| tc.as_array()) {
                        for tc_delta in tool_calls {
                            let idx = tc_delta.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                            let b = builders.entry(idx).or_insert_with(PartialToolCall::new);
                            if let Some(id) = tc_delta.get("id").and_then(|v| v.as_str()) {
                                b.call_id = Some(id.to_string());
                            }
                            if let Some(func) = tc_delta.get("function") {
                                if let Some(name) = func.get("name").and_then(|n| n.as_str()) {
                                    b.function_name = Some(name.to_string());
                                }
                                if let Some(args) = func.get("arguments").and_then(|a| a.as_str()) {
                                    b.arguments_buffer.push_str(args);
                                }
                            }
                        }
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Extract usage information from a parsed SSE event.
fn extract_usage(parsed: &Value, adapter_name: &str) -> Option<StreamChunk> {
    if adapter_name == "Anthropic" {
        let event_type = parsed.get("type").and_then(|t| t.as_str())?;
        match event_type {
            "message_start" => {
                let usage = parsed.get("message")?.get("usage")?;
                let input = usage.get("input_tokens").and_then(|v| v.as_u64());
                let cached = usage.get("cache_read_input_tokens").and_then(|v| v.as_u64());
                if input.is_some() {
                    return Some(StreamChunk::Usage(super::TokenUsage {
                        prompt_tokens: input,
                        completion_tokens: None,
                        total_tokens: None,
                        cached_tokens: cached,
                    }));
                }
            }
            "message_delta" => {
                let usage = parsed.get("usage")?;
                let input = usage.get("input_tokens").and_then(|v| v.as_u64());
                let output = usage.get("output_tokens").and_then(|v| v.as_u64());
                let cached = usage.get("cache_read_input_tokens").and_then(|v| v.as_u64());
                if input.is_some() || output.is_some() {
                    return Some(StreamChunk::Usage(super::TokenUsage {
                        prompt_tokens: input,
                        completion_tokens: output,
                        total_tokens: match (input, output) {
                            (Some(i), Some(o)) => Some(i + o),
                            _ => None,
                        },
                        cached_tokens: cached,
                    }));
                }
            }
            _ => {}
        }
    } else {
        // OpenAI format usage
        if let Some(usage_obj) = parsed.get("usage") {
            let prompt = usage_obj.get("prompt_tokens").and_then(|v| v.as_u64());
            let completion = usage_obj.get("completion_tokens").and_then(|v| v.as_u64());
            let total = usage_obj.get("total_tokens").and_then(|v| v.as_u64());
            let cached = usage_obj
                .get("prompt_tokens_details")
                .and_then(|d| {
                    // OpenAI uses "cached_tokens", Venus proxy uses "cache_read_tokens"
                    d.get("cached_tokens")
                        .or_else(|| d.get("cache_read_tokens"))
                })
                .and_then(|v| v.as_u64())
                .or_else(|| usage_obj.get("cache_read_input_tokens").and_then(|v| v.as_u64()));

            if prompt.is_some() || completion.is_some() || total.is_some() {
                return Some(StreamChunk::Usage(super::TokenUsage {
                    prompt_tokens: prompt,
                    completion_tokens: completion,
                    total_tokens: total,
                    cached_tokens: cached,
                }));
            }
        }

        // Gemini format usage
        if let Some(usage_meta) = parsed.get("usageMetadata") {
            let prompt = usage_meta.get("promptTokenCount").and_then(|v| v.as_u64());
            let completion = usage_meta.get("candidatesTokenCount").and_then(|v| v.as_u64());
            let cached = usage_meta.get("cachedContentTokenCount").and_then(|v| v.as_u64());

            if prompt.is_some() || completion.is_some() {
                return Some(StreamChunk::Usage(super::TokenUsage {
                    prompt_tokens: prompt,
                    completion_tokens: completion,
                    total_tokens: match (prompt, completion) {
                        (Some(p), Some(c)) => Some(p + c),
                        _ => None,
                    },
                    cached_tokens: cached,
                }));
            }
        }
    }
    None
}

/// Extract content or reasoning deltas from a parsed SSE event.
fn extract_content_delta(parsed: &Value, adapter_name: &str) -> Option<StreamChunk> {
    if adapter_name == "Anthropic" {
        if parsed.get("type").and_then(|t| t.as_str()) != Some("content_block_delta") {
            return None;
        }
        let delta = parsed.get("delta")?;
        match delta.get("type").and_then(|t| t.as_str()) {
            Some("text_delta") => {
                let text = delta.get("text").and_then(|t| t.as_str())?;
                if !text.is_empty() {
                    return Some(StreamChunk::ContentDelta(text.to_string()));
                }
            }
            Some("thinking_delta") => {
                let thinking = delta.get("thinking").and_then(|t| t.as_str())?;
                if !thinking.is_empty() {
                    return Some(StreamChunk::ReasoningDelta(thinking.to_string()));
                }
            }
            _ => {}
        }
    } else {
        // OpenAI/Venus/Gemini format: choices[].delta.content / reasoning_content
        let choices = parsed.get("choices")?.as_array()?;
        let choice = choices.first()?;
        let delta = choice.get("delta")?;

        if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
            if !content.is_empty() {
                return Some(StreamChunk::ContentDelta(content.to_string()));
            }
        }
        if let Some(reasoning) = delta.get("reasoning_content").and_then(|r| r.as_str()) {
            if !reasoning.is_empty() {
                return Some(StreamChunk::ReasoningDelta(reasoning.to_string()));
            }
        }
    }
    None
}

#[async_trait]
impl LlmApi for LlmProvider {
    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        tool_history: &[ToolRound],
        options: Option<&ChatOptions>,
    ) -> Result<ChatResponse> {
        let body = self.build_request_body(messages, tools, tool_history, options);
        let response_body = self.send_request(&body).await?;
        self.adapter.parse_response(&response_body)
    }

    async fn chat_with_tools_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        tool_history: &[ToolRound],
        options: Option<&ChatOptions>,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamChunk>> {
        let mut body = self.build_request_body(messages, tools, tool_history, options);
        // Enable streaming
        body["stream"] = json!(true);
        // stream_options is OpenAI-specific; Anthropic doesn't use it
        if self.adapter.name() != "Anthropic" {
            body["stream_options"] = json!({ "include_usage": true });
        }
        self.send_stream_request(body).await
    }

    fn supports_tools(&self) -> bool {
        true
    }

    fn model_name(&self) -> &str {
        &self.config.model
    }

    fn provider_name(&self) -> &str {
        self.adapter.name()
    }
}

/// Helper for incrementally assembling a tool call from SSE stream deltas.
#[derive(Debug, Default)]
struct PartialToolCall {
    call_id: Option<String>,
    function_name: Option<String>,
    arguments_buffer: String,
}

impl PartialToolCall {
    fn new() -> Self {
        Self::default()
    }

    fn into_tool_call(self) -> Option<ToolCall> {
        let call_id = self.call_id?;
        let function_name = self.function_name?;
        let arguments: Value = serde_json::from_str(&self.arguments_buffer)
            .unwrap_or(json!({}));
        Some(ToolCall {
            call_id,
            function_name,
            arguments,
        })
    }
}
