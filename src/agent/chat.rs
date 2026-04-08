use anyhow::Result;
use async_trait::async_trait;

use crate::config::AgentConfig;
use crate::llm::{
    ChatMessage, ChatResponse, LlmApi, ToolCall, ToolInfo, ToolResponse,
    TokenUsage, format_messages_for_log,
};
use crate::mcp::McpManager;
use crate::memory::Memory;
use crate::session::Session;

use super::AgentMode;

/// A factory function type for creating memory instances.
///
/// This allows `ChatAgent` to create new memory instances (e.g., when starting
/// a new session) without being coupled to a specific memory implementation.
pub type MemoryFactory = Box<dyn Fn(&str) -> Box<dyn Memory> + Send + Sync>;

/// Maximum number of tool-calling rounds per user message.
///
/// This prevents infinite loops if the LLM keeps requesting tool calls.
const MAX_TOOL_ROUNDS: usize = 10;

/// Chat mode — multi-turn conversation with optional MCP tool calling.
///
/// When an `McpManager` is attached, the agent will:
/// 1. Send the user message to the LLM with available tool definitions.
/// 2. If the LLM responds with tool calls, execute them via MCP.
/// 3. Feed the tool results back to the LLM.
/// 4. Repeat until the LLM produces a final text response (or max rounds).
pub struct ChatAgent {
    /// The LLM provider (trait object, provider-agnostic).
    llm: Box<dyn LlmApi>,
    /// The current conversation session (owns its own memory).
    session: Session,
    /// System prompt (kept for creating new sessions).
    system_prompt: String,
    /// Factory for creating memory instances (decoupled from concrete type).
    memory_factory: MemoryFactory,
    /// Optional MCP manager for tool calling.
    mcp: Option<McpManager>,
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
            mcp: None,
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
            tool_call_count = response.tool_calls.len(),
            prompt_tokens = response.usage.as_ref().and_then(|u| u.prompt_tokens),
            completion_tokens = response.usage.as_ref().and_then(|u| u.completion_tokens),
            total_tokens = response.usage.as_ref().and_then(|u| u.total_tokens),
            "LLM response: assistant output"
        );
    }

    /// Execute a single tool call via MCP and return a ToolResponse.
    async fn execute_tool_call(&self, tool_call: &ToolCall) -> ToolResponse {
        let mcp = match &self.mcp {
            Some(m) => m,
            None => return ToolResponse::new(&tool_call.call_id, "Error: No MCP manager available"),
        };

        tracing::info!(
            tool = %tool_call.fn_name,
            arguments = %tool_call.fn_arguments,
            "Executing MCP tool call"
        );

        match mcp.call_tool(&tool_call.fn_name, tool_call.fn_arguments.clone()).await {
            Ok(result) => {
                tracing::info!(
                    tool = %tool_call.fn_name,
                    result_len = result.len(),
                    "MCP tool call succeeded"
                );
                ToolResponse::new(&tool_call.call_id, result)
            }
            Err(e) => {
                tracing::error!(
                    tool = %tool_call.fn_name,
                    error = %e,
                    "MCP tool call failed"
                );
                ToolResponse::new(
                    &tool_call.call_id,
                    format!("Error calling tool '{}': {}", tool_call.fn_name, e),
                )
            }
        }
    }

    /// Build a summary of tool calls and results for storing in memory.
    ///
    /// This ensures the LLM can see tool usage history in subsequent turns.
    fn summarize_tool_history(history: &[(Vec<ToolCall>, Vec<ToolResponse>)]) -> String {
        let mut parts = Vec::new();
        for (round_idx, (calls, responses)) in history.iter().enumerate() {
            for (i, call) in calls.iter().enumerate() {
                let result = responses.get(i)
                    .map(|r| r.content.as_str())
                    .unwrap_or("(no result)");
                parts.push(format!(
                    "[Tool call round {}: {}({}) → {}]",
                    round_idx + 1,
                    call.fn_name,
                    call.fn_arguments,
                    result
                ));
            }
        }
        parts.join("\n")
    }

    /// Run the tool-calling loop.
    ///
    /// Iterates: LLM response → tool calls → execute via MCP → feed results back → repeat.
    /// All tool history is accumulated and passed to the provider on each round.
    /// Token usage is accumulated across all rounds.
    async fn chat_with_tools(
        &self,
        request_id: u64,
        messages: &[ChatMessage],
    ) -> Result<(ChatResponse, Vec<(Vec<ToolCall>, Vec<ToolResponse>)>)> {
        let tools = self.mcp.as_ref()
            .map(|m| m.build_tool_definitions())
            .unwrap_or_default();

        // Accumulated tool history: each entry is (tool_calls, tool_responses) for one round.
        let mut tool_history: Vec<(Vec<ToolCall>, Vec<ToolResponse>)> = Vec::new();

        // Accumulate token usage across all rounds
        let mut total_usage = TokenUsage::default();

        for round in 0..MAX_TOOL_ROUNDS {
            let response = self.llm.chat_with_tools(
                messages, &tools, &tool_history, None,
            ).await?;
            self.log_response(request_id, &response);

            // Accumulate usage from this round
            if let Some(ref usage) = response.usage {
                total_usage.prompt_tokens = Some(
                    total_usage.prompt_tokens.unwrap_or(0) + usage.prompt_tokens.unwrap_or(0)
                );
                total_usage.completion_tokens = Some(
                    total_usage.completion_tokens.unwrap_or(0) + usage.completion_tokens.unwrap_or(0)
                );
                total_usage.total_tokens = Some(
                    total_usage.total_tokens.unwrap_or(0) + usage.total_tokens.unwrap_or(0)
                );
            }

            if response.tool_calls.is_empty() {
                // No tool calls — this is the final response.
                // Replace usage with accumulated total.
                let final_response = ChatResponse {
                    content: response.content,
                    usage: Some(total_usage),
                    tool_calls: vec![],
                };
                return Ok((final_response, tool_history));
            }

            tracing::info!(
                round = round,
                tool_calls = response.tool_calls.len(),
                "LLM requested tool calls"
            );

            // Execute each tool call and collect responses
            let mut responses = Vec::new();
            for tc in &response.tool_calls {
                let tool_response = self.execute_tool_call(tc).await;
                responses.push(tool_response);
            }

            // Record this round's calls and responses
            tool_history.push((response.tool_calls, responses));

            // Continue the loop — send tool results back to LLM
        }

        anyhow::bail!("Exceeded maximum tool-calling rounds ({})", MAX_TOOL_ROUNDS)
    }
}

