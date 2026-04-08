use anyhow::Result;
use async_trait::async_trait;

use crate::config::AgentConfig;
use crate::llm::{ChatMessage, ChatResponse, LlmApi, format_messages_for_log};
use crate::memory::Memory;
use crate::session::Session;

use super::AgentMode;

/// A factory function type for creating memory instances.
///
/// This allows `ChatAgent` to create new memory instances (e.g., when starting
/// a new session) without being coupled to a specific memory implementation.
pub type MemoryFactory = Box<dyn Fn(&str) -> Box<dyn Memory> + Send + Sync>;

/// Chat mode — simple multi-turn conversation without tool use.
///
/// This is the basic mode where the agent acts as a conversational assistant,
/// using the session's `Memory` to manage conversation history. The memory
/// strategy is determined by the injected `MemoryFactory`, keeping this struct
/// decoupled from any specific memory implementation.
pub struct ChatAgent {
    /// The LLM provider (trait object, provider-agnostic).
    llm: Box<dyn LlmApi>,
    /// The current conversation session (owns its own memory).
    session: Session,
    /// System prompt (kept for creating new sessions).
    system_prompt: String,
    /// Factory for creating memory instances (decoupled from concrete type).
    memory_factory: MemoryFactory,
}

impl ChatAgent {
    /// Create a new chat agent with the given LLM provider, configuration,
    /// and memory factory.
    ///
    /// The `memory_factory` takes a system prompt and returns a boxed `Memory`.
    /// This decouples `ChatAgent` from any specific memory implementation.
    pub fn with_memory_factory(
        llm: Box<dyn LlmApi>,
        config: &AgentConfig,
        memory_factory: MemoryFactory,
    ) -> Self {
        let memory = memory_factory(&config.system_prompt);
        let session = Session::new(memory);

        tracing::info!(
            mode = "chat",
            memory_strategy = session.strategy_name(),
            provider = llm.provider_name(),
            model = llm.model_name(),
            "ChatAgent initialized"
        );

        Self {
            llm,
            session,
            system_prompt: config.system_prompt.clone(),
            memory_factory,
        }
    }

    /// Create a new chat agent with the default memory strategy (unlimited sliding window).
    pub fn new(llm: Box<dyn LlmApi>, config: &AgentConfig) -> Self {
        use crate::memory::SlidingWindowMemory;
        let factory: MemoryFactory = Box::new(|prompt: &str| {
            Box::new(SlidingWindowMemory::unlimited(prompt))
        });
        Self::with_memory_factory(llm, config, factory)
    }

    /// Log the outgoing LLM request details.
    fn log_request(&self, request_id: u64, user_input: &str, messages: &[ChatMessage]) {
        let llm_input = format_messages_for_log(messages);
        tracing::info!(
            session_id = %self.session.id,
            request_id = request_id,
            provider = self.llm.provider_name(),
            model = self.llm.model_name(),
            role = "user",
            message = user_input,
            memory_strategy = self.session.strategy_name(),
            turn_count = self.session.turn_count(),
            message_count = messages.len(),
            llm_input = llm_input.as_str(),
            "LLM request: user input"
        );
    }

    /// Log the incoming LLM response details.
    fn log_response(&self, request_id: u64, response: &ChatResponse) {
        tracing::info!(
            session_id = %self.session.id,
            request_id = request_id,
            provider = self.llm.provider_name(),
            model = self.llm.model_name(),
            role = "assistant",
            message = response.content.as_str(),
            content_len = response.content.len(),
            prompt_tokens = response.usage.as_ref().and_then(|u| u.prompt_tokens),
            completion_tokens = response.usage.as_ref().and_then(|u| u.completion_tokens),
            total_tokens = response.usage.as_ref().and_then(|u| u.total_tokens),
            "LLM response: assistant output"
        );
    }
}

#[async_trait]
impl AgentMode for ChatAgent {
    async fn chat(&mut self, user_input: &str) -> Result<String> {
        let request_id = self.session.next_request_id();

        // Store user message in session memory
        self.session.add_user_message(user_input);

        // Build the full message list from session memory
        let messages = self.session.build_messages();

        // Log the request
        self.log_request(request_id, user_input, &messages);

        // Call the LLM with messages built from session memory
        let response = self.llm.chat(&messages, None).await?;

        // Log the response
        self.log_response(request_id, &response);

        // Store assistant response in session memory
        self.session.add_assistant_message(&response.content);

        Ok(response.content)
    }

    fn new_session(&mut self) {
        let memory = (self.memory_factory)(&self.system_prompt);
        self.session = Session::new(memory);
    }

    fn session(&self) -> &Session {
        &self.session
    }

    fn provider_name(&self) -> &str {
        self.llm.provider_name()
    }

    fn model_name(&self) -> &str {
        self.llm.model_name()
    }

    fn mode_name(&self) -> &str {
        "chat"
    }
}
