use anyhow::Result;

use crate::agent::tool_loop::{run_tool_loop, LoopConfig, LoopContext, LoopOutcome, LoopResult, ToolExecutor};
use crate::agent_tracing::TracingHook;
use crate::tools::ToolEventCallback;
use crate::llm::{self, LlmApi, LlmConfig, ToolCall, ToolResponse};
use crate::tools::BuiltinToolRegistry;

use super::prompt;
use super::{IsolationMode, SubagentDefinition, SubagentResult};
#[cfg(feature = "team")]
use super::TeamTask;

/// Maximum tool-calling rounds for subagents (default, can be overridden per-agent).
const DEFAULT_MAX_TOOL_ROUNDS: usize = 100;

/// Tool names that are never available to subagents (prevents recursion and misuse).
const EXCLUDED_TOOLS: &[&str] = &["spawn_subagent", "spawn_team", "use_skill"];

/// Executes subagent tasks in isolated contexts.
///
/// The runner creates a fresh `ChatAgent`-like execution environment for each
/// subagent invocation:
/// - Independent LLM provider (possibly a different model)
/// - No memory system (stateless — only system prompt + task + tool history)
/// - Filtered tool set (whitelist/blacklist from the subagent definition)
/// - The subagent's own system prompt (NOT the main agent's prompt)
///
/// Subagents cannot spawn other subagents (no `spawn_subagent` tool is
/// registered in the subagent's tool router), preventing infinite recursion.
pub struct SubagentRunner {
    /// The parent agent's LLM config (used as base for creating sub-providers).
    parent_llm_config: LlmConfig,
}

impl SubagentRunner {
    /// Create a new runner with the parent agent's LLM configuration.
    pub fn new(parent_llm_config: LlmConfig) -> Self {
        Self { parent_llm_config }
    }

    /// Execute a subagent task and return the result.
    ///
    /// This creates a completely isolated execution environment:
    /// 1. Creates an LLM provider (same or different model based on definition)
    /// 2. Builds a filtered tool set (no `spawn_subagent` to prevent recursion)
    /// 3. Runs the tool-calling loop until completion or max rounds
    /// 4. Returns the final response content
    ///
    /// An optional `on_tool_event` callback can be provided to receive
    /// real-time notifications about tool execution progress within the subagent.
    pub async fn run(
        &self,
        definition: &SubagentDefinition,
        task: &str,
        on_tool_event: Option<&ToolEventCallback>,
        tracing_hook: Option<&TracingHook>,
    ) -> Result<SubagentResult> {
        tracing::info!(
            agent = %definition.name,
            model = ?definition.model,
            task_len = task.len(),
            isolation = %definition.isolation,
            "Starting subagent execution"
        );

        // Run onStart lifecycle hook if configured
        if let Some(ref on_start_cmd) = definition.on_start {
            super::isolation::run_lifecycle_hook(
                "onStart", &definition.name, on_start_cmd, task,
            ).await?;
        }

        // Set up worktree isolation if configured
        let _worktree_guard = if definition.isolation == IsolationMode::Worktree {
            Some(super::isolation::setup_worktree(&definition.name).await?)
        } else {
            None
        };

        // 1. Create LLM provider (possibly with a different model)
        let llm = self.create_provider(definition)?;

        // 2. Build the filtered tool set
        let filtered_tools = self.build_filtered_tools(definition);
        let tools = filtered_tools.build_tool_definitions();
        let has_tools = !tools.is_empty() && llm.supports_tools();

        // 3. Build the effective system prompt with tool guidance,
        //    environment context, and safety constraints.
        //    This aligns with Claude Code's SubAgent prompt construction:
        //    base prompt + tool inventory + usage strategy + env + constraints.
        let tool_infos = filtered_tools.tool_infos();
        let effective_prompt = prompt::build_effective_prompt(
            &definition.system_prompt,
            &tool_infos,
            has_tools,
        );

        // 4. Build messages: enhanced system prompt + user task.
        //
        //    Prompt caching strategy: mark the **user task** (not the system
        //    prompt) with CacheControl::Ephemeral. This tells the API to set
        //    a cache breakpoint at the end of the user message, so the entire
        //    prefix `system + user` is cached across the subagent's 50-100
        //    tool-calling rounds.
        //
        //    Why the user task and not the system prompt?
        //    - Anthropic/Claude cache by **prefix match** up to the last
        //      cache_control marker.
        //    - Marking only system means only ~4K tokens are cached.
        //    - Marking user means system + user (~5K tokens) are cached,
        //      AND the tool history prefix (old rounds that don't change
        //      between iterations) gets prefix-matched automatically.
        //
        //    The tool history is appended after these messages in
        //    `build_request_body()`. Since old truncated rounds are stable
        //    (their content doesn't change once truncated), the API's
        //    automatic prefix matching will cache them too — potentially
        //    caching 60-80% of the prompt instead of just 5%.
        let messages = vec![
            crate::llm::ChatMessage::system(&effective_prompt),
            crate::llm::ChatMessage::user(task)
                .with_cache_control(crate::llm::CacheControl::Ephemeral),
        ];

        // 5. Run the tool-calling loop
        let max_tool_rounds = definition.max_turns.unwrap_or(DEFAULT_MAX_TOOL_ROUNDS);

        let result = if has_tools {
            self.run_with_tools(
                definition, &*llm, &filtered_tools, &messages, &tools, max_tool_rounds, on_tool_event, tracing_hook,
            ).await
        } else {
            self.run_without_tools(definition, &*llm, &messages, tracing_hook).await
        };

        // Run onComplete lifecycle hook if configured
        if let Some(ref on_complete_cmd) = definition.on_complete {
            if let Ok(ref r) = result {
                let _ = super::isolation::run_lifecycle_hook(
                    "onComplete", &definition.name, on_complete_cmd, &r.content,
                ).await;
            }
        }

        result
    }

