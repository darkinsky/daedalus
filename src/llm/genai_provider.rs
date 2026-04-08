use anyhow::Result;
use async_trait::async_trait;

use genai::adapter::AdapterKind;
use genai::chat::{
    ChatMessage as GenAiChatMessage,
    ChatRequest,
};
use genai::resolver::{AuthData, Endpoint, ServiceTargetResolver};
use genai::{Client, ModelIden, ServiceTarget};

use super::types::*;
use super::LlmApi;

/// LLM provider implementation backed by the `genai` crate.
///
/// Supports OpenAI, Anthropic, Gemini, and any OpenAI-compatible proxy
/// (e.g., Venus) through the genai library's adapter system.
pub struct GenAiProvider {
    client: Client,
    config: LlmConfig,
}

impl GenAiProvider {
    /// Create a new GenAI provider with the given configuration.
    pub fn new(config: LlmConfig) -> Result<Self> {
        let api_key = config.api_key.clone();
        let api_base = config.api_base.clone();

        // Use ServiceTargetResolver to point to the custom endpoint
        // and force OpenAI adapter kind for compatibility.
        let target_resolver = ServiceTargetResolver::from_resolver_fn(
            move |service_target: ServiceTarget| -> Result<ServiceTarget, genai::resolver::Error> {
                let ServiceTarget { model, .. } = service_target;

                let endpoint = if let Some(ref base_url) = api_base {
                    Endpoint::from_owned(format!("{}/", base_url.trim_end_matches('/')))
                } else {
                    Endpoint::from_static("https://api.openai.com/v1/")
                };

                let auth = AuthData::Key(api_key.clone());
                let model = ModelIden::new(AdapterKind::OpenAI, model.model_name);

                Ok(ServiceTarget { endpoint, auth, model })
            },
        );

        let client = Client::builder()
            .with_service_target_resolver(target_resolver)
            .build();

        tracing::info!(
            "GenAI provider initialized with model: {}",
            config.model
        );

        Ok(Self { client, config })
    }

    /// Convert our provider-agnostic ChatMessage to genai's ChatMessage.
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
}

#[async_trait]
impl LlmApi for GenAiProvider {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        options: Option<&ChatOptions>,
    ) -> Result<ChatResponse> {
        // Build the ChatRequest from our messages
        let genai_messages = Self::convert_messages(messages);
        let chat_req = ChatRequest::from_messages(genai_messages);

        // Build genai ChatOptions if provided
        let genai_options = options.map(|opts| {
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
        });

        let chat_res = self
            .client
            .exec_chat(
                &self.config.model,
                chat_req,
                genai_options.as_ref(),
            )
            .await
            .map_err(|e| anyhow::anyhow!("GenAI chat error: {}", e))?;

        let content = chat_res
            .first_text()
            .unwrap_or("No response")
            .to_string();

        // Extract usage if available
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

        Ok(ChatResponse { content, usage })
    }

    fn model_name(&self) -> &str {
        &self.config.model
    }

    fn provider_name(&self) -> &str {
        "GenAI"
    }
}
