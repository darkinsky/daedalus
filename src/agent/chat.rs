use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;

use crate::config::AgentConfig;
use crate::llm::{
    ChatMessage, ChatResponse, LlmApi, LlmConfig, ToolResponse, ToolRound,
    format_messages_for_log,
};
use crate::tools::{truncate_at_char_boundary, ToolEventCallback, ToolInfo};
use crate::mcp::McpManager;
use crate::memory::{MemoryFactory, SlidingWindowFactory};
use crate::prompt::PromptBuilder;
use crate::skill::SkillInfo;
use crate::subagent::SubagentInfo;
use crate::workspace::Workspace;

use super::Session;

use super::{AgentMetadata, AgentMode};
use super::tool_loop::{run_tool_loop, LoopConfig, LoopOutcome, LoopResult, ToolExecutor};
use super::tool_router::ToolRouter;

/// Default maximum number of tool-calling rounds per user message.
///
/// Bounds the main agent's tool-calling loop so a misbehaving LLM cannot
/// spin forever. The user can override this via the `--max-turns` CLI flag;
/// subagents have their own independent default in `subagent::runner`.
const DEFAULT_MAX_TOOL_ROUNDS: usize = 100;

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
    memory_factory: Box<dyn MemoryFactory>,
    /// Unified tool router — handles both built-in and MCP tools.
    tool_router: ToolRouter,
    /// Custom system prompt override (from DAEDALUS_SYSTEM_PROMPT env var).
    /// When set, bypasses PromptBuilder entirely.
    prompt_override: Option<String>,
    /// Custom agent name for prompt building.
    agent_name: Option<String>,
    /// Soul/personality content loaded from SOUL.md.
    soul: Option<String>,
    /// Workspace for file I/O (memory persistence, etc.).
    workspace: Option<Workspace>,
    /// Maximum tool-calling rounds per user message (overridable via CLI).
    max_tool_rounds: usize,
}

