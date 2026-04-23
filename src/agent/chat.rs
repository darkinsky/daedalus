use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::config::AgentConfig;
use crate::llm::{ChatResponse, LlmApi, LlmConfig};
use crate::tools::{ToolEventCallback, ToolInfo};
use crate::mcp::McpManager;
use crate::memory::{MemoryFactory, SlidingWindowFactory};
use crate::middleware::{Extensions, TurnRequest};
use crate::middleware::pipeline::TurnPipeline;
use crate::middleware::builtin::tracing::TracingTurnMiddleware;
use crate::middleware::builtin::logging::LoggingTurnMiddleware;
use crate::middleware::builtin::memory::MemoryTurnMiddleware;
use crate::middleware::builtin::cost::{CostTurnMiddleware, SessionCost, SharedSessionCost};
use crate::middleware::builtin::metrics::MetricsTurnMiddleware;
use crate::middleware::config::MiddlewareConfig;
use crate::prompt::PromptStyle;
use crate::skill::SkillInfo;
use crate::subagent::SubagentInfo;
use crate::agent_tracing;
use crate::workspace::Workspace;

use super::Session;
use super::core_handler::CoreTurnHandler;
use super::{AgentMetadata, AgentMode};
use super::tool_router::ToolRouter;

/// Default maximum number of tool-calling rounds per user message.
const DEFAULT_MAX_TOOL_ROUNDS: usize = 200;

/// Chat mode — multi-turn conversation with optional tool calling.
///
/// Cross-cutting concerns (tracing, logging) are handled by the middleware
/// pipeline, keeping the core logic focused on LLM interaction and tool execution.
pub struct ChatAgent {
    /// The LLM provider (shared via Arc for middleware/core access).
    llm: Arc<dyn LlmApi>,
    /// The current conversation session.
    session: Session,
    /// System prompt (kept for creating new sessions).
    system_prompt: String,
    /// Factory for creating memory instances.
    memory_factory: Box<dyn MemoryFactory>,
    /// Unified tool router (shared via Arc for core handler access).
    tool_router: Arc<ToolRouter>,
    /// Custom system prompt override.
    prompt_override: Option<String>,
    /// Custom agent name for prompt building.
    agent_name: Option<String>,
    /// Soul/personality content loaded from SOUL.md.
    soul: Option<String>,
    /// Project rules content loaded from DAEDALUS.md files.
    project_rules: Option<String>,
    /// Prompt assembly style (default vs coding).
    prompt_style: PromptStyle,
    /// Workspace for file I/O.
    workspace: Option<Workspace>,
    /// Maximum tool-calling rounds per user message.
    max_tool_rounds: usize,
    /// Tracing manager for observability.
    tracing_manager: Option<Arc<agent_tracing::TracingManager>>,
    /// Middleware pipeline configuration (loaded from YAML).
    middleware_config: MiddlewareConfig,
    /// Shared session cost tracker (accessible by CLI for /cost display).
    session_cost: SharedSessionCost,
    /// Updatable memory handle for the recall_history tool.
    /// `None` if the memory strategy doesn't support history search.
    memory_handle: Option<crate::tools::recall_history::MemoryHandle>,
}

