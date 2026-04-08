use anyhow::Result;
use async_trait::async_trait;

use genai::adapter::AdapterKind;
use genai::chat::{
    ChatMessage as GenAiChatMessage,
    ChatRequest,
    Tool as GenAiTool,
    ToolCall as GenAiToolCall,
    ToolResponse as GenAiToolResponse,
};
use genai::resolver::{AuthData, Endpoint, ServiceTargetResolver};
use genai::{Client, ModelIden, ServiceTarget};

use genai::chat::ReasoningEffort as GenAiReasoningEffort;

use super::{
    ChatMessage, ChatOptions, ChatResponse, ChatRole, LlmApi, LlmConfig,
    ReasoningEffort, TokenUsage, ToolCall, ToolResponse,
};

/// LLM provider implementation backed by the `genai` crate.
///
/// Supports OpenAI, Anthropic, Gemini, and any OpenAI-compatible proxy
/// (e.g., Venus) through the genai library's adapter system.
///
/// All genai-specific type conversions are encapsulated within this module.
/// The public interface uses only our own provider-agnostic types.
pub struct GenAiProvider {
    client: Client,
    config: LlmConfig,
}

impl GenAiProvider {
    /// Create a new GenAI provider with the given configuration.
    pub fn new(config: LlmConfig) -> Result<Self> {
        let api_key = config.api_key.clone();
        let api_base = config.api_base.clone();
        let adapter_kind = Self::parse_adapter_kind(config.adapter_kind.as_deref());

        // Use ServiceTargetResolver to point to the custom endpoint.
        let target_resolver = ServiceTargetResolver::from_resolver_fn(
            move |service_target: ServiceTarget| -> Result<ServiceTarget, genai::resolver::Error> {
                let ServiceTarget { model, .. } = service_target;

                let endpoint = if let Some(ref base_url) = api_base {
                    Endpoint::from_owned(format!("{}/", base_url.trim_end_matches('/')))
                } else {
                    Endpoint::from_static("https://api.openai.com/v1/")
                };

                let auth = AuthData::Key(api_key.clone());
                let model = ModelIden::new(adapter_kind, model.model_name);

                Ok(ServiceTarget { endpoint, auth, model })
            },
        );

        let client = Client::builder()
            .with_service_target_resolver(target_resolver)
            .build();

        tracing::info!(
            model = %config.model,
            adapter = ?adapter_kind,
            "GenAI provider initialized"
        );

        Ok(Self { client, config })
    }

    /// Parse an adapter kind string into a genai AdapterKind.
    ///
    /// Supports: "openai" (default), "anthropic", "gemini", "groq", "cohere".
    fn parse_adapter_kind(kind: Option<&str>) -> AdapterKind {
        match kind.map(|s| s.to_lowercase()).as_deref() {
            Some("anthropic") => AdapterKind::Anthropic,
            Some("gemini") | Some("google") => AdapterKind::Gemini,
            Some("groq") => AdapterKind::Groq,
            Some("cohere") => AdapterKind::Cohere,
            _ => AdapterKind::OpenAI,
        }
    }

    // ── Type conversion helpers (genai ↔ our types) ──

    /// Convert our ChatMessages to genai ChatMessages.
    fn convert_messages(messages: &[ChatMessage]) -> Vec<GenAiChatMessage> {
        messages
            .iter()
            .map(|msg| match msg.role {
                ChatRole::System => GenAiChatMessage::system(&msg.content),
                ChatRole::User => GenAiChatMessage::user(&msg.content),
                ChatRole::Assistant => GenAiChatMessage::assistant(&msg.content),
                // Tool messages are stored in memory as context; when sent to
                // genai they are treated as assistant messages since genai
                // handles tool responses via its own ToolResponse type.
                ChatRole::Tool => GenAiChatMessage::assistant(&msg.content),
            })
            .collect()
    }

    /// Convert our ToolCall to genai's ToolCall.
    fn to_genai_tool_call(tc: &ToolCall) -> GenAiToolCall {
        GenAiToolCall {
            call_id: tc.call_id.clone(),
            fn_name: tc.fn_name.clone(),
            fn_arguments: tc.fn_arguments.clone(),
            thought_signatures: None,
        }
    }

    /// Convert genai's ToolCall to our ToolCall.
    fn from_genai_tool_call(tc: &GenAiToolCall) -> ToolCall {
        ToolCall {
            call_id: tc.call_id.clone(),
            fn_name: tc.fn_name.clone(),
            fn_arguments: tc.fn_arguments.clone(),
        }
    }

    /// Convert our ToolResponse to genai's ToolResponse.
    fn to_genai_tool_response(tr: &ToolResponse) -> GenAiToolResponse {
        GenAiToolResponse::new(&tr.call_id, &tr.content)
    }

    /// Convert tool definition JSON (OpenAI format) to genai Tool.
    fn json_to_genai_tool(tool_json: &serde_json::Value) -> Option<GenAiTool> {
        let func = tool_json.get("function")?;
        let name = func.get("name")?.as_str()?;
        let mut tool = GenAiTool::new(name);
        if let Some(desc) = func.get("description").and_then(|d| d.as_str()) {
            tool = tool.with_description(desc);
        }
        if let Some(params) = func.get("parameters") {
            tool = tool.with_schema(params.clone());
        }
        Some(tool)
    }

