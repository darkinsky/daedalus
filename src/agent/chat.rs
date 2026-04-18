use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::config::AgentConfig;
use crate::llm::{ChatResponse, LlmApi, LlmConfig, ToolResponse};
use crate::tools::{ToolEventCallback, ToolInfo};
use crate::mcp::McpManager;
use crate::memory::{MemoryFactory, SlidingWindowFactory};
use crate::middleware::{Extensions, TurnNext, TurnRequest, TurnResponse};
use crate::middleware::pipeline::{TurnPipeline, ToolPipeline};
use crate::middleware::builtin::tracing::{TracingTurnMiddleware, TracingToolMiddleware};
use crate::middleware::builtin::logging::{LoggingTurnMiddleware, LoggingToolMiddleware};
use crate::middleware::builtin::memory::MemoryTurnMiddleware;
use crate::middleware::builtin::permission::{PermissionToolMiddleware, PermissionPolicy};
use crate::middleware::config::MiddlewareConfig;
use crate::prompt::PromptBuilder;
use crate::skill::SkillInfo;
use crate::subagent::SubagentInfo;
use crate::agent_tracing;
use crate::workspace::Workspace;

use super::Session;

use super::{AgentMetadata, AgentMode};
use super::tool_loop::{run_tool_loop, LoopConfig, LoopOutcome, LoopResult, ToolExecutor};
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
    /// Workspace for file I/O.
    workspace: Option<Workspace>,
    /// Maximum tool-calling rounds per user message.
    max_tool_rounds: usize,
    /// Tracing manager for observability.
    tracing_manager: Option<Arc<agent_tracing::TracingManager>>,
    /// Middleware pipeline configuration (loaded from YAML).
    middleware_config: MiddlewareConfig,
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
            &[],
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
            workspace: None,
            max_tool_rounds: DEFAULT_MAX_TOOL_ROUNDS,
            tracing_manager: None,
            middleware_config: MiddlewareConfig::default(),
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
        agent
    }

    // ── Extension loading ──

    /// Get a mutable reference to the tool router.
    ///
    /// This is only called during setup (load_skills, load_subagents, etc.)
    /// before the router is shared with any pipeline. At that point there's
    /// only one Arc reference, so `get_mut` always succeeds.
    fn router_mut(&mut self) -> &mut ToolRouter {
        Arc::get_mut(&mut self.tool_router)
            .expect("ToolRouter should have no other references during setup")
    }

    pub fn load_skills(&mut self, dir: &Path) -> Result<usize> {
        let mut registry = crate::skill::SkillRegistry::new();
        let count = registry.load_from_dir(dir)?;
        if count > 0 {
            self.router_mut().install_skills(std::sync::Arc::new(registry));
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
            self.router_mut()
                .install_subagents(std::sync::Arc::new(registry), parent_llm_config);
            self.reset_with_updated_prompt();
        }
        Ok(count)
    }

    pub fn set_tool_filter(&mut self, filter: Option<super::tool_router::ToolFilter>) {
        self.router_mut().set_tool_filter(filter);
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

    // ── Prompt construction ──

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
        Session::new(memory)
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
        let core = Box::new(CoreTurnHandler {
            llm: Arc::clone(&self.llm),
            tool_router: Arc::clone(&self.tool_router),
            max_tool_rounds: self.max_tool_rounds,
            on_tool_event,
            tool_middleware_config: self.middleware_config.tool.iter()
                .map(|e| (e.name.clone(), e.enabled, e.config.clone()))
                .collect(),
        });

        let mut pipeline = TurnPipeline::new(core);

        // Determine if tracing is globally enabled (from TracingManager)
        let tracing_globally_enabled = self.tracing_manager
            .as_ref()
            .map(|mgr| mgr.is_enabled())
            .unwrap_or(false);

        if self.middleware_config.turn.is_empty() {
            // ── Default stack (innermost first) ──
            // memory → request_logging → tracing
            pipeline = pipeline.with(Box::new(MemoryTurnMiddleware::new(
                self.session.shared_memory(),
                Arc::clone(&self.llm),
            )));
            pipeline = pipeline.with(Box::new(LoggingTurnMiddleware::new(
                self.session.id.clone(),
                self.llm.provider_name().to_string(),
                self.llm.model_name().to_string(),
            )));
            if tracing_globally_enabled {
                if let Some(ref mgr) = self.tracing_manager {
                    pipeline = pipeline.with(Box::new(TracingTurnMiddleware::new(
                        Arc::clone(mgr),
                        self.session.id.clone(),
                        self.agent_name.clone(),
                        self.llm.model_name().to_string(),
                        self.llm.provider_name().to_string(),
                    )));
                }
            }
        } else {
            // ── Config-driven stack (innermost first, no reversal needed) ──
            // Safety: ensure memory is always present as innermost layer
            let has_memory = self.middleware_config.turn.iter()
                .any(|e| e.name == "memory" && e.enabled);
            if !has_memory {
                tracing::warn!("Turn middleware config missing 'memory' — injecting as innermost layer");
                pipeline = pipeline.with(Box::new(MemoryTurnMiddleware::new(
                    self.session.shared_memory(),
                    Arc::clone(&self.llm),
                )));
            }

            for entry in &self.middleware_config.turn {
                if !entry.enabled {
                    tracing::debug!(middleware = %entry.name, "Turn middleware disabled by config");
                    continue;
                }

                match entry.name.as_str() {
                    "tracing" => {
                        // Respects global tracing.enabled — no-op if globally disabled
                        if tracing_globally_enabled {
                            if let Some(ref mgr) = self.tracing_manager {
                                pipeline = pipeline.with(Box::new(TracingTurnMiddleware::new(
                                    Arc::clone(mgr),
                                    self.session.id.clone(),
                                    self.agent_name.clone(),
                                    self.llm.model_name().to_string(),
                                    self.llm.provider_name().to_string(),
                                )));
                            }
                        }
                    }
                    // Accept both old name "logging" and new name "request_logging"
                    "request_logging" | "logging" => {
                        pipeline = pipeline.with(Box::new(LoggingTurnMiddleware::new(
                            self.session.id.clone(),
                            self.llm.provider_name().to_string(),
                            self.llm.model_name().to_string(),
                        )));
                    }
                    "memory" => {
                        pipeline = pipeline.with(Box::new(MemoryTurnMiddleware::new(
                            self.session.shared_memory(),
                            Arc::clone(&self.llm),
                        )));
                    }
                    other => {
                        tracing::warn!(
                            middleware = other,
                            "Unknown turn middleware in config, skipping"
                        );
                    }
                }
            }
        }

        pipeline
    }
}

