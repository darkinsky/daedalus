use anyhow::Result;
use async_trait::async_trait;

use crate::config::AgentConfig;
use crate::llm::{
    ChatMessage, ChatResponse, LlmApi, ToolCall, ToolInfo, ToolResponse,
    TokenUsage, format_messages_for_log,
};
use crate::mcp::McpManager;
use crate::memory::{Memory, SlidingWindowMemory};
use crate::prompt::PromptBuilder;
use crate::session::Session;

use super::{AgentMode, ToolEvent, ToolEventCallback};
use super::tool_router::ToolRouter;

/// A factory function type for creating memory instances.
///
/// This allows `ChatAgent` to create new memory instances (e.g., when starting
/// a new session) without being coupled to a specific memory implementation.
pub type MemoryFactory = Box<dyn Fn(&str) -> Box<dyn Memory> + Send + Sync>;

/// Maximum number of tool-calling rounds per user message.
///
/// This prevents infinite loops if the LLM keeps requesting tool calls.
const MAX_TOOL_ROUNDS: usize = 10;

/// Truncate a string at a UTF-8 character boundary.
///
/// Returns a sub-slice of at most `max_len` bytes, guaranteed to end
/// on a valid character boundary (never splits a multi-byte character).
fn truncate_at_char_boundary(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        return s;
    }
    // Find the last char boundary at or before max_len.
    match s.char_indices().take_while(|(i, _)| *i <= max_len).last() {
        Some((i, _)) => &s[..i],
        None => &s[..0],
    }
}

/// Truncate a string for display preview, taking only the first line
/// and appending "…" if truncated.
fn preview_string(s: &str, max_len: usize) -> String {
    let first_line = s.lines().next().unwrap_or(s);
    if first_line.len() <= max_len {
        first_line.to_string()
    } else {
        format!("{}…", truncate_at_char_boundary(first_line, max_len))
    }
}