impl ChatAgent {
    /// Create a new chat agent with the given LLM provider, configuration,
    /// and memory factory.
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
            config.project_rules.as_deref(),
            &[],
            &config.prompt_style,
            None,
        );

        let memory = memory_factory.create_memory(&system_prompt);
        let session = Session::new(memory);
        let llm: Arc<dyn LlmApi> = Arc::from(llm);

        tracing::info!(
            mode = "chat",
            memory_strategy = session.shared_memory().try_lock()
                .map(|m| m.strategy_name().to_string())
                .unwrap_or_else(|_| "unknown".to_string()),
            provider = llm.provider_name(),
            model = llm.model_name(),
            prompt_len = system_prompt.len(),
            "ChatAgent initialized with middleware pipeline"
        );

        Self {
            llm,
            session,
            system_prompt,
            memory_factory,
            tool_router: Arc::new(ToolRouter::new()),
            prompt_override,
            agent_name: config.agent_name.clone(),
            soul: config.soul.clone(),
            project_rules: config.project_rules.clone(),
            prompt_style: config.prompt_style.clone(),
            workspace: None,
            max_tool_rounds: DEFAULT_MAX_TOOL_ROUNDS,
            tracing_manager: None,
            middleware_config: MiddlewareConfig::default(),
            session_cost: Arc::new(std::sync::Mutex::new(SessionCost::new())),
            memory_handle: None,
        }
    }

    #[allow(dead_code)]
    pub fn new(llm: Box<dyn LlmApi>, config: &AgentConfig) -> Self {
        Self::with_memory_factory(llm, config, Box::new(SlidingWindowFactory::new()))
    }

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

        // Register the recall_history tool for memory strategies that support
        // history search (currently SlidingWindow). This gives the LLM the
        // ability to search past conversation summaries on demand.
        let shared_memory = agent.session.shared_memory();
        let is_sliding_window = shared_memory.try_lock()
            .map(|m| m.strategy_name() == "sliding_window")
            .unwrap_or(false);
        if is_sliding_window {
            let handle = crate::tools::recall_history::new_memory_handle(shared_memory);
            let tool = crate::tools::recall_history::RecallHistoryTool::new(handle.clone());
            agent.memory_handle = Some(handle);
            agent.router_mut_exclusive().register_builtin_tool(Box::new(tool));
        }

        agent
    }

    // ── Extension loading ──

    /// Get exclusive mutable access to the tool router.
    ///
    /// # Panics
    ///
    /// Panics if the `Arc<ToolRouter>` has been cloned (i.e., a pipeline is
    /// currently running). This is only safe during setup (load_skills,
    /// load_subagents, etc.) before the router is shared with any pipeline.
    fn router_mut_exclusive(&mut self) -> &mut ToolRouter {
        Arc::get_mut(&mut self.tool_router)
            .expect("ToolRouter should have no other references during setup")
    }

    pub fn load_skills(&mut self, dir: &Path) -> Result<usize> {
        let mut registry = crate::skill::SkillRegistry::new();
        let count = registry.load_from_dir(dir)?;
        if count > 0 {
            self.router_mut_exclusive().install_skills(std::sync::Arc::new(registry));
            self.reset_with_updated_prompt();
        }
        Ok(count)
    }

    pub fn load_subagents(
        &mut self,
        dirs: &[&Path],
        sources: &[crate::subagent::SubagentSource],
        parent_llm_config: LlmConfig,
    ) -> Result<usize> {
        let mut registry = crate::subagent::SubagentRegistry::new();
        registry.register_builtins();
        for (dir, source) in dirs.iter().zip(sources.iter()) {
            registry.load_from_dir(dir, source.clone())?;
        }
        let count = registry.agent_count();
        if count > 0 {
            self.router_mut_exclusive()
                .install_subagents(std::sync::Arc::new(registry), parent_llm_config);
            self.reset_with_updated_prompt();
        }
        Ok(count)
    }

    pub fn set_tool_filter(&mut self, filter: Option<super::tool_router::ToolFilter>) {
        self.router_mut_exclusive().set_tool_filter(filter);
        self.reset_with_updated_prompt();
    }

    /// Install an ACP tool into the tool router.
    ///
    /// This registers the `call_acp_agent` built-in tool, making ACP agents
    /// available to the LLM through the standard tool-calling interface.
    pub fn install_acp_tool(&mut self, tool: Box<dyn crate::tools::BuiltinTool>) {
        self.router_mut_exclusive().register_builtin_tool(tool);
        self.reset_with_updated_prompt();
    }

    pub fn set_max_tool_rounds(&mut self, max_tool_rounds: usize) {
        self.max_tool_rounds = if max_tool_rounds == 0 {
            DEFAULT_MAX_TOOL_ROUNDS
        } else {
            max_tool_rounds
        };
        tracing::info!(max_tool_rounds = self.max_tool_rounds, "Max tool rounds updated");
    }

    pub fn set_tracing_manager(&mut self, manager: Arc<agent_tracing::TracingManager>) {
        self.tracing_manager = Some(manager);
    }

    /// Set the middleware pipeline configuration (loaded from YAML).
    pub fn set_middleware_config(&mut self, config: MiddlewareConfig) {
        tracing::info!(
            turn_layers = config.turn.len(),
            tool_layers = config.tool.len(),
            "Middleware configuration applied"
        );
        self.middleware_config = config;
    }

    /// Return the shared session cost handle for external access (e.g., CLI `/cost`).
    #[allow(dead_code)]
    pub fn session_cost(&self) -> &SharedSessionCost {
        &self.session_cost
    }

    // ── Prompt construction ──

    /// Build the system prompt, delegating to `prompt::build_system_prompt`.
    ///
    /// This thin wrapper resolves the workspace CWD and forwards all
    /// parameters to the centralized prompt builder in `prompt/mod.rs`.
    fn build_prompt(
        prompt_override: Option<&str>,
        agent_name: Option<&str>,
        soul: Option<&str>,
        project_rules: Option<&str>,
        tools: &[ToolInfo],
        style: &PromptStyle,
        workspace: Option<&Workspace>,
    ) -> String {
        let cwd = workspace.map(|ws| ws.project_dir().to_string_lossy().to_string());
        crate::prompt::build_system_prompt(
            prompt_override,
            agent_name,
            soul,
            project_rules,
            tools,
            style,
            cwd.as_deref(),
        )
    }

    fn reset_with_updated_prompt(&mut self) {
        let tools = self.tool_router.tool_infos();
        self.system_prompt = Self::build_prompt(
            self.prompt_override.as_deref(),
            self.agent_name.as_deref(),
            self.soul.as_deref(),
            self.project_rules.as_deref(),
            &tools,
            &self.prompt_style,
            self.workspace.as_ref(),
        );
        self.session = self.create_session_with_migration();
        tracing::info!(
            prompt_len = self.system_prompt.len(),
            "System prompt rebuilt with updated tool definitions"
        );
    }

    fn create_session_with_migration(&mut self) -> Session {
        let shared = self.session.shared_memory();
        let persistent_state = {
            // try_lock is safe here: this is only called during setup/new_session
            // when no middleware pipeline is running (no concurrent access).
            let mut mem = shared.try_lock()
                .expect("Memory should not be locked during session migration");
            mem.take_persistent_state()
        };
        let mut memory = self.memory_factory.create_memory(&self.system_prompt);
        if let Some(state) = persistent_state {
            memory.restore_persistent_state(state);
        }
        let new_session = Session::new(memory);

        // Update the memory handle so the recall_history tool sees the new session's memory.
        if let Some(ref handle) = self.memory_handle {
            if let Ok(mut guard) = handle.write() {
                *guard = new_session.shared_memory();
            }
        }

        new_session
    }

    // ── Pipeline construction ──

    /// Build the turn middleware pipeline for a single chat() call.
    ///
    /// Reads `self.middleware_config.turn` to decide which middleware layers
    /// are active. Config order is **innermost first** (matching `.with()` semantics).
    ///
    /// If the config is empty (default), uses the built-in default stack.
    /// Memory middleware is **always** included as a safety measure — even if
    /// accidentally omitted from config, it's injected as the innermost layer.
    fn build_turn_pipeline(
        &self,
        on_tool_event: Option<ToolEventCallback>,
    ) -> TurnPipeline {
        let core = Box::new(CoreTurnHandler::new(
            Arc::clone(&self.llm),
            Arc::clone(&self.tool_router),
            self.max_tool_rounds,
            on_tool_event,
            self.middleware_config.tool.clone(),
        ));

        let mut pipeline = TurnPipeline::new(core);

        // Determine if tracing is globally enabled (from TracingManager)
        let tracing_globally_enabled = self.tracing_manager
            .as_ref()
            .map(|mgr| mgr.is_enabled())
            .unwrap_or(false);

        if self.middleware_config.turn.is_empty() {
            // ── Default stack (innermost first) ──
            // memory → cost → metrics → request_logging → tracing
            pipeline = self.add_turn_layer(pipeline, "memory", tracing_globally_enabled);
            pipeline = self.add_turn_layer(pipeline, "cost", tracing_globally_enabled);
            pipeline = self.add_turn_layer(pipeline, "metrics", tracing_globally_enabled);
            pipeline = self.add_turn_layer(pipeline, "request_logging", tracing_globally_enabled);
            if tracing_globally_enabled {
                pipeline = self.add_turn_layer(pipeline, "tracing", tracing_globally_enabled);
            }
        } else {
            // ── Config-driven stack (innermost first, no reversal needed) ──
            // Safety: ensure memory is always present as innermost layer
            let has_memory = self.middleware_config.turn.iter()
                .any(|e| e.name == "memory" && e.enabled);
            if !has_memory {
                tracing::warn!("Turn middleware config missing 'memory' — injecting as innermost layer");
                pipeline = self.add_turn_layer(pipeline, "memory", tracing_globally_enabled);
            }

            for entry in &self.middleware_config.turn {
                if !entry.enabled {
                    tracing::debug!(middleware = %entry.name, "Turn middleware disabled by config");
                    continue;
                }
                pipeline = self.add_turn_layer(pipeline, &entry.name, tracing_globally_enabled);
            }
        }

        pipeline
    }

    /// Add a single turn middleware layer to the pipeline by name.
    ///
    /// Centralizes the name → middleware mapping so that both the default
    /// stack and the config-driven stack share the same construction logic,
    /// eliminating the previous code duplication.
    fn add_turn_layer(
        &self,
        pipeline: TurnPipeline,
        name: &str,
        tracing_globally_enabled: bool,
    ) -> TurnPipeline {
        match name {
            "tracing" => {
                // Respects global tracing.enabled — no-op if globally disabled
                if tracing_globally_enabled {
                    if let Some(ref mgr) = self.tracing_manager {
                        return pipeline.with(Box::new(TracingTurnMiddleware::new(
                            Arc::clone(mgr),
                            self.session.id.clone(),
                            self.agent_name.clone(),
                            self.llm.model_name().to_string(),
                            self.llm.provider_name().to_string(),
                        )));
                    }
                }
                pipeline
            }
            // Accept both old name "logging" and new name "request_logging"
            "request_logging" | "logging" => {
                pipeline.with(Box::new(LoggingTurnMiddleware::new(
                    self.session.id.clone(),
                    self.llm.provider_name().to_string(),
                    self.llm.model_name().to_string(),
                )))
            }
            "memory" => {
                pipeline.with(Box::new(MemoryTurnMiddleware::new(
                    self.session.shared_memory(),
                    Arc::clone(&self.llm),
                )))
            }
            "cost" => {
                pipeline.with(Box::new(CostTurnMiddleware::new(
                    Arc::clone(&self.session_cost),
                )))
            }
            "metrics" => {
                pipeline.with(Box::new(MetricsTurnMiddleware::new()))
            }
            other => {
                tracing::warn!(
                    middleware = other,
                    "Unknown turn middleware in config, skipping"
                );
                pipeline
            }
        }
    }
}

