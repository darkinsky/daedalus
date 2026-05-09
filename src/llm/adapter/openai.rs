//! OpenAI-compatible API adapter.
//!
//! Handles OpenAI, Venus proxy, DeepSeek, and any API that follows the
//! OpenAI chat completions format. This is the default adapter.

use anyhow::Result;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use super::ApiAdapter;
use crate::llm::{
    ChatMessage, ChatOptions, ChatResponse, StreamChunk,
    TokenUsage, ToolCall, ToolRound, VenusExtensions,
};

/// Adapter for OpenAI-compatible APIs (OpenAI, Venus, DeepSeek, etc.).
pub struct OpenAiAdapter;

impl ApiAdapter for OpenAiAdapter {
    fn endpoint(&self, base_url: &str, _model: &str) -> String {
        format!("{}/chat/completions", base_url)
    }

    fn headers(&self, api_key: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", api_key)).unwrap(),
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
        let mut msg_array: Vec<Value> = messages
            .iter()
            .map(|msg| {
                if msg.cache_control.is_some() {
                    json!({
                        "role": msg.role.to_string(),
                        "content": [{
                            "type": "text",
                            "text": msg.content,
                            "cache_control": { "type": "ephemeral" }
                        }]
                    })
                } else {
                    json!({
                        "role": msg.role.to_string(),
                        "content": msg.content,
                    })
                }
            })
            .collect();

        // Replay tool history with prompt caching optimization.
        // Use the FIRST (oldest) truncated round as cache boundary — its content
        // is already at maximum compression and won't change in future rounds,
        // making the cached prefix stable across requests.
        let first_stable_round_idx = tool_history.iter()
            .position(|round| {
                round.responses.iter().any(|r| r.content.contains("...(truncated,"))
            });

        for (round_idx, round) in tool_history.iter().enumerate() {
            // Assistant message with tool_calls
            let tool_calls_json: Vec<Value> = round.calls
                .iter()
                .map(|tc| {
                    json!({
                        "id": tc.call_id,
                        "type": "function",
                        "function": {
                            "name": tc.function_name,
                            "arguments": tc.arguments.to_string(),
                        }
                    })
                })
                .collect();

            let mut assistant_msg = json!({
                "role": "assistant",
                "content": null,
                "tool_calls": tool_calls_json,
            });
            // DeepSeek V4 requires reasoning_content passback
            if let Some(ref reasoning) = round.reasoning_content {
                assistant_msg["reasoning_content"] = json!(reasoning);
            }
            msg_array.push(assistant_msg);

            // Tool response messages
            let is_cache_boundary = first_stable_round_idx == Some(round_idx);
            let resp_count = round.responses.len();
            for (resp_idx, resp) in round.responses.iter().enumerate() {
                if is_cache_boundary && resp_idx == resp_count - 1 {
                    msg_array.push(json!({
                        "role": "tool",
                        "tool_call_id": resp.call_id,
                        "content": [{
                            "type": "text",
                            "text": resp.content,
                            "cache_control": { "type": "ephemeral" }
                        }],
                    }));
                } else {
                    msg_array.push(json!({
                        "role": "tool",
                        "tool_call_id": resp.call_id,
                        "content": resp.content,
                    }));
                }
            }
        }

        let mut body = json!({
            "model": model,
            "messages": msg_array,
        });

        // Add tool definitions
        if !tools.is_empty() {
            body["tools"] = json!(tools);
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

        // Merge Venus extensions: request-level overrides config-level defaults
        let request_venus = options.map(|o| &o.venus);
        let merged = match request_venus {
            Some(overrides) => config_venus.merge_with_overrides(overrides),
            None => config_venus.clone(),
        };

        if let Some(enabled) = merged.thinking_enabled {
            body["thinking_enabled"] = json!(enabled);
        }
        if let Some(tokens) = merged.thinking_tokens {
            body["thinking_tokens"] = json!(tokens);
        }
        if let Some(ref effort) = merged.reasoning_effort {
            body["reasoning_effort"] = json!(effort.to_string());
        }

        body
    }

    fn parse_response(&self, response_body: &Value) -> Result<ChatResponse> {
        let message = extract_first_message(response_body)?;
        let content = parse_content(&message);
        let reasoning_content = parse_reasoning(&message, &content);
        let tool_calls = parse_tool_calls(&message);
        let usage = parse_usage(response_body);

        Ok(ChatResponse { content, reasoning_content, usage, tool_calls })
    }

    fn parse_stream_event(&self, data: &str) -> Option<StreamChunk> {
        if data.trim() == "[DONE]" {
            return Some(StreamChunk::Done);
        }

        let parsed: Value = serde_json::from_str(data).ok()?;

        // Check for usage (typically in the last chunk)
        if let Some(usage) = parse_usage(&parsed) {
            return Some(StreamChunk::Usage(usage));
        }

        let choices = parsed.get("choices")?.as_array()?;
        let choice = choices.first()?;
        let delta = choice.get("delta")?;

        // Content delta
        if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
            if !content.is_empty() {
                return Some(StreamChunk::ContentDelta(content.to_string()));
            }
        }

        // Reasoning content delta
        if let Some(reasoning) = delta.get("reasoning_content").and_then(|r| r.as_str()) {
            if !reasoning.is_empty() {
                return Some(StreamChunk::ReasoningDelta(reasoning.to_string()));
            }
        }

        None
    }