    /// Build genai ChatOptions from our ChatOptions and LlmConfig.
    ///
    /// Always enables `capture_reasoning_content` and `normalize_reasoning_content`
    /// so that reasoning models (Claude extended thinking, DeepSeek-R1, OpenAI o1/o3)
    /// return their thinking process. For non-reasoning models this is a no-op.
    ///
    /// Maps `reasoning_effort` from our config to genai's `ReasoningEffort`.
    /// For Venus proxy's `thinking_enabled`/`thinking_tokens`, these are handled
    /// at the HTTP level since genai's OpenAI adapter doesn't support them natively.
    fn build_options(&self, options: Option<&ChatOptions>) -> genai::chat::ChatOptions {
        let mut genai_opts = genai::chat::ChatOptions::default()
            .with_capture_reasoning_content(true)
            .with_normalize_reasoning_content(true);

        if let Some(opts) = options {
            if let Some(temp) = opts.temperature {
                genai_opts = genai_opts.with_temperature(temp);
            }
            if let Some(max_tokens) = opts.max_tokens {
                genai_opts = genai_opts.with_max_tokens(max_tokens);
            }
            if let Some(top_p) = opts.top_p {
                genai_opts = genai_opts.with_top_p(top_p);
            }
        }

        // Resolve reasoning_effort: request-level overrides config-level
        let effective_effort = options
            .and_then(|o| o.venus.reasoning_effort.as_ref())
            .or(self.config.venus.reasoning_effort.as_ref());
        if let Some(effort) = effective_effort {
            genai_opts = genai_opts.with_reasoning_effort(Self::to_genai_reasoning_effort(effort));
        }

        genai_opts
    }

    /// Convert our ReasoningEffort to genai's ReasoningEffort.
    fn to_genai_reasoning_effort(effort: &ReasoningEffort) -> GenAiReasoningEffort {
        match effort {
            ReasoningEffort::Low => GenAiReasoningEffort::Low,
            ReasoningEffort::Medium => GenAiReasoningEffort::Medium,
            ReasoningEffort::High => GenAiReasoningEffort::High,
        }
    }

    /// Build a ChatResponse from a genai ChatResponse.
    fn build_response(chat_res: &genai::chat::ChatResponse) -> ChatResponse {
        let content = chat_res.first_text().unwrap_or("").to_string();
        let reasoning_content = chat_res.reasoning_content.clone();
        let tool_calls: Vec<ToolCall> = chat_res
            .tool_calls()
            .into_iter()
            .map(Self::from_genai_tool_call)
            .collect();

        let usage = &chat_res.usage;
        let usage = if usage.prompt_tokens.is_some() || usage.completion_tokens.is_some() || usage.total_tokens.is_some() {
            Some(TokenUsage {
                prompt_tokens: usage.prompt_tokens.map(|v| v as u64),
                completion_tokens: usage.completion_tokens.map(|v| v as u64),
                total_tokens: usage.total_tokens.map(|v| v as u64),
            })
        } else {
            None
        };

        ChatResponse { content, reasoning_content, usage, tool_calls }
    }
}

#[async_trait]
impl LlmApi for GenAiProvider {
    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        tool_history: &[(Vec<ToolCall>, Vec<ToolResponse>)],
        options: Option<&ChatOptions>,
    ) -> Result<ChatResponse> {
        // Build the initial request from conversation messages
        let genai_messages = Self::convert_messages(messages);
        let mut chat_req = ChatRequest::from_messages(genai_messages);

        // Only attach tool definitions if tools are provided.
        // Sending an empty tools array can cause errors with some API backends.
        if !tools.is_empty() {
            let genai_tools: Vec<GenAiTool> = tools
                .iter()
                .filter_map(Self::json_to_genai_tool)
                .collect();
            chat_req = chat_req.with_tools(genai_tools);
        }

        // Replay tool history: for each prior round, append the assistant's
        // tool calls and the corresponding tool responses.
        for (calls, responses) in tool_history {
            let genai_calls: Vec<GenAiToolCall> = calls.iter().map(Self::to_genai_tool_call).collect();
            chat_req = chat_req.append_message(GenAiChatMessage::from(genai_calls));

            for resp in responses {
                let genai_resp = Self::to_genai_tool_response(resp);
                chat_req = chat_req.append_message(GenAiChatMessage::from(genai_resp));
            }
        }

        let genai_options = self.build_options(options);

        let chat_res = self
            .client
            .exec_chat(&self.config.model, chat_req, Some(&genai_options))
            .await
            .map_err(|e| anyhow::anyhow!("GenAI chat error: {}", e))?;

        Ok(Self::build_response(&chat_res))
    }

    fn supports_tools(&self) -> bool {
        true
    }

    fn model_name(&self) -> &str {
        &self.config.model
    }

    fn provider_name(&self) -> &str {
        "GenAI"
    }
}