/// The core turn handler — does the actual LLM call + tool loop.
///
/// This is the innermost layer of the turn pipeline. All cross-cutting
/// concerns (tracing spans, logging, memory) are handled by outer
/// middleware layers. Tool-level concerns (tracing, permission, logging)
/// are handled by the tool pipeline.
struct CoreTurnHandler {
    llm: Arc<dyn LlmApi>,
    tool_router: Arc<ToolRouter>,
    max_tool_rounds: usize,
    on_tool_event: Option<ToolEventCallback>,
    /// Tool middleware config for building per-turn tool pipelines.
    tool_middleware_config: Vec<(String, bool, serde_json::Value)>,
}

#[async_trait]
impl TurnNext for CoreTurnHandler {
    async fn run(&self, request: TurnRequest<'_>) -> Result<TurnResponse> {
        let trace_ctx = request
            .extensions
            .get::<Arc<agent_tracing::TraceContext>>()
            .cloned();

        if self.tool_router.has_tools() && self.llm.supports_tools() {
            // ── Tool-calling path ──
            let tools = self.tool_router.build_tool_definitions();
            let executor: Arc<dyn ToolExecutor> = Arc::new(ToolRouterExecutor {
                router: Arc::clone(&self.tool_router),
            });
            let cfg = LoopConfig {
                max_tool_rounds: self.max_tool_rounds,
                agent_label: "Lead agent".to_string(),
                track_reasoning: true,
            };

            let tracing_hook = trace_ctx.map(|ctx| {
                agent_tracing::TracingHook::new(ctx)
            });

            if let Some(ref hook) = tracing_hook {
                self.tool_router
                    .set_shared_tracing_hook(Some(hook.context_arc()));
            }

            // Build tool pipeline from config
            let tool_pipeline = self.build_tool_pipeline(Arc::clone(&executor));

            let LoopResult {
                outcome,
                usage,
                tool_history,
            } = run_tool_loop(
                &*self.llm,
                &*executor,
                &request.messages,
                &tools,
                self.on_tool_event.as_ref(),
                &cfg,
                None,
                tracing_hook.as_ref(),
                Some(&tool_pipeline),
            )
            .await?;

            self.tool_router.set_shared_tracing_hook(None);

            match outcome {
                LoopOutcome::Final { content, reasoning } => Ok(TurnResponse {
                    chat_response: ChatResponse {
                        content,
                        reasoning_content: reasoning,
                        usage: Some(usage.clone()),
                        tool_calls: vec![],
                    },
                    tool_history,
                    usage,
                    extensions: Extensions::new(),
                }),
                LoopOutcome::DuplicateStop { message } => anyhow::bail!("{}", message),
                LoopOutcome::MaxRoundsExceeded => {
                    anyhow::bail!(
                        "Exceeded maximum tool-calling rounds ({})",
                        self.max_tool_rounds
                    )
                }
            }
        } else {
            // ── Simple chat path (no tools) ──
            let mut llm_guard =
                if let Some(ctx) = request.extensions.get::<Arc<agent_tracing::TraceContext>>() {
                    if ctx.is_enabled() {
                        Some(
                            ctx.start_llm_call(
                                self.llm.model_name(),
                                self.llm.provider_name(),
                                &request.messages,
                            )
                            .await,
                        )
                    } else {
                        None
                    }
                } else {
                    None
                };

            let llm_result = self.llm.chat(&request.messages, None).await;

            match llm_result {
                Err(e) => {
                    if let Some(guard) = llm_guard {
                        guard.finish_error(e.to_string()).await;
                    }
                    Err(e)
                }
                Ok(response) => {
                    if let Some(ref mut guard) = llm_guard {
                        guard.set_llm_response(&response);
                    }
                    if let Some(guard) = llm_guard {
                        guard.finish_ok().await;
                    }
                    let usage = response.usage.clone().unwrap_or_default();
                    Ok(TurnResponse {
                        chat_response: response,
                        tool_history: vec![],
                        usage,
                        extensions: Extensions::new(),
                    })
                }
            }
        }
    }
}

