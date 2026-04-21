use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};

use super::{
    ChatMessage, ChatOptions, ChatResponse, LlmApi, LlmConfig,
    TokenUsage, ToolCall, ToolRound, VenusExtensions,
};

/// LLM provider that directly calls the Venus API proxy via HTTP.
///
/// Unlike `GenAiProvider` which uses the `genai` crate's adapter system,
/// this provider constructs raw HTTP requests, giving full control over
/// the request body. This enables support for Venus-specific extensions:
///
/// - `thinking_enabled` / `thinking_tokens` (Claude, Gemini, VenusLLMServing)
/// - `reasoning_effort` (OpenAI o-series, Gemini 3 series)
///
/// The response format follows OpenAI's chat completion API, which Venus
/// normalizes across all backend providers.
pub struct VenusProvider {
    client: Client,
    config: LlmConfig,
    base_url: String,
}

impl VenusProvider {
    /// Create a new Venus provider with the given configuration.
    pub fn new(config: LlmConfig) -> Result<Self> {
        let base_url = config
            .api_base
            .as_deref()
            .unwrap_or("https://api.openai.com/v1")
            .trim_end_matches('/')
            .to_string();

        let client = Client::new();

        tracing::info!(
            model = %config.model,
            base_url = %base_url,
            thinking_enabled = ?config.venus.thinking_enabled,
            thinking_tokens = ?config.venus.thinking_tokens,
            reasoning_effort = ?config.venus.reasoning_effort,
            "Venus provider initialized"
        );

        Ok(Self { client, config, base_url })
    }