impl ChatAgent {
    /// Create a new chat agent with the given LLM provider, configuration,
    /// and memory factory.
    ///
    /// The `memory_factory` creates memory instances for new sessions.
    /// This decouples `ChatAgent` from any specific memory implementation.
    pub fn with_memory_factory(
        llm: Box<dyn LlmApi>,
        config: &AgentConfig,
        memory_factory: Box<dyn MemoryFactory>,
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

        let memory = memory_factory.create_memory(&system_prompt);
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
            workspace: None,
            max_tool_rounds: DEFAULT_MAX_TOOL_ROUNDS,
        }
    }

    /// Create a new chat agent with the default memory strategy
    /// (sliding window with dual-layer consolidation).
    #[allow(dead_code)]
    pub fn new(llm: Box<dyn LlmApi>, config: &AgentConfig) -> Self {
        Self::with_memory_factory(llm, config, Box::new(SlidingWindowFactory::new()))
    }

    /// Create a new chat agent with workspace support.
    ///
    /// The workspace is used for:
    /// - Loading persisted memory at startup (strategy-dependent paths)
    /// - Saving memory state on shutdown
    ///
    /// The memory strategy is selected from `config.memory_strategy`.
    /// Factory creation is delegated to `memory::create_memory_factory`.
    pub fn new_with_workspace(
        llm: Box<dyn LlmApi>,
        config: &AgentConfig,
        workspace: Workspace,
    ) -> Self {
        let factory = crate::memory::create_memory_factory(
            &config.memory_strategy,
            &config.embedding,
            &workspace,
        );
        let mut agent = Self::with_memory_factory(llm, config, factory);
        agent.workspace = Some(workspace);
        agent
    }

    /// Load skills from a directory and rebuild the system prompt.
    ///
    /// Skills are exposed to the LLM as a `use_skill` tool. The LLM
    /// decides which skill to invoke based on the user's request.
    ///
    /// Internally this splits into two concerns:
    /// 1. **Load** the skill definitions via `SkillRegistry::load_from_dir`.
    /// 2. **Install** the ready registry into the `ToolRouter`, which
    ///    wires a `use_skill` built-in tool.
    ///
    /// Keeping the split explicit here makes the router easier to reason
    /// about (it knows nothing about the filesystem) and leaves the door
    /// open to loading from alternative sources in the future.
    pub fn load_skills(&mut self, dir: &Path) -> Result<usize> {
        let mut registry = crate::skill::SkillRegistry::new();
        let count = registry.load_from_dir(dir)?;
        if count > 0 {
            self.tool_router.install_skills(std::sync::Arc::new(registry));
            self.reset_with_updated_prompt();
        }
        Ok(count)
    }

    /// Load subagent definitions from directories and rebuild the system prompt.
    ///
    /// Subagents are exposed to the LLM as a `spawn_subagent` tool.
    /// The LLM decides which subagent to invoke based on the subagent
    /// descriptions embedded in the tool definition.
    ///
    /// Each directory is associated with a source priority. Built-in
    /// agents are registered first (lowest priority), then each
    /// `(dir, source)` pair is loaded in order — later sources override
    /// earlier ones. Typical call order: `[Project, Global]` so that
    /// project-level agents win.
    pub fn load_subagents(
        &mut self,
        dirs: &[&Path],
        sources: &[crate::subagent::SubagentSource],
        parent_llm_config: LlmConfig,
    ) -> Result<usize> {
        let mut registry = crate::subagent::SubagentRegistry::new();

        // Built-ins first so user-defined agents with the same name take
        // precedence when the directories are scanned.
        registry.register_builtins();

        for (dir, source) in dirs.iter().zip(sources.iter()) {
            registry.load_from_dir(dir, source.clone())?;
        }

        // `agent_count()` is the post-dedup total; that's what the caller
        // cares about and what the router uses to decide whether to
        // register the `spawn_subagent` / `spawn_team` tools.
        let count = registry.agent_count();
        if count > 0 {
            self.tool_router
                .install_subagents(std::sync::Arc::new(registry), parent_llm_config);
            self.reset_with_updated_prompt();
        }
        Ok(count)
    }

    /// Set a tool filter for --allowed-tools / --disallowed-tools.
    ///
    /// When set, only tools matching the filter are exposed to the LLM
    /// and allowed to execute. The system prompt is rebuilt to reflect
    /// the filtered tool set.
    pub fn set_tool_filter(&mut self, filter: Option<super::tool_router::ToolFilter>) {
        self.tool_router.set_tool_filter(filter);
        self.reset_with_updated_prompt();
    }

    /// Set the maximum number of tool-calling rounds per user message.
    ///
    /// Used by the `--max-turns` CLI flag. A value of 0 means use the
    /// internal default.
    pub fn set_max_tool_rounds(&mut self, max_tool_rounds: usize) {
        self.max_tool_rounds = if max_tool_rounds == 0 {
            DEFAULT_MAX_TOOL_ROUNDS
        } else {
            max_tool_rounds
        };
        tracing::info!(max_tool_rounds = self.max_tool_rounds, "Max tool rounds updated");
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
        let tools = self.tool_router.tool_infos();
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
    /// Uses the `Memory` trait's `take_persistent_state` / `restore_persistent_state`
    /// methods, so this works with any memory strategy that supports migration
    /// — no hardcoded downcasting to a specific implementation.
    fn create_session_with_migration(&mut self) -> Session {
        // Extract persistent state from the old session via the trait method.
        let persistent_state = self.session.memory_mut().take_persistent_state();

        let mut memory = self.memory_factory.create_memory(&self.system_prompt);

        // Restore persistent state into the new memory if available.
        if let Some(state) = persistent_state {
            memory.restore_persistent_state(state);
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
    fn summarize_tool_history(history: &[ToolRound]) -> String {
        let mut parts = Vec::new();
        for (round_idx, round) in history.iter().enumerate() {
            for (i, call) in round.calls.iter().enumerate() {
                let result = round.responses.get(i)
                    .map(|r| r.content.as_str())
                    .unwrap_or("(no result)");
                parts.push(format!(
                    "[Tool call round {}: {}({}) -> {}]",
                    round_idx + 1,
                    call.function_name,
                    truncate_at_char_boundary(&call.arguments.to_string(), 200),
                    truncate_at_char_boundary(result, 500),
                ));
            }
        }
        parts.join("\n")
    }

    /// Run the tool-calling loop.
    ///
    /// Thin wrapper over `tool_loop::run_tool_loop`: builds the executor
    /// adapter, picks loop config, and translates the generic
    /// `LoopOutcome` into `ChatAgent`'s fail-hard semantics
    /// (duplicate-stop / max-rounds both `bail!`).
    async fn chat_with_tools(
        &self,
        request_id: u64,
        messages: &[ChatMessage],
        on_tool_event: Option<&ToolEventCallback>,
    ) -> Result<(ChatResponse, Vec<ToolRound>)> {
        let tools = self.tool_router.build_tool_definitions();
        let executor = ToolRouterExecutor { router: &self.tool_router };
        let cfg = LoopConfig {
            max_tool_rounds: self.max_tool_rounds,
            agent_label: "Lead agent".to_string(),
            track_reasoning: true,
        };

        // Per-round LLM response logging — lets chat_with_tools keep the
        // log-with-request-id semantics without leaking request_id into
        // the generic loop.
        let log_cb = |resp: &ChatResponse| self.log_response(request_id, resp);

        let LoopResult { outcome, usage, tool_history } = run_tool_loop(
            &*self.llm,
            &executor,
            messages,
            &tools,
            on_tool_event,
            &cfg,
            Some(&log_cb),
        ).await?;

        match outcome {
            LoopOutcome::Final { content, reasoning } => {
                let final_response = ChatResponse {
                    content,
                    reasoning_content: reasoning,
                    usage: Some(usage),
                    tool_calls: vec![],
                };
                Ok((final_response, tool_history))
            }
            LoopOutcome::DuplicateStop { message } => anyhow::bail!("{}", message),
            LoopOutcome::MaxRoundsExceeded => anyhow::bail!(
                "Exceeded maximum tool-calling rounds ({})",
                self.max_tool_rounds
            ),
        }
    }
}

/// Adapter that lets `ToolRouter` satisfy `tool_loop::ToolExecutor`.
///
/// Kept as a private newtype so the coupling between the generic loop
/// and the routing layer stays local to this file. The lifetime is tied
/// to the borrow of the router — callers build the adapter on the stack
/// just before invoking `run_tool_loop` and drop it right after.
struct ToolRouterExecutor<'a> {
    router: &'a ToolRouter,
}

#[async_trait]
impl<'a> ToolExecutor for ToolRouterExecutor<'a> {
    async fn execute(&self, call: &crate::llm::ToolCall) -> ToolResponse {
        self.router.execute(call).await
    }

    fn source_of(&self, tool_name: &str) -> String {
        if self.router.is_builtin(tool_name) {
            "built-in".to_string()
        } else {
            "mcp".to_string()
        }
    }
}

// ── AgentMetadata implementation (read-only introspection) ──

impl AgentMetadata for ChatAgent {
    fn has_tools(&self) -> bool {
        self.tool_router.has_tools()
    }

    fn tool_count(&self) -> usize {
        self.tool_router.tool_count()
    }

    fn tool_infos(&self) -> Vec<ToolInfo> {
        self.tool_router.tool_infos()
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

    fn skill_infos(&self) -> Vec<SkillInfo> {
        self.tool_router.skill_registry().skill_infos()
    }

    fn skill_count(&self) -> usize {
        self.tool_router.skill_registry().skill_count()
    }

    fn subagent_infos(&self) -> Vec<SubagentInfo> {
        self.tool_router.subagent_registry().agent_infos()
    }

    fn subagent_count(&self) -> usize {
        self.tool_router.subagent_registry().agent_count()
    }
}

// ── AgentMode implementation (core behavior) ──

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
            let (final_resp, tool_history) = self.chat_with_tools(
                request_id, &messages, on_tool_event,
            ).await?;

            if !tool_history.is_empty() {
                let summary = Self::summarize_tool_history(&tool_history);
                self.session.memory_mut().add_tool_context(&summary);
            }
            self.session.memory_mut().add_assistant_message(&final_resp.content);
            final_resp
        } else {
            let llm_resp = self.llm.chat(&messages, None).await?;
            self.log_response(request_id, &llm_resp);
            self.session.memory_mut().add_assistant_message(&llm_resp.content);
            llm_resp
        };

        // Check if consolidation should be triggered after this turn.
        // The actual LLM-backed consolidation is not yet implemented;
        // for now we only surface the threshold event for observability.
        if self.session.memory().should_consolidate() {
            tracing::debug!(
                session_id = %self.session.id,
                "Consolidation threshold reached"
            );
        }

        // Trigger post-turn reflection (strategy-specific: DC insight
        // extraction, A-MEM note storage + context pre-retrieval, etc.).
        self.session.memory_mut().reflect_on_turn(
            user_input, &response.content, &*self.llm,
        ).await;

        Ok(response)
    }

    fn attach_mcp(&mut self, mcp: McpManager) {
        self.tool_router.attach_mcp(mcp);
        self.reset_with_updated_prompt();
    }

    fn new_session(&mut self) {
        let tools = self.tool_router.tool_infos();
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

    fn set_subagent_event_callback(&self, callback: Option<ToolEventCallback>) {
        self.tool_router.set_subagent_event_callback(callback);
    }

    async fn shutdown(&mut self) -> Result<()> {
        // 1. Persist memory state via the Memory trait (no downcast needed)
        if let Some(ref workspace) = self.workspace {
            tracing::info!("Persisting memory to workspace...");

            if let Err(e) = self.session.memory().persist(workspace) {
                tracing::error!(error = %e, "Failed to persist memory to workspace");
                return Err(e);
            }

            // Save last session ID
            if let Err(e) = std::fs::write(
                workspace.last_session_id_path(),
                &self.session.id,
            ) {
                tracing::warn!(error = %e, "Failed to save last session ID");
            }

            tracing::info!(
                session_id = %self.session.id,
                "Memory persisted to workspace successfully"
            );
        }

        // 2. Shut down MCP servers to prevent orphaned child processes
        self.tool_router.shutdown().await;

        Ok(())
    }
}