// CoreTurnHandler and ToolRouterExecutor have been extracted to
// `agent/core_handler.rs` for better separation of concerns.

// ── AgentMetadata ──

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
    fn session_cost(&self) -> Option<&SharedSessionCost> {
        Some(&self.session_cost)
    }
}

// ── AgentMode — pipeline-driven ──

#[async_trait]
impl AgentMode for ChatAgent {
    /// Send a user message and get the response.
    ///
    /// The entire turn is processed through the middleware pipeline:
    /// `Tracing → Logging → Memory → Core(LLM + tool loop)`
    ///
    /// All cross-cutting concerns are handled by middleware.
    /// This method only constructs the request and dispatches to the pipeline.
    async fn chat(
        &mut self,
        user_input: &str,
        on_tool_event: Option<&ToolEventCallback>,
    ) -> Result<ChatResponse> {
        let pipeline = self.build_turn_pipeline(on_tool_event.cloned());

        let request = TurnRequest {
            user_input,
            messages: vec![], // MemoryMiddleware fills this
            extensions: Extensions::new(),
        };

        let response = pipeline.execute(request).await?;
        Ok(response.chat_response)
    }

    fn attach_mcp(&mut self, mcp: McpManager) {
        self.router_mut_exclusive().attach_mcp(mcp);
        self.reset_with_updated_prompt();
    }