    /// Create an LLM provider for the subagent.
    ///
    /// If the subagent specifies a model, creates a new provider with that model.
    /// Otherwise, clones the parent's config (inheriting the same model).
    fn create_provider(
        &self,
        definition: &SubagentDefinition,
    ) -> Result<Box<dyn LlmApi>> {
        let mut config = self.parent_llm_config.clone();

        if let Some(ref model) = definition.model {
            // Map shorthand names to full model IDs.
            // Returns None for "inherit" — leave config.model unchanged (use parent).
            if let Some(resolved) = Self::resolve_model_name(model) {
                config.model = resolved;
                tracing::info!(
                    agent = %definition.name,
                    model = %config.model,
                    "Subagent using custom model"
                );
            } else {
                tracing::debug!(
                    agent = %definition.name,
                    model = %config.model,
                    "Subagent inheriting parent model"
                );
            }
        }

        llm::create_provider(config)
    }

    /// Resolve shorthand model names to full model identifiers.
    ///
    /// Supports Claude Code-style shorthands: "haiku", "sonnet", "opus".
    /// Full model IDs are passed through unchanged.
    ///
    /// "inherit" means use the parent's model (caller is responsible for not
    /// overriding `config.model` in this case).
    fn resolve_model_name(name: &str) -> Option<String> {
        match name.to_lowercase().as_str() {
            "haiku" => Some("claude-3-5-haiku-20241022".to_string()),
            "sonnet" => Some("claude-sonnet-4-20250514".to_string()),
            "opus" => Some("claude-opus-4-20250514".to_string()),
            "inherit" => None, // Signal: keep parent config unchanged
            _ => Some(name.to_string()), // Full model ID passed through
        }
    }