    /// Build the request body for a chat completion call.
    fn build_request_body(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        tool_history: &[ToolRound],
        options: Option<&ChatOptions>,
    ) -> Value {
        // Build messages array, with cache_control support.
        //
        // When a message has `cache_control: Some(Ephemeral)`, we emit
        // the Anthropic-style content block format:
        //   { "role": "...", "content": [{ "type": "text", "text": "...",
        //     "cache_control": { "type": "ephemeral" } }] }
        //
        // Venus proxy forwards this to Anthropic backends as-is, and
        // for OpenAI backends the proxy strips the cache_control field
        // (OpenAI uses automatic prefix caching, no explicit markers needed).
        let mut msg_array: Vec<Value> = messages
            .iter()
            .map(|msg| {
                if msg.cache_control.is_some() {
                    // Use content-block format with cache_control marker
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

        // Replay tool history
        for round in tool_history {
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

            msg_array.push(json!({
                "role": "assistant",
                "content": null,
                "tool_calls": tool_calls_json,
            }));

            // Tool response messages
            for resp in &round.responses {
                msg_array.push(json!({
                    "role": "tool",
                    "tool_call_id": resp.call_id,
                    "content": resp.content,
                }));
            }
        }

        let mut body = json!({
            "model": self.config.model,
            "messages": msg_array,
        });

        // Add tool definitions
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }

        // Apply standard chat options (temperature, max_tokens, top_p)
        if let Some(opts) = options {
            Self::apply_standard_options(&mut body, opts);
        }

        // Merge Venus extensions: request-level overrides config-level defaults,
        // then apply the merged result to the request body.
        let request_venus = options.map(|o| &o.venus);
        self.apply_venus_extensions(&mut body, request_venus);

        body
    }

    /// Apply standard ChatOptions fields (temperature, max_tokens, top_p)
    /// to the request body.
    fn apply_standard_options(body: &mut Value, opts: &ChatOptions) {
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

    /// Apply Venus extension parameters to the request body.
    ///
    /// Merges config-level defaults with optional request-level overrides.
    /// Request-level values take priority over config-level defaults.
    fn apply_venus_extensions(
        &self,
        body: &mut Value,
        request_venus: Option<&VenusExtensions>,
    ) {
        let merged = match request_venus {
            Some(overrides) => self.config.venus.merge_with_overrides(overrides),
            None => self.config.venus.clone(),
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
    }

    /// Parse the Venus API response into our ChatResponse.
    fn parse_response(response_body: &Value) -> Result<ChatResponse> {
        let message = Self::extract_first_message(response_body)?;
        let content = Self::parse_content(&message);
        let reasoning_content = Self::parse_reasoning(&message, &content);
        let tool_calls = Self::parse_tool_calls(&message);
        let usage = Self::parse_usage(response_body);

        Ok(ChatResponse { content, reasoning_content, usage, tool_calls })
    }

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
            .or_else(|| Self::extract_think_content(content))
    }

    /// Parse tool calls from a message object.
    fn parse_tool_calls(message: &Value) -> Vec<ToolCall> {
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
    /// Venus proxy normalizes both formats, so we check both.
    fn parse_usage(response_body: &Value) -> Option<TokenUsage> {
        let usage_obj = response_body.get("usage")?;
        let prompt = usage_obj.get("prompt_tokens").and_then(|v| v.as_u64());
        let completion = usage_obj.get("completion_tokens").and_then(|v| v.as_u64());
        let total = usage_obj.get("total_tokens").and_then(|v| v.as_u64());

        // Extract cached tokens from OpenAI-style nested field or Anthropic-style flat field
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
    ///
    /// Returns `None` if no `<think>` tags are found.
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

    /// Send an HTTP request to the Venus API and return the parsed JSON body.
    ///
    /// Handles the full HTTP lifecycle: send request → read response → parse JSON
    /// → check HTTP status. This separates network concerns from business logic
    /// in `chat_with_tools`.
    async fn send_chat_request(&self, body: &Value) -> Result<Value> {
        let url = format!("{}/chat/completions", self.base_url);

        tracing::debug!(
            url = %url,
            model = %self.config.model,
            thinking_enabled = ?body.get("thinking_enabled"),
            thinking_tokens = ?body.get("thinking_tokens"),
            reasoning_effort = ?body.get("reasoning_effort"),
            message_count = ?body.get("messages").and_then(|m| m.as_array()).map(|a| a.len()),
            tool_count = ?body.get("tools").and_then(|t| t.as_array()).map(|a| a.len()),
            "Venus API request"
        );

        // Log full request body at trace level for deep debugging
        tracing::trace!(
            request_body = %body,
            "Venus API request body (full)"
        );

        let start = std::time::Instant::now();
        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .json(body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Venus HTTP request error: {}", e))?;
        let http_elapsed_ms = start.elapsed().as_millis() as u64;

        let status = response.status();
        let response_text = response
            .text()
            .await
            .map_err(|e| anyhow::anyhow!("Venus response read error: {}", e))?;

        tracing::debug!(
            status = %status,
            response_len = response_text.len(),
            http_elapsed_ms = http_elapsed_ms,
            "Venus API response received"
        );

        // Log full response body at trace level for deep debugging
        tracing::trace!(
            response_body = %response_text,
            "Venus API response body (full)"
        );

        let response_body: Value = serde_json::from_str(&response_text)
            .map_err(|e| {
                let preview = if response_text.len() <= 500 {
                    &response_text
                } else {
                    &response_text[..500]
                };
                anyhow::anyhow!("Venus response parse error: {} (body: {})", e, preview)
            })?;

        if !status.is_success() {
            let error_msg = response_body
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error");
            return Err(anyhow::anyhow!(
                "Venus API error (HTTP {}): {}",
                status.as_u16(),
                error_msg
            ));
        }

        Ok(response_body)
    }
}

#[async_trait]
impl LlmApi for VenusProvider {
    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        tool_history: &[ToolRound],
        options: Option<&ChatOptions>,
    ) -> Result<ChatResponse> {
        let body = self.build_request_body(messages, tools, tool_history, options);
        let response_body = self.send_chat_request(&body).await?;
        Self::parse_response(&response_body)
    }

    fn supports_tools(&self) -> bool {
        true
    }

    fn model_name(&self) -> &str {
        &self.config.model
    }

    fn provider_name(&self) -> &str {
        "Venus"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ReasoningEffort, ToolResponse};

    #[test]
    fn test_extract_think_content() {
        // Normal case
        let content = "<think>I need to think about this</think>Here is my answer";
        assert_eq!(
            VenusProvider::extract_think_content(content),
            Some("I need to think about this".to_string())
        );

        // Empty think tags
        let content = "<think></think>Answer";
        assert_eq!(VenusProvider::extract_think_content(content), None);

        // Whitespace-only think tags
        let content = "<think>  \n  </think>Answer";
        assert_eq!(VenusProvider::extract_think_content(content), None);

        // No think tags
        let content = "Just a normal response";
        assert_eq!(VenusProvider::extract_think_content(content), None);
    }

    #[test]
    fn test_parse_response_basic() {
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

        let resp = VenusProvider::parse_response(&body).unwrap();
        assert_eq!(resp.content, "Hello!");
        assert!(resp.reasoning_content.is_none());
        assert!(resp.tool_calls.is_empty());
        assert_eq!(resp.usage.as_ref().unwrap().total_tokens, Some(15));
    }

    #[test]
    fn test_parse_response_with_reasoning() {
        let body = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "The answer is 42.",
                    "reasoning_content": "Let me think step by step..."
                }
            }]
        });

        let resp = VenusProvider::parse_response(&body).unwrap();
        assert_eq!(resp.content, "The answer is 42.");
        assert_eq!(resp.reasoning_content.unwrap(), "Let me think step by step...");
    }