    fn name(&self) -> &str {
        "OpenAI"
    }
}

// ── Shared parsing helpers ──

/// Extract the first message object from the response choices array.
fn extract_first_message(response_body: &Value) -> Result<Value> {
    let choices = response_body
        .get("choices")
        .and_then(|c| c.as_array())
        .ok_or_else(|| anyhow::anyhow!("Missing 'choices' in response"))?;

    let first_choice = choices
        .first()
        .ok_or_else(|| anyhow::anyhow!("Empty 'choices' array in response"))?;

    first_choice
        .get("message")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Missing 'message' in first choice"))
}

/// Extract the text content from a message object.
fn parse_content(message: &Value) -> String {
    message
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Extract reasoning/thinking content from a message.
///
/// First checks the `reasoning_content` field (Venus proxy standard),
/// then falls back to `<think>` tag extraction (DeepSeek-R1 style).
fn parse_reasoning(message: &Value, content: &str) -> Option<String> {
    message
        .get("reasoning_content")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| extract_think_content(content))
}

/// Parse tool calls from a message object.
pub(crate) fn parse_tool_calls(message: &Value) -> Vec<ToolCall> {
    message
        .get("tool_calls")
        .and_then(|tc| tc.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|tc| {
                    let call_id = tc.get("id")?.as_str()?.to_string();
                    let func = tc.get("function")?;
                    let function_name = func.get("name")?.as_str()?.to_string();
                    let arguments_str = func.get("arguments")?.as_str().unwrap_or("{}");
                    let arguments: Value =
                        serde_json::from_str(arguments_str).unwrap_or(json!({}));
                    Some(ToolCall { call_id, function_name, arguments })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse token usage statistics from the response body.
///
/// Extracts cached token counts from multiple possible locations:
/// - `usage.prompt_tokens_details.cached_tokens` (OpenAI format)
/// - `usage.cache_read_input_tokens` (Anthropic format)
pub(crate) fn parse_usage(response_body: &Value) -> Option<TokenUsage> {
    let usage_obj = response_body.get("usage")?;
    let prompt = usage_obj.get("prompt_tokens").and_then(|v| v.as_u64());
    let completion = usage_obj.get("completion_tokens").and_then(|v| v.as_u64());
    let total = usage_obj.get("total_tokens").and_then(|v| v.as_u64());

    let cached = usage_obj
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .or_else(|| usage_obj.get("cache_read_input_tokens").and_then(|v| v.as_u64()));

    if prompt.is_some() || completion.is_some() || total.is_some() {
        Some(TokenUsage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: total,
            cached_tokens: cached,
        })
    } else {
        None
    }
}

/// Extract thinking content from `<think>...</think>` tags (DeepSeek-R1 style).
fn extract_think_content(content: &str) -> Option<String> {
    let start_tag = "<think>";
    let end_tag = "</think>";
    let start = content.find(start_tag)?;
    let end = content.find(end_tag)?;
    if end > start {
        let think_content = &content[start + start_tag.len()..end];
        let trimmed = think_content.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{CacheControl, ReasoningEffort, ToolResponse};

    #[test]
    fn test_extract_think_content() {
        let content = "<think>I need to think about this</think>Here is my answer";
        assert_eq!(
            extract_think_content(content),
            Some("I need to think about this".to_string())
        );

        let content = "<think></think>Answer";
        assert_eq!(extract_think_content(content), None);

        let content = "<think>  \n  </think>Answer";
        assert_eq!(extract_think_content(content), None);

        let content = "Just a normal response";
        assert_eq!(extract_think_content(content), None);
    }

    #[test]
    fn test_parse_response_basic() {
        let adapter = OpenAiAdapter;
        let body = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Hello!"
                }
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        });

        let resp = adapter.parse_response(&body).unwrap();
        assert_eq!(resp.content, "Hello!");
        assert!(resp.reasoning_content.is_none());
        assert!(resp.tool_calls.is_empty());
        assert_eq!(resp.usage.as_ref().unwrap().total_tokens, Some(15));
    }

    #[test]
    fn test_parse_response_with_reasoning() {
        let adapter = OpenAiAdapter;
        let body = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "The answer is 42.",
                    "reasoning_content": "Let me think step by step..."
                }
            }]
        });

        let resp = adapter.parse_response(&body).unwrap();
        assert_eq!(resp.content, "The answer is 42.");
        assert_eq!(resp.reasoning_content.unwrap(), "Let me think step by step...");
    }

    #[test]
    fn test_parse_response_with_tool_calls() {
        let adapter = OpenAiAdapter;
        let body = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_123",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\": \"Beijing\"}"
                        }
                    }]
                }
            }]
        });

        let resp = adapter.parse_response(&body).unwrap();
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].call_id, "call_123");
        assert_eq!(resp.tool_calls[0].function_name, "get_weather");
        assert_eq!(resp.tool_calls[0].arguments["city"], "Beijing");
    }

    #[test]
    fn test_build_body_with_thinking() {
        let adapter = OpenAiAdapter;
        let messages = vec![ChatMessage::user("Hello")];
        let config_venus = VenusExtensions {
            thinking_enabled: Some(true),
            thinking_tokens: Some(4096),
            reasoning_effort: None,
        };

        let body = adapter.build_body(
            "claude-sonnet-4-20250514", &messages, &[], &[], None, &config_venus,
        );

        assert_eq!(body["model"], "claude-sonnet-4-20250514");
        assert_eq!(body["thinking_enabled"], true);
        assert_eq!(body["thinking_tokens"], 4096);
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn test_build_body_with_reasoning_effort() {
        let adapter = OpenAiAdapter;
        let messages = vec![ChatMessage::user("Hello")];
        let config_venus = VenusExtensions {
            thinking_enabled: None,
            thinking_tokens: None,
            reasoning_effort: Some(ReasoningEffort::High),
        };

        let body = adapter.build_body("o3-mini", &messages, &[], &[], None, &config_venus);

        assert_eq!(body["model"], "o3-mini");
        assert_eq!(body["reasoning_effort"], "high");
        assert!(body.get("thinking_enabled").is_none());
    }

    #[test]
    fn test_request_level_options_override_config() {
        let adapter = OpenAiAdapter;
        let messages = vec![ChatMessage::user("Hello")];
        let config_venus = VenusExtensions {
            thinking_enabled: Some(true),
            thinking_tokens: Some(2048),
            reasoning_effort: Some(ReasoningEffort::Low),
        };
        let opts = ChatOptions {
            venus: VenusExtensions {
                thinking_enabled: Some(false),
                thinking_tokens: Some(8192),
                reasoning_effort: Some(ReasoningEffort::High),
            },
            ..Default::default()
        };

        let body = adapter.build_body(
            "test-model", &messages, &[], &[], Some(&opts), &config_venus,
        );
        assert_eq!(body["thinking_enabled"], false);
        assert_eq!(body["thinking_tokens"], 8192);
        assert_eq!(body["reasoning_effort"], "high");
    }

    #[test]
    fn test_build_body_with_tool_history() {
        let adapter = OpenAiAdapter;
        let messages = vec![ChatMessage::user("What's the weather?")];
        let config_venus = VenusExtensions::default();

        let tool_calls = vec![ToolCall {
            call_id: "call_1".to_string(),
            function_name: "get_weather".to_string(),
            arguments: json!({"city": "Beijing"}),
        }];
        let tool_responses = vec![ToolResponse::new("call_1", "Sunny, 25°C")];
        let tool_history = vec![ToolRound {
            calls: tool_calls,
            responses: tool_responses,
            reasoning_content: None,
        }];

        let body = adapter.build_body(
            "test-model", &messages, &[], &tool_history, None, &config_venus,
        );
        let msgs = body["messages"].as_array().unwrap();

        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["role"], "assistant");
        assert!(msgs[1]["tool_calls"].is_array());
        assert_eq!(msgs[2]["role"], "tool");
        assert_eq!(msgs[2]["tool_call_id"], "call_1");
    }

    #[test]
    fn test_build_body_with_cache_control() {
        let adapter = OpenAiAdapter;
        let messages = vec![
            ChatMessage::system("You are helpful.")
                .with_cache_control(CacheControl::Ephemeral),
            ChatMessage::user("Hello"),
        ];
        let config_venus = VenusExtensions::default();

        let body = adapter.build_body(
            "claude-sonnet-4-20250514", &messages, &[], &[], None, &config_venus,
        );
        let msgs = body["messages"].as_array().unwrap();

        // System message should use content-block format with cache_control
        assert_eq!(msgs[0]["role"], "system");
        let content_blocks = msgs[0]["content"].as_array()
            .expect("System message with cache_control should use content-block format");
        assert_eq!(content_blocks.len(), 1);
        assert_eq!(content_blocks[0]["type"], "text");
        assert_eq!(content_blocks[0]["text"], "You are helpful.");
        assert_eq!(content_blocks[0]["cache_control"]["type"], "ephemeral");

        // User message should use plain string format
        assert_eq!(msgs[1]["role"], "user");
        assert!(msgs[1]["content"].is_string());
    }

    #[test]
    fn test_parse_usage_with_cached_tokens_openai_format() {
        let body = json!({
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 20,
                "total_tokens": 120,
                "prompt_tokens_details": {
                    "cached_tokens": 80
                }
            }
        });

        let usage = parse_usage(&body).unwrap();
        assert_eq!(usage.prompt_tokens, Some(100));
        assert_eq!(usage.cached_tokens, Some(80));
    }

    #[test]
    fn test_parse_usage_with_cached_tokens_anthropic_format() {
        let body = json!({
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 20,
                "total_tokens": 120,
                "cache_read_input_tokens": 75
            }
        });

        let usage = parse_usage(&body).unwrap();
        assert_eq!(usage.prompt_tokens, Some(100));
        assert_eq!(usage.cached_tokens, Some(75));
    }
}