    /// Build a filtered `BuiltinToolRegistry` based on the subagent's tool config.
    ///
    /// - If `tools` (whitelist) is set: only include those tools
    /// - If `disallowed_tools` (blacklist) is set: include all except those
    /// - If neither is set: include all built-in tools
    ///
    /// Tools in `EXCLUDED_TOOLS` are never included (prevents recursion).
    fn build_filtered_tools(&self, definition: &SubagentDefinition) -> BuiltinToolRegistry {
        let full_registry = BuiltinToolRegistry::new();

        // Build a filter predicate based on the subagent's tool configuration.
        // This unifies the whitelist/blacklist/no-filter branches into a single loop.
        let filter: Box<dyn Fn(&str) -> bool> = match (&definition.tools, &definition.disallowed_tools) {
            (Some(whitelist), _) => {
                let whitelist = whitelist.clone();
                Box::new(move |name: &str| whitelist.iter().any(|w| w == name))
            }
            (_, Some(blacklist)) => {
                let blacklist = blacklist.clone();
                Box::new(move |name: &str| !blacklist.iter().any(|b| b == name))
            }
            _ => Box::new(|_: &str| true),
        };

        let mut filtered = BuiltinToolRegistry::new_empty();
        for tool in full_registry.into_tools() {
            let name = tool.name().to_string();
            if filter(&name) && !EXCLUDED_TOOLS.contains(&name.as_str()) {
                filtered.register_tool(tool);
            }
        }

        tracing::info!(
            agent = %definition.name,
            tools = filtered.tool_count(),
            whitelist = ?definition.tools,
            blacklist = ?definition.disallowed_tools,
            "Subagent tool set built"
        );

        filtered
    }

    /// Run the subagent without tools (simple chat).
    async fn run_without_tools(
        &self,
        definition: &SubagentDefinition,
        llm: &dyn LlmApi,
        messages: &[crate::llm::ChatMessage],
        tracing_hook: Option<&TracingHook>,
    ) -> Result<SubagentResult> {
        // Start LLM call tracing span for subagent
        let mut llm_span = if let Some(hook) = tracing_hook {
            hook.on_llm_call_start(
                llm.model_name(),
                llm.provider_name(),
                messages,
                &[], // No tools available in simple chat mode
            ).await
        } else {
            None
        };

        let response = llm.chat(messages, None).await?;

        // Finish LLM call tracing span
        if let Some(ref mut span) = llm_span {
            span.set_llm_response(&response);
        }
        if let Some(span) = llm_span {
            span.finish_ok().await;
        }

        tracing::info!(
            agent = %definition.name,
            content_len = response.content.len(),
            "Subagent completed (no tools)"
        );

        Ok(SubagentResult {
            agent_name: definition.name.clone(),
            content: response.content,
            usage: response.usage,
            tool_rounds: 0,
        })
    }