/// Chat mode — multi-turn conversation with optional tool calling.
///
/// `ChatAgent` is the core orchestrator that coordinates:
/// - **LLM interaction**: Sending messages and receiving responses.
/// - **Tool execution**: Delegated to `ToolRouter` (built-in + MCP).
/// - **Memory management**: Storing conversation history via `Session`.
/// - **Prompt construction**: Delegated to `PromptBuilder`.
///
/// The tool-calling loop works as follows:
/// 1. Send the user message to the LLM with available tool definitions.
/// 2. If the LLM responds with tool calls, execute them via `ToolRouter`.
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
    /// Unified tool router — handles both built-in and MCP tools.
    tool_router: ToolRouter,
    /// Custom system prompt override (from DAEDALUS_SYSTEM_PROMPT env var).
    /// When set, bypasses PromptBuilder entirely.
    prompt_override: Option<String>,
    /// Custom agent name for prompt building.
    agent_name: Option<String>,
    /// Soul/personality content loaded from SOUL.md.
    soul: Option<String>,
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
        let prompt_override = if config.is_custom_prompt {
            Some(config.system_prompt.clone())
        } else {
            None
        };

        let system_prompt = Self::build_prompt(
            prompt_override.as_deref(),
            config.agent_name.as_deref(),
            config.soul.as_deref(),
            &[],
        );

        let memory = memory_factory(&system_prompt);
        let session = Session::new(memory);

        tracing::info!(
            mode = "chat",
            memory_strategy = session.memory().strategy_name(),
            provider = llm.provider_name(),
            model = llm.model_name(),
            prompt_len = system_prompt.len(),
            "ChatAgent initialized with dynamic prompt"
        );

        Self {
            llm,
            session,
            system_prompt,
            memory_factory,
            tool_router: ToolRouter::new(),
            prompt_override,
            agent_name: config.agent_name.clone(),
            soul: config.soul.clone(),
        }
    }

    /// Create a new chat agent with the default memory strategy
    /// (sliding window with dual-layer consolidation).
    pub fn new(llm: Box<dyn LlmApi>, config: &AgentConfig) -> Self {
        let factory: MemoryFactory = Box::new(|prompt: &str| {
            Box::new(SlidingWindowMemory::with_defaults(prompt))
        });
        Self::with_memory_factory(llm, config, factory)
    }

    // ── Prompt construction ──

    /// Build the system prompt using PromptBuilder.
    ///
    /// Delegates to `PromptBuilder::build_with_override` which handles
    /// the "custom override vs. dynamic assembly" decision.
    fn build_prompt(
        prompt_override: Option<&str>,
        agent_name: Option<&str>,
        soul: Option<&str>,
        tools: &[ToolInfo],
    ) -> String {
        let mut builder = PromptBuilder::new().tools(tools);
        if let Some(name) = agent_name {
            builder = builder.agent_name(name);
        }
        if let Some(soul_content) = soul {
            builder = builder.soul(soul_content);
        }
        builder.build_with_override(prompt_override)
    }

    /// Rebuild the system prompt and reset the session, preserving
    /// long-term memory and history log across the reset.
    ///
    /// Called when the tool set changes (e.g., after MCP attachment) so the
    /// LLM sees updated tool guidance in the system prompt.
    fn reset_with_updated_prompt(&mut self) {
        let tools = self.tool_router.tool_descriptions();
        self.system_prompt = Self::build_prompt(
            self.prompt_override.as_deref(),
            self.agent_name.as_deref(),
            self.soul.as_deref(),
            &tools,
        );

        self.session = self.create_session_with_migration();

        tracing::info!(
            prompt_len = self.system_prompt.len(),
            "System prompt rebuilt with updated tool definitions"
        );
    }

    /// Create a new session, migrating persistent state (long-term memory
    /// and history log) from the current session into the new one.
    ///
    /// This is the single place that handles the
    /// `take_persistent_state → memory_factory → restore_persistent_state`
    /// lifecycle, used by both `reset_with_updated_prompt` and `new_session`.
    fn create_session_with_migration(&mut self) -> Session {
        // Extract persistent state from the old session (if supported).
        let persistent_state = self.session
            .memory_as_mut::<SlidingWindowMemory>()
            .map(|swm| swm.take_persistent_state());

        let mut memory = (self.memory_factory)(&self.system_prompt);

        // Restore persistent state into the new memory if available.
        if let Some((ltm, log)) = persistent_state {
            if let Some(swm) = memory.as_any_mut().downcast_mut::<SlidingWindowMemory>() {
                swm.restore_persistent_state(ltm, log);
            }
        }

        Session::new(memory)
    }

    // ── Logging helpers ──

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
            memory_strategy = self.session.memory().strategy_name(),
            turn_count = self.session.memory().turn_count(),
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

    // ── Tool-calling loop ──

    /// Build a summary of tool calls and results for storing in memory.
    ///
    /// This ensures the LLM can see tool usage history in subsequent turns.
    /// Arguments and results are truncated to avoid wasting tokens on
    /// excessively large tool payloads.
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
                    call.function_name,
                    truncate_at_char_boundary(&call.arguments.to_string(), 200),
                    truncate_at_char_boundary(result, 500),
                ));
            }
        }
        parts.join("\n")
    }

    /// Emit a tool event to the optional callback.
    fn emit_event(callback: Option<&ToolEventCallback>, event: ToolEvent) {
        if let Some(cb) = callback {
            cb(event);
        }
    }

    /// Run the tool-calling loop.
    ///
    /// Iterates: LLM response → tool calls → execute via ToolRouter → feed results back.
    /// All tool history is accumulated and passed to the provider on each round.
    /// Token usage is accumulated across all rounds.
    async fn chat_with_tools(
        &self,
        request_id: u64,
        messages: &[ChatMessage],
        on_tool_event: Option<&ToolEventCallback>,
    ) -> Result<(ChatResponse, Vec<(Vec<ToolCall>, Vec<ToolResponse>)>)> {
        let tools = self.tool_router.build_tool_definitions();
        let mut tool_history: Vec<(Vec<ToolCall>, Vec<ToolResponse>)> = Vec::new();
        let mut total_usage = TokenUsage::default();
        let mut last_reasoning_content: Option<String> = None;

        for round in 0..MAX_TOOL_ROUNDS {
            let response = self.llm.chat_with_tools(
                messages, &tools, &tool_history, None,
            ).await?;
            self.log_response(request_id, &response);

            if let Some(ref usage) = response.usage {
                total_usage.accumulate(usage);
            }

            if response.tool_calls.is_empty() {
                let final_response = ChatResponse {
                    content: response.content,
                    reasoning_content: response.reasoning_content.or(last_reasoning_content),
                    usage: Some(total_usage),
                    tool_calls: vec![],
                };
                return Ok((final_response, tool_history));
            }

            if response.reasoning_content.is_some() {
                last_reasoning_content = response.reasoning_content;
            }

            tracing::info!(
                round = round,
                tool_calls = response.tool_calls.len(),
                "LLM requested tool calls"
            );

            // Notify CLI about the new round
            Self::emit_event(on_tool_event, ToolEvent::RoundStart { round: round + 1 });

            // Emit ToolCallStart events for all tool calls upfront
            for tc in &response.tool_calls {
                let source = if self.tool_router.is_builtin(&tc.function_name) {
                    "built-in"
                } else {
                    "mcp"
                };
                Self::emit_event(on_tool_event, ToolEvent::ToolCallStart {
                    tool_name: tc.function_name.clone(),
                    source: source.to_string(),
                });
            }

            // Execute all tool calls in parallel via the unified router
            let futures: Vec<_> = response.tool_calls.iter()
                .map(|tc| self.tool_router.execute(tc))
                .collect();
            let responses: Vec<ToolResponse> = futures::future::join_all(futures).await;

            // Emit ToolCallComplete events for all results
            for (tool_call, tool_response) in response.tool_calls.iter().zip(responses.iter()) {
                let success = tool_response.success;
                let result_preview = preview_string(&tool_response.content, 80);
                Self::emit_event(on_tool_event, ToolEvent::ToolCallComplete {
                    tool_name: tool_call.function_name.clone(),
                    success,
                    result_preview,
                });
            }
            Self::emit_event(on_tool_event, ToolEvent::RoundComplete {
                tool_count: responses.len(),
            });

            tool_history.push((response.tool_calls, responses));
        }

        anyhow::bail!("Exceeded maximum tool-calling rounds ({})", MAX_TOOL_ROUNDS)
    }
}