#[async_trait]
impl AgentMode for ChatAgent {
    async fn chat(&mut self, user_input: &str) -> Result<ChatResponse> {
        let request_id = self.session.next_request_id();

        // Store user message in session memory
        self.session.add_user_message(user_input);

        // Build the full message list from session memory
        let messages = self.session.build_messages();

        // Log the request
        self.log_request(request_id, user_input, &messages);

        // Decide whether to use tool calling
        let response = if self.has_tools() && self.llm.supports_tools() {
            let (resp, tool_history) = self.chat_with_tools(request_id, &messages).await?;

            // Store tool usage summary in memory so subsequent turns can see it.
            // The summary is stored as a separate assistant message, keeping the
            // final response clean.
            if !tool_history.is_empty() {
                let summary = Self::summarize_tool_history(&tool_history);
                self.session.add_assistant_message(&summary);
                self.session.add_user_message("Based on the tool results above, please provide your response.");
            }
            self.session.add_assistant_message(&resp.content);

            resp
        } else {
            // Simple chat without tools
            let resp = self.llm.chat(&messages, None).await?;
            self.log_response(request_id, &resp);
            self.session.add_assistant_message(&resp.content);
            resp
        };

        Ok(response)
    }

    fn attach_mcp(&mut self, mcp: McpManager) {
        tracing::info!(
            tools = mcp.tool_count(),
            "MCP manager attached to ChatAgent"
        );
        self.mcp = Some(mcp);
    }

    fn has_tools(&self) -> bool {
        self.mcp.as_ref().map(|m| m.has_servers()).unwrap_or(false)
    }

    fn tool_count(&self) -> usize {
        self.mcp.as_ref().map(|m| m.tool_count()).unwrap_or(0)
    }

    fn tool_descriptions(&self) -> Vec<ToolInfo> {
        self.mcp.as_ref()
            .map(|m| m.tool_descriptions())
            .unwrap_or_default()
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
        if self.has_tools() {
            "chat+tools"
        } else {
            "chat"
        }
    }
}