    /// Run the subagent with the tool-calling loop.
    ///
    /// Thin wrapper over `tool_loop::run_tool_loop`: builds the executor
    /// adapter, picks loop config, and translates the generic
    /// `LoopOutcome` into the subagent's "return a partial result"
    /// semantics (neither a duplicate-stop nor a max-rounds condition
    /// should propagate as an `Err` — they're normal subagent outcomes).
    async fn run_with_tools(
        &self,
        definition: &SubagentDefinition,
        llm: &dyn LlmApi,
        builtin: &BuiltinToolRegistry,
        messages: &[crate::llm::ChatMessage],
        tools: &[serde_json::Value],
        max_tool_rounds: usize,
        on_tool_event: Option<&ToolEventCallback>,
        tracing_hook: Option<&TracingHook>,
    ) -> Result<SubagentResult> {
        let executor = SubagentExecutor {
            builtin,
            agent_name: &definition.name,
        };
        let cfg = LoopConfig {
            max_tool_rounds,
            agent_label: format!("Subagent '{}'", definition.name),
            // Subagents don't surface reasoning content upstream, so there's
            // no point paying the book-keeping cost.
            track_reasoning: false,
            // Subagents share the same model as the parent — scale truncation
            // to the model's actual context window size.
            truncation: Some(crate::agent::tool_loop::TruncationConfig::for_context_window(
                self.parent_llm_config.resolved_context_window(),
            )),
            // Subagents have short lifespans and limited tool sets, so context
            // pressure awareness is disabled (they rarely fill the window).
            context_window_tokens: None,
            context_soft_limit_ratio: 0.7,
            context_hard_limit_ratio: 0.9,
        };

        let loop_ctx = LoopContext {
            executor: &executor,
            messages,
            tools,
            on_tool_event,
            on_llm_response: None,
            tracing_hook, // Pass tracing hook to subagent's tool loop
            tool_pipeline: None, // No tool pipeline for subagents (uses direct execution)
            shared_notes: None, // TODO: wire up take_note SharedNotes when the tool is in the filtered set
        };

        let LoopResult { outcome, usage, tool_history } = run_tool_loop(
            llm,
            &cfg,
            &loop_ctx,
        ).await?;

        let tool_rounds = tool_history.len();
        let content = match outcome {
            LoopOutcome::Final { content, .. } => {
                tracing::info!(
                    agent = %definition.name,
                    rounds = tool_rounds,
                    content_len = content.len(),
                    "Subagent completed with tools"
                );
                content
            }
            LoopOutcome::DuplicateStop { message } => {
                format!("[Subagent '{}' stopped: {}]", definition.name, message)
            }
            LoopOutcome::MaxRoundsExceeded => {
                // ── Graceful degradation (inspired by Claude Code) ──
                //
                // Instead of returning a useless error string that discards
                // all the subagent's work, make one final LLM call WITHOUT
                // tools to force a summary of everything found so far.
                // This is the single most impactful optimization: it prevents
                // the main agent from re-doing the entire task from scratch.
                tracing::warn!(
                    agent = %definition.name,
                    rounds = tool_rounds,
                    max_rounds = max_tool_rounds,
                    "Subagent reached max rounds, attempting final summary"
                );

                self.force_final_summary(
                    definition, llm, messages, &tool_history, max_tool_rounds,
                ).await
            }
            LoopOutcome::ContextBudgetExceeded { content, .. } => {
                tracing::warn!(
                    agent = %definition.name,
                    rounds = tool_rounds,
                    "Subagent stopped due to context budget exceeded"
                );
                content
            }
        };

        Ok(SubagentResult {
            agent_name: definition.name.clone(),
            content,
            usage: Some(usage),
            tool_rounds,
        })
    }

    /// Force a final summary when `MaxRoundsExceeded`.
    ///
    /// Makes one last LLM call **without tools** (so the model is forced to
    /// produce text), injecting a summary of the tool history as context.
    /// This is inspired by Claude Code's `max_turns_reached` mechanism:
    /// instead of discarding all work, give the LLM one chance to distill
    /// its findings into a usable response.
    ///
    /// If this final call fails for any reason, falls back to a structured
    /// summary of the tool history itself (tool names + truncated results).
    async fn force_final_summary(
        &self,
        definition: &SubagentDefinition,
        llm: &dyn LlmApi,
        original_messages: &[crate::llm::ChatMessage],
        tool_history: &[crate::llm::ToolRound],
        max_tool_rounds: usize,
    ) -> String {
        use crate::llm::ChatMessage;

        // Build a condensed view of what the subagent accomplished.
        let mut work_summary = String::with_capacity(4096);
        for (i, round) in tool_history.iter().enumerate() {
            work_summary.push_str(&format!("Round {}:\n", i + 1));
            for (call, resp) in round.calls.iter().zip(round.responses.iter()) {
                work_summary.push_str(&format!("  Tool: {} → ", call.function_name));
                // Keep first ~1500 bytes of each result for the summary.
                // With 100 rounds × ~2 calls/round, this can produce up to ~300KB,
                // well within a 256K-token model's capacity for the final summary call.
                // Use truncate_at_char_boundary to avoid splitting multi-byte
                // characters (emoji, CJK, etc.).
                let preview = if resp.content.len() > 1500 {
                    let safe = crate::tools::truncate_at_char_boundary(&resp.content, 1500);
                    format!("{}...", safe)
                } else {
                    resp.content.clone()
                };
                work_summary.push_str(&preview);
                work_summary.push('\n');
            }
        }

        // Build the forcing prompt. Re-use the original system message so
        // the LLM retains its role identity.
        let system = original_messages.first().cloned()
            .unwrap_or_else(|| ChatMessage::system("You are a helpful assistant."));

        let forcing_prompt = format!(
            "You have reached the maximum number of tool-calling rounds ({max_tool_rounds}). \
             Below is a summary of all work completed so far:\n\n\
             {work_summary}\n\n\
             Based on EVERYTHING you have reviewed and found, output your COMPLETE \
             findings NOW. Do not request any more tools. Provide a thorough, \
             well-organized summary of all insights, issues, and observations.",
        );

        let messages = vec![system, ChatMessage::user(&forcing_prompt)];

        match llm.chat(&messages, None).await {
            Ok(response) if !response.content.trim().is_empty() => {
                tracing::info!(
                    agent = %definition.name,
                    summary_len = response.content.len(),
                    "Subagent produced graceful summary after max rounds"
                );
                response.content
            }
            Ok(_) | Err(_) => {
                // Fallback: return the raw work summary so the main agent
                // has *something* to work with rather than nothing.
                tracing::warn!(
                    agent = %definition.name,
                    "Final summary call failed, returning raw work summary"
                );
                format!(
                    "[Subagent '{}' reached max rounds ({}). Partial work summary:]\n\n{}",
                    definition.name, max_tool_rounds, work_summary
                )
            }
        }
    }