    #[test]
    fn test_parse_response_with_tool_calls() {
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

        let resp = VenusProvider::parse_response(&body).unwrap();
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].call_id, "call_123");
        assert_eq!(resp.tool_calls[0].function_name, "get_weather");
        assert_eq!(resp.tool_calls[0].arguments["city"], "Beijing");
    }

    #[test]
    fn test_build_request_body_with_thinking() {
        let config = LlmConfig {
            api_key: "test-key".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            api_base: Some("https://venus.example.com/v1".to_string()),
            adapter_kind: None,
            venus: VenusExtensions {
                thinking_enabled: Some(true),
                thinking_tokens: Some(4096),
                reasoning_effort: None,
            },
        };

        let provider = VenusProvider::new(config).unwrap();
        let messages = vec![ChatMessage::user("Hello")];
        let body = provider.build_request_body(&messages, &[], &[], None);

        assert_eq!(body["model"], "claude-sonnet-4-20250514");
        assert_eq!(body["thinking_enabled"], true);
        assert_eq!(body["thinking_tokens"], 4096);
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn test_build_request_body_with_reasoning_effort() {
        let config = LlmConfig {
            api_key: "test-key".to_string(),
            model: "o3-mini".to_string(),
            api_base: None,
            adapter_kind: None,
            venus: VenusExtensions {
                thinking_enabled: None,
                thinking_tokens: None,
                reasoning_effort: Some(ReasoningEffort::High),
            },
        };

        let provider = VenusProvider::new(config).unwrap();
        let messages = vec![ChatMessage::user("Hello")];
        let body = provider.build_request_body(&messages, &[], &[], None);

        assert_eq!(body["model"], "o3-mini");
        assert_eq!(body["reasoning_effort"], "high");
        assert!(body.get("thinking_enabled").is_none());
    }

    #[test]
    fn test_request_level_options_override_config() {
        let config = LlmConfig {
            api_key: "test-key".to_string(),
            model: "test-model".to_string(),
            api_base: None,
            adapter_kind: None,
            venus: VenusExtensions {
                thinking_enabled: Some(true),
                thinking_tokens: Some(2048),
                reasoning_effort: Some(ReasoningEffort::Low),
            },
        };

        let provider = VenusProvider::new(config).unwrap();
        let messages = vec![ChatMessage::user("Hello")];

        // Request-level options should override config-level
        let opts = ChatOptions {
            venus: VenusExtensions {
                thinking_enabled: Some(false),
                thinking_tokens: Some(8192),
                reasoning_effort: Some(ReasoningEffort::High),
            },
            ..Default::default()
        };

        let body = provider.build_request_body(&messages, &[], &[], Some(&opts));
        assert_eq!(body["thinking_enabled"], false);
        assert_eq!(body["thinking_tokens"], 8192);
        assert_eq!(body["reasoning_effort"], "high");
    }

    #[test]
    fn test_build_request_body_with_tool_history() {
        let config = LlmConfig {
            api_key: "test-key".to_string(),
            model: "test-model".to_string(),
            api_base: None,
            adapter_kind: None,
            venus: VenusExtensions::default(),
        };

        let provider = VenusProvider::new(config).unwrap();
        let messages = vec![ChatMessage::user("What's the weather?")];

        let tool_calls = vec![ToolCall {
            call_id: "call_1".to_string(),
            function_name: "get_weather".to_string(),
            arguments: json!({"city": "Beijing"}),
        }];
        let tool_responses = vec![ToolResponse::new("call_1", "Sunny, 25°C")];
        let tool_history = vec![ToolRound {
            calls: tool_calls,
            responses: tool_responses,
        }];

        let body = provider.build_request_body(&messages, &[], &tool_history, None);
        let msgs = body["messages"].as_array().unwrap();

        // Should have: user message + assistant tool_calls + tool response = 3 messages
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["role"], "assistant");
        assert!(msgs[1]["tool_calls"].is_array());
        assert_eq!(msgs[2]["role"], "tool");
        assert_eq!(msgs[2]["tool_call_id"], "call_1");
    }

    #[test]
    fn test_build_request_body_with_cache_control() {
        use crate::llm::CacheControl;

        let config = LlmConfig {
            api_key: "test-key".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            api_base: None,
            adapter_kind: None,
            venus: VenusExtensions::default(),
        };

        let provider = VenusProvider::new(config).unwrap();
        let messages = vec![
            ChatMessage::system("You are helpful.")
                .with_cache_control(CacheControl::Ephemeral),
            ChatMessage::user("Hello"),
        ];

        let body = provider.build_request_body(&messages, &[], &[], None);
        let msgs = body["messages"].as_array().unwrap();

        // System message should use content-block format with cache_control
        assert_eq!(msgs[0]["role"], "system");
        let content_blocks = msgs[0]["content"].as_array()
            .expect("System message with cache_control should use content-block format");
        assert_eq!(content_blocks.len(), 1);
        assert_eq!(content_blocks[0]["type"], "text");
        assert_eq!(content_blocks[0]["text"], "You are helpful.");
        assert_eq!(content_blocks[0]["cache_control"]["type"], "ephemeral");

        // User message should use plain string format (no cache_control)
        assert_eq!(msgs[1]["role"], "user");
        assert!(msgs[1]["content"].is_string());
        assert_eq!(msgs[1]["content"], "Hello");
    }

    #[test]
    fn test_parse_usage_with_cached_tokens_openai_format() {
        let body = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Hello!"
                }
            }],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 20,
                "total_tokens": 120,
                "prompt_tokens_details": {
                    "cached_tokens": 80
                }
            }
        });

        let resp = VenusProvider::parse_response(&body).unwrap();
        let usage = resp.usage.unwrap();
        assert_eq!(usage.prompt_tokens, Some(100));
        assert_eq!(usage.cached_tokens, Some(80));
    }

    #[test]
    fn test_parse_usage_with_cached_tokens_anthropic_format() {
        let body = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Hello!"
                }
            }],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 20,
                "total_tokens": 120,
                "cache_read_input_tokens": 75
            }
        });

        let resp = VenusProvider::parse_response(&body).unwrap();
        let usage = resp.usage.unwrap();
        assert_eq!(usage.prompt_tokens, Some(100));
        assert_eq!(usage.cached_tokens, Some(75));
    }

    #[test]
    fn test_parse_usage_without_cached_tokens() {
        let body = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Hello!"
                }
            }],
            "usage": {
                "prompt_tokens": 50,
                "completion_tokens": 10,
                "total_tokens": 60
            }
        });

        let resp = VenusProvider::parse_response(&body).unwrap();
        let usage = resp.usage.unwrap();
        assert_eq!(usage.prompt_tokens, Some(50));
        assert!(usage.cached_tokens.is_none());
    }
}
