//! Anthropic Messages API adapter.
//!
//! Handles direct connections to the Anthropic API (not via Venus proxy).
//! Key differences from OpenAI format:
//! - System message is a separate top-level field, not in the messages array
//! - Tool use is via content blocks (`type: "tool_use"` / `type: "tool_result"`)
//! - Authentication uses `x-api-key` header + `anthropic-version` header
//! - Streaming uses different event types (`content_block_delta`, etc.)

use anyhow::Result;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde_json::{json, Value};

use super::ApiAdapter;
use crate::llm::{
    ChatMessage, ChatOptions, ChatResponse, ChatRole, StreamChunk,
    TokenUsage, ToolCall, ToolRound, VenusExtensions,
};

/// Adapter for the Anthropic Messages API (direct connection).
pub struct AnthropicAdapter;

impl ApiAdapter for AnthropicAdapter {
    fn endpoint(&self, base_url: &str, _model: &str) -> String {
        format!("{}/messages", base_url)
    }

    fn headers(&self, api_key: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "x-api-key",
            HeaderValue::from_bytes(api_key.as_bytes())
                .expect("API key contains invalid header bytes"),
        );
        headers.insert(
            "anthropic-version",
            HeaderValue::from_static("2023-06-01"),
        );
        headers
    }

    fn build_body(
        &self,
        model: &str,
        messages: &[ChatMessage],
        tools: &[Value],
        tool_history: &[ToolRound],
        options: Option<&ChatOptions>,
        config_venus: &VenusExtensions,
    ) -> Value {
        // Separate system message from conversation messages
        let system_content: Option<String> = messages
            .iter()
            .find(|m| m.role == ChatRole::System)
            .map(|m| m.content.clone());

        // Build messages array (excluding system)
        let mut msg_array: Vec<Value> = messages
            .iter()
            .filter(|m| m.role != ChatRole::System)
            .map(|msg| {
                let role = match msg.role {
                    ChatRole::User => "user",
                    ChatRole::Assistant => "assistant",
                    _ => "user", // Tool messages mapped to user with tool_result blocks
                };

                if msg.cache_control.is_some() {
                    json!({
                        "role": role,
                        "content": [{
                            "type": "text",
                            "text": msg.content,
                            "cache_control": { "type": "ephemeral" }
                        }]
                    })
                } else {
                    json!({
                        "role": role,
                        "content": msg.content,
                    })
                }
            })
            .collect();

        // Replay tool history in Anthropic format with prompt caching optimization.
        // Use the FIRST (oldest) truncated round as cache boundary — its content
        // is already at maximum compression and won't change in future rounds,
        // making the cached prefix stable across requests.
        let first_stable_round_idx = tool_history.iter()
            .position(|round| {
                round.responses.iter().any(|r| r.content.contains("...(truncated,"))
            });

        for (round_idx, round) in tool_history.iter().enumerate() {
            // Assistant message with tool_use content blocks
            let content_blocks: Vec<Value> = round.calls
                .iter()
                .map(|tc| {
                    json!({
                        "type": "tool_use",
                        "id": tc.call_id,
                        "name": tc.function_name,
                        "input": tc.arguments,
                    })
                })
                .collect();

            // If there was reasoning, prepend as text block
            if let Some(ref _reasoning) = round.reasoning_content {
                // Anthropic handles thinking internally, no need to pass back
            }

            msg_array.push(json!({
                "role": "assistant",
                "content": content_blocks,
            }));

            // User message with tool_result content blocks
            // Mark the first stable (truncated) round as cache boundary
            let is_cache_boundary = first_stable_round_idx == Some(round_idx);
            let result_blocks: Vec<Value> = round.responses
                .iter()
                .enumerate()
                .map(|(resp_idx, resp)| {
                    if is_cache_boundary && resp_idx == round.responses.len() - 1 {
                        json!({
                            "type": "tool_result",
                            "tool_use_id": resp.call_id,
                            "content": resp.content,
                            "cache_control": { "type": "ephemeral" }
                        })
                    } else {
                        json!({
                            "type": "tool_result",
                            "tool_use_id": resp.call_id,
                            "content": resp.content,
                        })
                    }
                })
                .collect();

            msg_array.push(json!({
                "role": "user",
                "content": result_blocks,
            }));
        }

        let mut body = json!({
            "model": model,
            "messages": msg_array,
            "max_tokens": 8192,
        });

        // Add system message as top-level field
        if let Some(system) = system_content {
            if messages.iter().any(|m| m.role == ChatRole::System && m.cache_control.is_some()) {
                body["system"] = json!([{
                    "type": "text",
                    "text": system,
                    "cache_control": { "type": "ephemeral" }
                }]);
            } else {
                body["system"] = json!(system);
            }
        }

        // Add tool definitions (Anthropic format)
        if !tools.is_empty() {
            let anthropic_tools: Vec<Value> = tools
                .iter()
                .filter_map(|t| {
                    let func = t.get("function")?;
                    Some(json!({
                        "name": func.get("name")?,
                        "description": func.get("description").unwrap_or(&json!("")),
                        "input_schema": func.get("parameters").unwrap_or(&json!({"type": "object"})),
                    }))
                })
                .collect();
            body["tools"] = json!(anthropic_tools);
        }

        // Apply standard chat options
        if let Some(opts) = options {
            if let Some(temp) = opts.temperature {
                body["temperature"] = json!(temp);
            }
            if let Some(max_tokens) = opts.max_tokens {
                body["max_tokens"] = json!(max_tokens);
            }
            if let Some(top_p) = opts.top_p {
                body["top_p"] = json!(top_p);
            }
        }

        // Apply thinking parameters (Anthropic extended thinking)
        let request_venus = options.map(|o| &o.venus);
        let merged = match request_venus {
            Some(overrides) => config_venus.merge_with_overrides(overrides),
            None => config_venus.clone(),
        };

        if let Some(true) = merged.thinking_enabled {
            let budget = merged.thinking_tokens.unwrap_or(4096);
            body["thinking"] = json!({
                "type": "enabled",
                "budget_tokens": budget,
            });
        }

        body
    }

    fn parse_response(&self, response_body: &Value) -> Result<ChatResponse> {
        let content_blocks = response_body
            .get("content")
            .and_then(|c| c.as_array())
            .ok_or_else(|| anyhow::anyhow!("Missing 'content' in Anthropic response"))?;

        let mut content = String::new();
        let mut reasoning_content: Option<String> = None;
        let mut tool_calls = Vec::new();

        for block in content_blocks {
            match block.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                        content.push_str(text);
                    }
                }
                Some("thinking") => {
                    if let Some(thinking) = block.get("thinking").and_then(|t| t.as_str()) {
                        reasoning_content = Some(thinking.to_string());
                    }
                }
                Some("tool_use") => {
                    if let (Some(id), Some(name), Some(input)) = (
                        block.get("id").and_then(|v| v.as_str()),
                        block.get("name").and_then(|v| v.as_str()),
                        block.get("input"),
                    ) {
                        tool_calls.push(ToolCall {
                            call_id: id.to_string(),
                            function_name: name.to_string(),
                            arguments: input.clone(),
                        });
                    }
                }
                _ => {}
            }
        }

        // Parse usage
        let usage = response_body.get("usage").map(|u| {
            let input = u.get("input_tokens").and_then(|v| v.as_u64());
            let output = u.get("output_tokens").and_then(|v| v.as_u64());
            let cached = u.get("cache_read_input_tokens").and_then(|v| v.as_u64());
            TokenUsage {
                prompt_tokens: input,
                completion_tokens: output,
                total_tokens: match (input, output) {
                    (Some(i), Some(o)) => Some(i + o),
                    _ => None,
                },
                cached_tokens: cached,
            }
        });

        Ok(ChatResponse { content, reasoning_content, usage, tool_calls })
    }

    fn parse_stream_event(&self, data: &str) -> Option<StreamChunk> {
        // Anthropic SSE format uses event types
        let parsed: Value = serde_json::from_str(data).ok()?;

        let event_type = parsed.get("type").and_then(|t| t.as_str())?;

        match event_type {
            "content_block_delta" => {
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
                    Some("input_json_delta") => {
                        // Tool call argument fragment — handled by PartialToolCall accumulator
                    }
                    _ => {}
                }
            }
            "content_block_start" => {
                let content_block = parsed.get("content_block")?;
                if content_block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                    // Tool call start — will be accumulated
                }
            }
            "message_delta" => {
                // May contain usage info
                if let Some(usage) = parsed.get("usage") {
                    let output = usage.get("output_tokens").and_then(|v| v.as_u64());
                    if output.is_some() {
                        return Some(StreamChunk::Usage(TokenUsage {
                            prompt_tokens: None,
                            completion_tokens: output,
                            total_tokens: None,
                            cached_tokens: None,
                        }));
                    }
                }
            }
            "message_stop" => {
                return Some(StreamChunk::Done);
            }
            _ => {}
        }

        None
    }

    fn stream_done_signal(&self) -> &str {
        // Anthropic uses event-based signaling, not a data sentinel.
        // The parse_stream_event handles "message_stop" directly.
        // This is a fallback that won't normally match.
        "event: message_stop"
    }

    fn name(&self) -> &str {
        "Anthropic"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_anthropic_endpoint() {
        let adapter = AnthropicAdapter;
        assert_eq!(
            adapter.endpoint("https://api.anthropic.com/v1", "claude-sonnet-4-20250514"),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn test_anthropic_headers() {
        let adapter = AnthropicAdapter;
        let headers = adapter.headers("test-key");
        assert_eq!(headers.get("x-api-key").unwrap(), "test-key");
        assert_eq!(headers.get("anthropic-version").unwrap(), "2023-06-01");
    }

    #[test]
    fn test_anthropic_parse_response() {
        let adapter = AnthropicAdapter;
        let body = json!({
            "content": [
                {"type": "text", "text": "Hello!"}
            ],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5
            }
        });

        let resp = adapter.parse_response(&body).unwrap();
        assert_eq!(resp.content, "Hello!");
        assert!(resp.tool_calls.is_empty());
        assert_eq!(resp.usage.as_ref().unwrap().prompt_tokens, Some(10));
        assert_eq!(resp.usage.as_ref().unwrap().completion_tokens, Some(5));
    }

    #[test]
    fn test_anthropic_parse_tool_use() {
        let adapter = AnthropicAdapter;
        let body = json!({
            "content": [
                {
                    "type": "tool_use",
                    "id": "toolu_123",
                    "name": "get_weather",
                    "input": {"city": "Beijing"}
                }
            ],
            "usage": {
                "input_tokens": 20,
                "output_tokens": 10
            }
        });

        let resp = adapter.parse_response(&body).unwrap();
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].call_id, "toolu_123");
        assert_eq!(resp.tool_calls[0].function_name, "get_weather");
        assert_eq!(resp.tool_calls[0].arguments["city"], "Beijing");
    }

    #[test]
    fn test_anthropic_build_body_system_separate() {
        let adapter = AnthropicAdapter;
        let messages = vec![
            ChatMessage::system("You are helpful."),
            ChatMessage::user("Hello"),
        ];
        let config_venus = VenusExtensions::default();

        let body = adapter.build_body(
            "claude-sonnet-4-20250514", &messages, &[], &[], None, &config_venus,
        );

        // System should be a top-level field
        assert_eq!(body["system"], "You are helpful.");
        // Messages should only contain user message
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
    }

    #[test]
    fn test_anthropic_build_body_with_thinking() {
        let adapter = AnthropicAdapter;
        let messages = vec![ChatMessage::user("Think about this")];
        let config_venus = VenusExtensions {
            thinking_enabled: Some(true),
            thinking_tokens: Some(8192),
            reasoning_effort: None,
        };

        let body = adapter.build_body(
            "claude-sonnet-4-20250514", &messages, &[], &[], None, &config_venus,
        );

        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 8192);
    }
}