    /// Execute multiple subagent tasks in parallel and return all results.
    ///
    /// This is the core of the "Agent Teams" feature. Each task is assigned
    /// to a named subagent and executed concurrently.
    ///
    /// Only compiled in when the `team` feature is enabled.
    #[cfg(feature = "team")]
    pub async fn run_team(
        &self,
        tasks: &[TeamTask],
        registry: &super::SubagentRegistry,
        on_tool_event: Option<&ToolEventCallback>,
    ) -> Vec<Result<SubagentResult>> {
        tracing::info!(
            team_size = tasks.len(),
            agents = ?tasks.iter().map(|t| t.agent_name.as_str()).collect::<Vec<_>>(),
            "Starting parallel team execution"
        );

        let futures: Vec<_> = tasks
            .iter()
            .map(|task| {
                let definition = registry.get(&task.agent_name).cloned();
                let task_str = task.task.clone();
                async move {
                    let def = definition.ok_or_else(|| {
                        anyhow::anyhow!("Subagent '{}' not found", task.agent_name)
                    })?;
                    self.run(&def, &task_str, on_tool_event, None).await
                }
            })
            .collect();

        futures::future::join_all(futures).await
    }
}

/// Adapter that lets a filtered `BuiltinToolRegistry` satisfy
/// `tool_loop::ToolExecutor`.
///
/// Kept private to this module so the coupling between the generic loop
/// and subagent-specific routing stays local. The `'a` lifetimes bind
/// the adapter to the caller's stack; it does not outlive the enclosing
/// `run_with_tools` call.
struct SubagentExecutor<'a> {
    builtin: &'a BuiltinToolRegistry,
    agent_name: &'a str,
}

#[async_trait::async_trait]
impl<'a> ToolExecutor for SubagentExecutor<'a> {
    async fn execute(&self, call: &ToolCall) -> ToolResponse {
        // Subagents run against a pre-filtered registry — tools that
        // were excluded at build time are simply not present, so we
        // synthesize an error rather than routing elsewhere.
        if self.builtin.has_tool(&call.function_name) {
            match self
                .builtin
                .call_tool(&call.function_name, call.arguments.clone())
                .await
            {
                Ok(result) => ToolResponse::new(&call.call_id, result),
                Err(e) => ToolResponse::error(&call.call_id, format!("Error: {}", e)),
            }
        } else {
            ToolResponse::error(
                &call.call_id,
                format!(
                    "Tool '{}' not available in this subagent",
                    call.function_name
                ),
            )
        }
    }

    fn source_of(&self, _tool_name: &str) -> String {
        // Tool-event display labels every subagent tool call with its
        // agent name so the REPL can distinguish concurrent team runs.
        format!("subagent:{}", self.agent_name)
    }
}
