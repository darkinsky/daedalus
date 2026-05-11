//! Venus LLM Proxy adapter.
//!
//! Venus (v2.open.venus.oa.com) is a unified LLM proxy that provides an
//! OpenAI-compatible interface for all models (OpenAI, Claude, Gemini,
//! DeepSeek, VenusLLMServing, etc.).
//!
//! Key differences from plain OpenAI adapter:
//! - Claude thinking mode: `thinking_enabled`/`thinking_tokens`/`reasoning_effort`
//!   are top-level body fields (not nested under `thinking` object)
//! - Claude thinking mode requires `temperature: 1` and no `top_p`
//! - `reasoning_effort` and `thinking_tokens` are mutually exclusive
//! - Supports `Venus-Sticky-Routing` header for prompt cache affinity
//! - Supports `Venus-Session-Id` header for session-based routing
//! - Claude models need `max_tokens` to be explicitly set
//!
//! Reference docs:
//! - OpenAI-compatible: https://iwiki.woa.com/p/4009937875
//! - Anthropic native:  https://iwiki.woa.com/p/4017340682

use anyhow::Result;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use super::ApiAdapter;
use super::openai::{parse_tool_calls, parse_usage};
use crate::llm::{
    ChatMessage, ChatOptions, ChatResponse, StreamChunk,
    ToolRound, VenusExtensions,
};

/// Default max_tokens for Claude models when thinking is enabled.
/// Claude requires explicit max_tokens; this is a safe default for tool-use agents.
const CLAUDE_DEFAULT_MAX_TOKENS: u32 = 16384;

/// Adapter for Venus LLM Proxy (OpenAI-compatible format with extensions).
///
/// Handles all models through Venus's unified `/chat/completions` endpoint:
/// - OpenAI models (GPT-4o, o3, etc.)
/// - Anthropic Claude models (with thinking extensions)
/// - Google Gemini models (with thinking extensions)
/// - DeepSeek models (with reasoning_content passback)
/// - VenusLLMServing models (Qwen, GLM, etc.)
pub struct VenusAdapter;

impl VenusAdapter {
    /// Check if the model is a Claude model (case-insensitive).
    fn is_claude_model(model: &str) -> bool {
        let lower = model.to_lowercase();
        lower.contains("claude")
    }

    /// Check if thinking mode is enabled in the merged Venus extensions.
    fn is_thinking_enabled(venus: &VenusExtensions) -> bool {
        // thinking is enabled if:
        // 1. thinking_enabled is explicitly true, OR
        // 2. reasoning_effort is set (implicitly enables adaptive thinking)
        venus.thinking_enabled == Some(true) || venus.reasoning_effort.is_some()
    }
}

impl ApiAdapter for VenusAdapter {
    fn endpoint(&self, base_url: &str, _model: &str) -> String {
        format!("{}/chat/completions", base_url)
    }