#[async_trait]
impl AgentMode for ChatAgent {
    async fn chat(
        &mut self,
        user_input: &str,
        on_tool_event: Option<&ToolEventCallback>,
    ) -> Result<ChatResponse> {
        let request_id = self.session.next_request_id();

        self.session.memory_mut().add_user_message(user_input);
        let messages = self.session.memory().build_messages();
        self.log_request(request_id, user_input, &messages);

        let response = if self.has_tools() && self.llm.supports_tools() {
            let (resp, tool_history) = self.chat_with_tools(
                request_id, &messages, on_tool_event,
            ).await?;

            if !tool_history.is_empty() {
                let summary = Self::summarize_tool_history(&tool_history);
                self.session.memory_mut().add_tool_context(&summary);
            }
            self.session.memory_mut().add_assistant_message(&resp.content);
            resp
        } else {
            let resp = self.llm.chat(&messages, None).await?;
            self.log_response(request_id, &resp);
            self.session.memory_mut().add_assistant_message(&resp.content);
            resp
        };

        // Check if consolidation should be triggered after this turn.
        if self.session.memory().should_consolidate() {
            tracing::info!(
                session_id = %self.session.id,
                "Consolidation threshold reached — consolidation should be triggered"
            );
            // TODO: Trigger background consolidation via LLM.
            // For now, we log the event. The actual consolidation LLM call
            // will be implemented as a separate async task in a future iteration.
        }

        Ok(response)
    }

    fn attach_mcp(&mut self, mcp: McpManager) {
        self.tool_router.attach_mcp(mcp);
        self.reset_with_updated_prompt();
    }

    fn has_tools(&self) -> bool {
        self.tool_router.has_tools()
    }

    fn tool_count(&self) -> usize {
        self.tool_router.tool_count()
    }

    fn tool_descriptions(&self) -> Vec<ToolInfo> {
        self.tool_router.tool_descriptions()
    }

    fn new_session(&mut self) {
        let tools = self.tool_router.tool_descriptions();
        self.system_prompt = Self::build_prompt(
            self.prompt_override.as_deref(),
            self.agent_name.as_deref(),
            self.soul.as_deref(),
            &tools,
        );

        self.session = self.create_session_with_migration();

        tracing::info!(
            session_id = %self.session.id,
            "New session created with migrated persistent memory"
        );
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