    fn new_session(&mut self) {
        self.reset_with_updated_prompt();
        tracing::info!(
            session_id = %self.session.id,
            "New session created with migrated persistent memory"
        );
    }

    fn set_subagent_event_callback(&self, callback: Option<ToolEventCallback>) {
        self.tool_router.set_subagent_event_callback(callback);
    }

    async fn shutdown(&mut self) -> Result<()> {
        // Persist memory and session ID, but do NOT early-return on failure.
        // Router shutdown and tracing flush must always run, regardless of
        // whether persist succeeds, to avoid MCP child process leaks.
        let mut persist_error: Option<anyhow::Error> = None;

        if let Some(ref workspace) = self.workspace {
            tracing::info!("Persisting memory to workspace...");
            let shared = self.session.shared_memory();
            let mem = shared.lock().await;
            match mem.persist(workspace) {
                Ok(()) => {
                    if let Err(e) = std::fs::write(workspace.last_session_id_path(), &self.session.id) {
                        tracing::warn!(error = %e, "Failed to save last session ID");
                    }
                    tracing::info!(session_id = %self.session.id, "Memory persisted successfully");
                }
                Err(e) => {
                    tracing::error!(error = %e, "Failed to persist memory to workspace");
                    persist_error = Some(e);
                }
            }
        }

        // Always run cleanup, even if persist failed.
        self.router_mut_exclusive().shutdown().await;
        if let Some(ref mgr) = self.tracing_manager {
            mgr.flush().await;
        }

        // Propagate the persist error only after all cleanup is done.
        if let Some(e) = persist_error {
            return Err(e);
        }

        Ok(())
    }

    async fn persist_memory(&self) {
        if let Some(ref workspace) = self.workspace {
            let shared = self.session.shared_memory();
            let mem = shared.lock().await;
            if let Err(e) = mem.persist(workspace) {
                tracing::warn!(error = %e, "Failed to persist memory after turn");
            }
        }
    }

    async fn compact(&mut self, instruction: Option<&str>) -> anyhow::Result<String> {
        let shared = self.session.shared_memory();
        let mut mem = shared.lock().await;
        let result = mem.compact(&*self.llm, instruction).await?;

        // Persist after compact to save the compressed state
        if let Some(ref workspace) = self.workspace {
            if let Err(e) = mem.persist(workspace) {
                tracing::warn!(error = %e, "Failed to persist memory after compact");
            }
        }

        Ok(result)
    }
}
