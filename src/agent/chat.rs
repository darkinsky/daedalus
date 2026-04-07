use anyhow::Result;
use async_trait::async_trait;

use crate::config::AgentConfig;
use crate::llm::{LlmApi, format_messages_for_log};
use crate::memory::SlidingWindowMemory;
use crate::session::Session;

use super::Mode;

/// Chat mode — simple multi-turn conversation without tool use.
///
/// This is the basic mode where the agent acts as a conversational assistant,
/// using the session's `Memory` to manage conversation history. The memory
/// strategy determines how past messages are included in each LLM request.
pub struct ChatAgent {
    /// The LLM provider (trait object, provider-agnostic).
    llm: Box<dyn LlmApi>,
    /// The current conversation session (owns its own memory).
    session: Session,
    /// System prompt (kept for creating new sessions).
    system_prompt: String,
}

impl ChatAgent {
    /// Create a new chat agent with the given LLM provider and configuration.
    pub fn new(llm: Box<dyn LlmApi>, config: &AgentConfig) -> Self {
        let memory = Box::new(SlidingWindowMemory::unlimited(&config.system_prompt));
        let session = Session::new(memory);

        tracing::info!(
            mode = "chat",
            memory_strategy = session.memory().strategy_name(),
            provider = llm.provider_name(),
            model = llm.model_name(),
            "ChatAgent initialized"
        );

        Self {
            llm,
            session,
            system_prompt: config.system_prompt.clone(),
        }
    }
}

#[async_trait]
impl Mode for ChatAgent {
    async fn chat(&mut self, user_input: &str) -> Result<String> {
        let request_id = self.session.next_request_id();

        // Store user message in session memory
        self.session.memory_mut().add_user_message(user_input);

        // Build the full message list from session memory
        let messages = self.session.build_messages();

        // Log the user input and full LLM input (messages from memory)
        let llm_input = format_messages_for_log(&messages);
        tracing::info!(
            session_id = %self.session.id,
            request_id = request_id,
            provider = self.llm.provider_name(),
            model = self.llm.model_name(),
            role = "user",
            message = user_input,
            memory_strategy = self.session.memory().strategy_name(),
            turn_count = self.session.memory().turn_count(),
            message_count = messages.len(),
            llm_input = llm_input.as_str(),
            "LLM request: user input"
        );

        // Call the LLM with messages built from session memory
        let response = self.llm.chat(&messages, None).await?;

        // Log the assistant output
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

        // Store assistant response in session memory
        self.session.memory_mut().add_assistant_message(&response.content);

        Ok(response.content)
    }

    fn new_session(&mut self) {
        let memory = Box::new(SlidingWindowMemory::unlimited(&self.system_prompt));
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
