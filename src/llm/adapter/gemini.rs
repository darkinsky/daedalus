//! Google Gemini API adapter.
//!
//! Handles direct connections to the Google Gemini API.
//! Key differences from OpenAI format:
//! - Uses `contents` array with `parts` instead of `messages`
//! - Tool calls use `functionCall` / `functionResponse` format
//! - Authentication via API key in query parameter or Bearer token
//! - Different endpoint URL structure

use anyhow::Result;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde_json::{json, Value};

use super::ApiAdapter;
use crate::llm::{
    ChatMessage, ChatOptions, ChatResponse, ChatRole, StreamChunk,
    TokenUsage, ToolCall, ToolRound, VenusExtensions,
};

/// Adapter for the Google Gemini API (direct connection).
pub struct GeminiAdapter;

impl ApiAdapter for GeminiAdapter {
    fn endpoint(&self, base_url: &str, model: &str) -> String {
        format!("{}/models/{}:generateContent", base_url, model)
    }

    fn headers(&self, api_key: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        // Gemini uses Bearer token or x-goog-api-key
        headers.insert(
            "x-goog-api-key",
            HeaderValue::from_str(api_key).unwrap(),
        );
        headers
    }

    fn build_body(
        &self,
        _model: &str,
        messages: &[ChatMessage],
        tools: &[Value],
        tool_history: &[ToolRound],
        options: Option<&ChatOptions>,
        config_venus: &VenusExtensions,
    ) -> Value {
        // Separate system instruction
        let system_instruction: Option<String> = messages
            .iter()
            .find(|m| m.role == ChatRole::System)
            .map(|m| m.content.clone());

        // Build contents array
        let mut contents: Vec<Value> = messages
            .iter()
            .filter(|m| m.role != ChatRole::System)
            .map(|msg| {
                let role = match msg.role {
                    ChatRole::User => "user",
                    ChatRole::Assistant => "model",
                    _ => "user",
                };
                json!({
                    "role": role,
                    "parts": [{"text": msg.content}]
                })
            })
            .collect();

        // Replay tool history in Gemini format
        for round in tool_history.iter() {
            // Model message with functionCall parts
            let function_call_parts: Vec<Value> = round.calls
                .iter()
                .map(|tc| {
                    json!({
                        "functionCall": {
                            "name": tc.function_name,
                            "args": tc.arguments,
                        }
                    })
                })
                .collect();

            contents.push(json!({
                "role": "model",
                "parts": function_call_parts,
            }));

            // User message with functionResponse parts
            let function_response_parts: Vec<Value> = round.responses
                .iter()
                .zip(round.calls.iter())
                .map(|(resp, call)| {
                    json!({
                        "functionResponse": {
                            "name": call.function_name,
                            "response": {
                                "content": resp.content,
                            }
                        }
                    })
                })
                .collect();

            contents.push(json!({
                "role": "user",
                "parts": function_response_parts,
            }));
        }

        let mut body = json!({
            "contents": contents,
        });

        // Add system instruction
        if let Some(system) = system_instruction {
            body["systemInstruction"] = json!({
                "parts": [{"text": system}]
            });
        }

        // Add tool definitions (Gemini format)
        if !tools.is_empty() {
            let function_declarations: Vec<Value> = tools
                .iter()
                .filter_map(|t| {
                    let func = t.get("function")?;
                    Some(json!({
                        "name": func.get("name")?,
                        "description": func.get("description").unwrap_or(&json!("")),
                        "parameters": func.get("parameters").unwrap_or(&json!({"type": "object"})),
                    }))
                })
                .collect();
            body["tools"] = json!([{
                "functionDeclarations": function_declarations,
            }]);
        }

        // Generation config
        let mut generation_config = json!({});
        if let Some(opts) = options {
            if let Some(temp) = opts.temperature {
                generation_config["temperature"] = json!(temp);
            }
            if let Some(max_tokens) = opts.max_tokens {
                generation_config["maxOutputTokens"] = json!(max_tokens);
            }
            if let Some(top_p) = opts.top_p {
                generation_config["topP"] = json!(top_p);
            }
        }

        // Thinking configuration
        let request_venus = options.map(|o| &o.venus);
        let merged = match request_venus {
            Some(overrides) => config_venus.merge_with_overrides(overrides),
            None => config_venus.clone(),
        };

        if let Some(true) = merged.thinking_enabled {
            let budget = merged.thinking_tokens.unwrap_or(4096);
            generation_config["thinkingConfig"] = json!({
                "thinkingBudget": budget,
            });
        }

        if generation_config.as_object().map_or(false, |o| !o.is_empty()) {
            body["generationConfig"] = generation_config;
        }

        body
    }