impl CoreTurnHandler {
    /// Build the tool middleware pipeline from config.
    ///
    /// Config order is **innermost first** (matching `.with()` semantics).
    /// Tracing middleware respects the global `tracing.enabled` flag via
    /// the `TraceContext` presence in extensions (no-op if absent).
    fn build_tool_pipeline(&self, executor: Arc<dyn ToolExecutor>) -> ToolPipeline {
        use super::tool_loop::ToolExecutorCore;

        let core = Box::new(ToolExecutorCore { executor });
        let mut pipeline = ToolPipeline::new(core);

        if self.tool_middleware_config.is_empty() {
            // ── Default stack (innermost first) ──
            // tool_logging → permission → tracing
            pipeline = pipeline.with(Box::new(LoggingToolMiddleware));
            pipeline = pipeline.with(Box::new(PermissionToolMiddleware::new(PermissionPolicy::Allow)));
            pipeline = pipeline.with(Box::new(TracingToolMiddleware));
        } else {
            // ── Config-driven (innermost first, no reversal) ──
            for (name, enabled, config) in &self.tool_middleware_config {
                if !enabled {
                    continue;
                }
                match name.as_str() {
                    "tracing" => {
                        pipeline = pipeline.with(Box::new(TracingToolMiddleware));
                    }
                    "permission" => {
                        let policy = config.get("policy")
                            .and_then(|v| v.as_str())
                            .unwrap_or("allow");
                        let tool_list: Vec<String> = config.get("tools")
                            .and_then(|v| v.as_array())
                            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                            .unwrap_or_default();

                        let permission_policy = match policy {
                            "deny_list" => PermissionPolicy::DenyList(tool_list),
                            "allow_list" => PermissionPolicy::AllowList(tool_list),
                            "allow" => PermissionPolicy::Allow,
                            unknown => {
                                tracing::warn!(
                                    policy = unknown,
                                    "Unknown permission policy in middleware config, \
                                     falling back to 'allow'. Valid values: allow, deny_list, allow_list"
                                );
                                PermissionPolicy::Allow
                            }
                        };
                        pipeline = pipeline.with(Box::new(PermissionToolMiddleware::new(permission_policy)));
                    }
                    // Accept both old name "logging" and new name "tool_logging"
                    "tool_logging" | "logging" => {
                        pipeline = pipeline.with(Box::new(LoggingToolMiddleware));
                    }
                    other => {
                        tracing::warn!(
                            middleware = other,
                            "Unknown tool middleware in config, skipping"
                        );
                    }
                }
            }
        }

        pipeline
    }
}

/// Adapter: ToolRouter → ToolExecutor.
///
/// Holds `Arc<ToolRouter>` so it can be shared across pipeline layers
/// and satisfy the `'static` lifetime requirement.
struct ToolRouterExecutor {
    router: Arc<ToolRouter>,
}

#[async_trait]
impl ToolExecutor for ToolRouterExecutor {
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
        self.router_mut().attach_mcp(mcp);
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
        if let Some(ref workspace) = self.workspace {
            tracing::info!("Persisting memory to workspace...");
            let shared = self.session.shared_memory();
            let mem = shared.lock().await;
            if let Err(e) = mem.persist(workspace) {
                tracing::error!(error = %e, "Failed to persist memory to workspace");
                return Err(e);
            }
            if let Err(e) = std::fs::write(workspace.last_session_id_path(), &self.session.id) {
                tracing::warn!(error = %e, "Failed to save last session ID");
            }
            tracing::info!(session_id = %self.session.id, "Memory persisted successfully");
        }
        self.router_mut().shutdown().await;
        if let Some(ref mgr) = self.tracing_manager {
            mgr.flush().await;
        }
        Ok(())
    }
}