    fn headers(&self, api_key: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_bytes(format!("Bearer {}", api_key).as_bytes())
                .expect("API key contains invalid header bytes"),
        );
        // Enable sticky routing by token for prompt cache affinity.
        // This ensures consecutive requests from the same token hit the same
        // backend, maximizing prompt cache hit rate.
        headers.insert(
            "Venus-Sticky-Routing",
            HeaderValue::from_static("token"),
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
        // Build messages array with prompt cache support.
        // Venus proxy supports content block array format with cache_control
        // for Claude models (same as direct Anthropic API). When a message has
        // cache_control set, we use the content block format to mark it as a
        // cache breakpoint. Combined with Venus-Sticky-Routing header (set in
        // headers()), this enables effective prompt caching.
        //
        // IMPORTANT: Venus/Claude rejects messages with empty content
        // ("message has no content" error). Filter them out to be robust
        // against memory strategies that may produce empty assistant messages.
        let mut msg_array: Vec<Value> = messages
            .iter()
            .filter(|msg| !msg.content.is_empty())
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
        // Use the FIRST truncated round as cache boundary — its content is already
        // at maximum compression and won't change in future rounds, making the
        // cached prefix stable across requests.
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

            // Tool response messages with cache boundary on first stable round
            let is_cache_boundary = first_stable_round_idx == Some(round_idx);
            let resp_count = round.responses.len();
            for (resp_idx, resp) in round.responses.iter().enumerate() {
                if is_cache_boundary && resp_idx == resp_count - 1 {
                    // Mark last response of first stable round as cache breakpoint
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

        // Merge Venus extensions: request-level overrides config-level defaults
        let request_venus = options.map(|o| &o.venus);
        let merged = match request_venus {
            Some(overrides) => config_venus.merge_with_overrides(overrides),
            None => config_venus.clone(),
        };

        let is_claude = Self::is_claude_model(model);
        let thinking_active = Self::is_thinking_enabled(&merged);

        // Apply standard chat options (with Claude thinking constraints)
        if let Some(opts) = options {
            if let Some(temp) = opts.temperature {
                // Claude thinking mode requires temperature = 1
                if is_claude && thinking_active {
                    body["temperature"] = json!(1);
                } else {
                    body["temperature"] = json!(temp);
                }
            }
            if let Some(max_tokens) = opts.max_tokens {
                body["max_tokens"] = json!(max_tokens);
            }
            // Claude thinking mode: top_p must not be set
            if !thinking_active || !is_claude {
                if let Some(top_p) = opts.top_p {
                    body["top_p"] = json!(top_p);
                }
            }
        }

        // For Claude with thinking, ensure temperature is set to 1
        if is_claude && thinking_active && body.get("temperature").is_none() {
            body["temperature"] = json!(1);
        }

        // Ensure max_tokens is set for Claude (required by the API)
        if is_claude && body.get("max_tokens").is_none() {
            body["max_tokens"] = json!(CLAUDE_DEFAULT_MAX_TOKENS);
        }

        // Apply Venus thinking extensions
        // reasoning_effort and thinking_tokens are mutually exclusive
        if let Some(ref effort) = merged.reasoning_effort {
            // reasoning_effort implicitly enables adaptive thinking
            body["reasoning_effort"] = json!(effort.to_string());
            // Don't set thinking_enabled or thinking_tokens when using reasoning_effort
        } else {
            // Manual thinking mode
            if let Some(enabled) = merged.thinking_enabled {
                body["thinking_enabled"] = json!(enabled);
            }
            if let Some(tokens) = merged.thinking_tokens {
                body["thinking_tokens"] = json!(tokens);
            }
        }

        body
    }

    fn parse_response(&self, response_body: &Value) -> Result<ChatResponse> {
        let choices = response_body
            .get("choices")
            .and_then(|c| c.as_array())
            .ok_or_else(|| anyhow::anyhow!("Missing 'choices' in response"))?;

        let first_choice = choices
            .first()
            .ok_or_else(|| anyhow::anyhow!("Empty 'choices' array in response"))?;

        let message = first_choice
            .get("message")
            .ok_or_else(|| anyhow::anyhow!("Missing 'message' in first choice"))?;

        let content = message
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let reasoning_content = message
            .get("reasoning_content")
            .and_then(|v| v.as_str())
            .map(String::from);

        let tool_calls = parse_tool_calls(message);
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

        // Reasoning content delta (Venus returns thinking as reasoning_content)
        if let Some(reasoning) = delta.get("reasoning_content").and_then(|r| r.as_str()) {
            if !reasoning.is_empty() {
                return Some(StreamChunk::ReasoningDelta(reasoning.to_string()));
            }
        }

        None
    }

    fn name(&self) -> &str {
        "Venus"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ReasoningEffort, ToolCall, ToolResponse};

    #[test]
    fn test_venus_endpoint() {
        let adapter = VenusAdapter;
        assert_eq!(
            adapter.endpoint("http://v2.open.venus.oa.com/llmproxy", "gpt-4o"),
            "http://v2.open.venus.oa.com/llmproxy/chat/completions"
        );
    }

    #[test]
    fn test_venus_headers_include_sticky_routing() {
        let adapter = VenusAdapter;
        let headers = adapter.headers("test-token");
        assert_eq!(
            headers.get("Authorization").unwrap(),
            "Bearer test-token"
        );
        assert_eq!(
            headers.get("Venus-Sticky-Routing").unwrap(),
            "token"
        );
    }

    #[test]
    fn test_venus_claude_thinking_forces_temperature_1() {
        let adapter = VenusAdapter;
        let messages = vec![ChatMessage::user("Hello")];
        let config_venus = VenusExtensions {
            thinking_enabled: Some(true),
            thinking_tokens: Some(4096),
            reasoning_effort: None,
        };
        let opts = ChatOptions {
            temperature: Some(0.7),
            ..Default::default()
        };

        let body = adapter.build_body(
            "claude-sonnet-4-6", &messages, &[], &[], Some(&opts), &config_venus,
        );

        // Temperature must be forced to 1 for Claude thinking mode
        assert_eq!(body["temperature"], 1);
        assert_eq!(body["thinking_enabled"], true);
        assert_eq!(body["thinking_tokens"], 4096);
    }

    #[test]
    fn test_venus_claude_reasoning_effort_no_thinking_tokens() {
        let adapter = VenusAdapter;
        let messages = vec![ChatMessage::user("Hello")];
        let config_venus = VenusExtensions {
            thinking_enabled: None,
            thinking_tokens: None,
            reasoning_effort: Some(ReasoningEffort::High),
        };

        let body = adapter.build_body(
            "claude-sonnet-4-6", &messages, &[], &[], None, &config_venus,
        );

        // reasoning_effort should be set, thinking_enabled/thinking_tokens should NOT
        assert_eq!(body["reasoning_effort"], "high");
        assert!(body.get("thinking_enabled").is_none());
        assert!(body.get("thinking_tokens").is_none());
        // Temperature forced to 1 (reasoning_effort implies thinking)
        assert_eq!(body["temperature"], 1);
        // max_tokens should be set for Claude
        assert_eq!(body["max_tokens"], CLAUDE_DEFAULT_MAX_TOKENS);
    }

    #[test]
    fn test_venus_non_claude_model_no_temperature_forcing() {
        let adapter = VenusAdapter;
        let messages = vec![ChatMessage::user("Hello")];
        let config_venus = VenusExtensions {
            thinking_enabled: Some(true),
            thinking_tokens: Some(2048),
            reasoning_effort: None,
        };
        let opts = ChatOptions {
            temperature: Some(0.5),
            ..Default::default()
        };

        let body = adapter.build_body(
            "gemini-2.5-flash", &messages, &[], &[], Some(&opts), &config_venus,
        );

        // Non-Claude models keep their temperature
        assert_eq!(body["temperature"], 0.5);
    }

    #[test]
    fn test_venus_claude_no_top_p_in_thinking_mode() {
        let adapter = VenusAdapter;
        let messages = vec![ChatMessage::user("Hello")];
        let config_venus = VenusExtensions {
            thinking_enabled: Some(true),
            thinking_tokens: Some(4096),
            reasoning_effort: None,
        };
        let opts = ChatOptions {
            temperature: Some(0.7),
            top_p: Some(0.9),
            ..Default::default()
        };

        let body = adapter.build_body(
            "claude-sonnet-4-6", &messages, &[], &[], Some(&opts), &config_venus,
        );

        // top_p must NOT be set for Claude thinking mode
        assert!(body.get("top_p").is_none());
    }

    #[test]
    fn test_venus_parse_response_with_reasoning() {
        let adapter = VenusAdapter;
        let body = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "The answer is 42.",
                    "reasoning_content": "Let me think step by step..."
                }
            }],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 50,
                "total_tokens": 150,
                "prompt_tokens_details": {
                    "cache_read_tokens": 80,
                    "cache_creation_tokens": 10
                }
            }
        });

        let resp = adapter.parse_response(&body).unwrap();
        assert_eq!(resp.content, "The answer is 42.");
        assert_eq!(resp.reasoning_content.unwrap(), "Let me think step by step...");
        let usage = resp.usage.unwrap();
        assert_eq!(usage.prompt_tokens, Some(100));
        assert_eq!(usage.cached_tokens, Some(80));
    }

    #[test]
    fn test_venus_cache_control_content_block_format() {
        use crate::llm::CacheControl;
        let adapter = VenusAdapter;
        let messages = vec![
            ChatMessage::system("You are helpful."),
            ChatMessage::user("Hello")
                .with_cache_control(CacheControl::Ephemeral),
        ];
        let config_venus = VenusExtensions::default();

        let body = adapter.build_body(
            "claude-sonnet-4-6", &messages, &[], &[], None, &config_venus,
        );

        let msgs = body["messages"].as_array().unwrap();
        // System message: plain string (no cache_control)
        assert!(msgs[0]["content"].is_string());
        // User message: content block array with cache_control
        let content_blocks = msgs[1]["content"].as_array()
            .expect("User message with cache_control should use content-block format");
        assert_eq!(content_blocks[0]["type"], "text");
        assert_eq!(content_blocks[0]["text"], "Hello");
        assert_eq!(content_blocks[0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn test_venus_tool_history_cache_boundary() {
        let adapter = VenusAdapter;
        let messages = vec![ChatMessage::user("Hello")];
        let config_venus = VenusExtensions::default();

        let tool_history = vec![
            ToolRound {
                calls: vec![ToolCall {
                    call_id: "call_1".to_string(),
                    function_name: "read_file".to_string(),
                    arguments: json!({"path": "foo.rs"}),
                }],
                responses: vec![ToolResponse::new("call_1", "content...(truncated, 500 chars)")],
                reasoning_content: None,
            },
            ToolRound {
                calls: vec![ToolCall {
                    call_id: "call_2".to_string(),
                    function_name: "read_file".to_string(),
                    arguments: json!({"path": "bar.rs"}),
                }],
                responses: vec![ToolResponse::new("call_2", "full content here")],
                reasoning_content: None,
            },
        ];

        let body = adapter.build_body(
            "claude-sonnet-4-6", &messages, &[], &tool_history, None, &config_venus,
        );

        let msgs = body["messages"].as_array().unwrap();
        // msg[0] = user, msg[1] = assistant (call_1), msg[2] = tool (call_1, cache boundary),
        // msg[3] = assistant (call_2), msg[4] = tool (call_2, no cache)
        assert_eq!(msgs.len(), 5);

        // First tool response should have cache_control (it's the truncated one)
        let first_tool_resp = &msgs[2];
        let content_blocks = first_tool_resp["content"].as_array()
            .expect("Cache boundary tool response should use content-block format");
        assert_eq!(content_blocks[0]["cache_control"]["type"], "ephemeral");

        // Second tool response should be plain string (not a cache boundary)
        let second_tool_resp = &msgs[4];
        assert!(second_tool_resp["content"].is_string());
    }

    #[test]
    fn test_venus_build_body_with_tool_history() {
        let adapter = VenusAdapter;
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
            "gpt-4o", &messages, &[], &tool_history, None, &config_venus,
        );
        let msgs = body["messages"].as_array().unwrap();

        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["role"], "assistant");
        assert!(msgs[1]["tool_calls"].is_array());
        assert_eq!(msgs[2]["role"], "tool");
        assert_eq!(msgs[2]["tool_call_id"], "call_1");
    }
}