    fn parse_response(&self, response_body: &Value) -> Result<ChatResponse> {
        let candidates = response_body
            .get("candidates")
            .and_then(|c| c.as_array())
            .ok_or_else(|| anyhow::anyhow!("Missing 'candidates' in Gemini response"))?;

        let first = candidates
            .first()
            .ok_or_else(|| anyhow::anyhow!("Empty 'candidates' array"))?;

        let parts = first
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
            .ok_or_else(|| anyhow::anyhow!("Missing 'content.parts' in candidate"))?;

        let mut content = String::new();
        let mut reasoning_content: Option<String> = None;
        let mut tool_calls = Vec::new();

        for part in parts {
            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                content.push_str(text);
            }
            if let Some(thought) = part.get("thought").and_then(|t| t.as_str()) {
                reasoning_content = Some(thought.to_string());
            }
            if let Some(fc) = part.get("functionCall") {
                if let (Some(name), Some(args)) = (
                    fc.get("name").and_then(|n| n.as_str()),
                    fc.get("args"),
                ) {
                    tool_calls.push(ToolCall {
                        call_id: format!("gemini_{}", uuid_v4_short()),
                        function_name: name.to_string(),
                        arguments: args.clone(),
                    });
                }
            }
        }

        // Parse usage
        let usage = response_body.get("usageMetadata").map(|u| {
            let prompt = u.get("promptTokenCount").and_then(|v| v.as_u64());
            let completion = u.get("candidatesTokenCount").and_then(|v| v.as_u64());
            let cached = u.get("cachedContentTokenCount").and_then(|v| v.as_u64());
            TokenUsage {
                prompt_tokens: prompt,
                completion_tokens: completion,
                total_tokens: match (prompt, completion) {
                    (Some(p), Some(c)) => Some(p + c),
                    _ => None,
                },
                cached_tokens: cached,
            }
        });

        Ok(ChatResponse { content, reasoning_content, usage, tool_calls })
    }

    fn parse_stream_event(&self, data: &str) -> Option<StreamChunk> {
        // Gemini streaming returns JSON objects directly (not SSE)
        let parsed: Value = serde_json::from_str(data).ok()?;

        let candidates = parsed.get("candidates")?.as_array()?;
        let first = candidates.first()?;
        let parts = first.get("content")?.get("parts")?.as_array()?;

        for part in parts {
            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                if !text.is_empty() {
                    return Some(StreamChunk::ContentDelta(text.to_string()));
                }
            }
            if let Some(thought) = part.get("thought").and_then(|t| t.as_str()) {
                if !thought.is_empty() {
                    return Some(StreamChunk::ReasoningDelta(thought.to_string()));
                }
            }
        }

        // Check for usage in the last chunk
        if let Some(usage_meta) = parsed.get("usageMetadata") {
            let prompt = usage_meta.get("promptTokenCount").and_then(|v| v.as_u64());
            let completion = usage_meta.get("candidatesTokenCount").and_then(|v| v.as_u64());
            let cached = usage_meta.get("cachedContentTokenCount").and_then(|v| v.as_u64());
            if prompt.is_some() || completion.is_some() {
                return Some(StreamChunk::Usage(TokenUsage {
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

        None
    }

    fn name(&self) -> &str {
        "Gemini"
    }
}

/// Generate a short pseudo-random ID for Gemini tool calls.
///
/// Gemini doesn't provide call IDs like OpenAI, so we generate them.
fn uuid_v4_short() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    format!("{:08x}", nanos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gemini_endpoint() {
        let adapter = GeminiAdapter;
        assert_eq!(
            adapter.endpoint("https://generativelanguage.googleapis.com/v1beta", "gemini-2.5-pro"),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:generateContent"
        );
    }

    #[test]
    fn test_gemini_parse_response() {
        let adapter = GeminiAdapter;
        let body = json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "Hello from Gemini!"}],
                    "role": "model"
                }
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 5
            }
        });

        let resp = adapter.parse_response(&body).unwrap();
        assert_eq!(resp.content, "Hello from Gemini!");
        assert!(resp.tool_calls.is_empty());
        assert_eq!(resp.usage.as_ref().unwrap().prompt_tokens, Some(10));
    }

    #[test]
    fn test_gemini_parse_function_call() {
        let adapter = GeminiAdapter;
        let body = json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "functionCall": {
                            "name": "get_weather",
                            "args": {"city": "Tokyo"}
                        }
                    }],
                    "role": "model"
                }
            }]
        });

        let resp = adapter.parse_response(&body).unwrap();
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].function_name, "get_weather");
        assert_eq!(resp.tool_calls[0].arguments["city"], "Tokyo");
    }

    #[test]
    fn test_gemini_build_body_system_instruction() {
        let adapter = GeminiAdapter;
        let messages = vec![
            ChatMessage::system("You are helpful."),
            ChatMessage::user("Hello"),
        ];
        let config_venus = VenusExtensions::default();

        let body = adapter.build_body(
            "gemini-2.5-pro", &messages, &[], &[], None, &config_venus,
        );

        // System should be in systemInstruction
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "You are helpful.");
        // Contents should only have user message
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
    }
}
