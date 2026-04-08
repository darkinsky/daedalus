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

use super::types::*;
use super::LlmApi;

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

    /// Build genai ChatOptions from our ChatOptions.
    fn build_options(options: Option<&ChatOptions>) -> Option<genai::chat::ChatOptions> {
        options.map(|opts| {
            let mut genai_opts = genai::chat::ChatOptions::default();
            if let Some(temp) = opts.temperature {
                genai_opts = genai_opts.with_temperature(temp);
            }
            if let Some(max_tokens) = opts.max_tokens {
                genai_opts = genai_opts.with_max_tokens(max_tokens);
            }
            if let Some(top_p) = opts.top_p {
                genai_opts = genai_opts.with_top_p(top_p);
            }
            genai_opts
        })
    }

    /// Build a ChatResponse from a genai ChatResponse.
    fn build_response(chat_res: &genai::chat::ChatResponse) -> ChatResponse {
        let content = chat_res.first_text().unwrap_or("").to_string();
        let tool_calls: Vec<ToolCall> = chat_res
            .tool_calls()
            .into_iter()
            .map(Self::from_genai_tool_call)
            .collect();

        let u = &chat_res.usage;
        let usage = if u.prompt_tokens.is_some() || u.completion_tokens.is_some() || u.total_tokens.is_some() {
            Some(TokenUsage {
                prompt_tokens: u.prompt_tokens.map(|v| v as u64),
                completion_tokens: u.completion_tokens.map(|v| v as u64),
                total_tokens: u.total_tokens.map(|v| v as u64),
            })
        } else {
            None
        };

        ChatResponse { content, usage, tool_calls }
    }
}

#[async_trait]
impl LlmApi for GenAiProvider {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        options: Option<&ChatOptions>,
    ) -> Result<ChatResponse> {
        let genai_messages = Self::convert_messages(messages);
        let chat_req = ChatRequest::from_messages(genai_messages);
        let genai_options = Self::build_options(options);

        let chat_res = self
            .client
            .exec_chat(&self.config.model, chat_req, genai_options.as_ref())
            .await
            .map_err(|e| anyhow::anyhow!("GenAI chat error: {}", e))?;

        Ok(Self::build_response(&chat_res))
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        tool_history: &[(Vec<ToolCall>, Vec<ToolResponse>)],
        options: Option<&ChatOptions>,
    ) -> Result<ChatResponse> {
        // Convert tool definitions from JSON to genai Tool
        let genai_tools: Vec<GenAiTool> = tools
            .iter()
            .filter_map(Self::json_to_genai_tool)
            .collect();

        // Build the initial request from conversation messages + tools
        let genai_messages = Self::convert_messages(messages);
        let mut chat_req = ChatRequest::from_messages(genai_messages)
            .with_tools(genai_tools);

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

        let genai_options = Self::build_options(options);

        let chat_res = self
            .client
            .exec_chat(&self.config.model, chat_req, genai_options.as_ref())
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
