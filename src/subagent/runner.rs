use anyhow::Result;

use crate::agent::duplicate_detector::{annotate_responses, DuplicateAction, DuplicateDetector};
use crate::tools::{ToolEvent, ToolEventCallback};
use crate::llm::{self, LlmApi, LlmConfig, TokenUsage, ToolRound};
use crate::tools::BuiltinToolRegistry;

use super::{IsolationMode, SubagentDefinition, SubagentResult, TeamTask};

/// Maximum tool-calling rounds for subagents (default, can be overridden per-agent).
const DEFAULT_MAX_TOOL_ROUNDS: usize = 50;

/// Tool names that are never available to subagents (prevents recursion and misuse).
const EXCLUDED_TOOLS: &[&str] = &["spawn_subagent", "spawn_team", "use_skill"];

/// Executes subagent tasks in isolated contexts.
///
/// The runner creates a fresh `ChatAgent`-like execution environment for each
/// subagent invocation:
/// - Independent LLM provider (possibly a different model)
/// - Independent memory (no shared conversation history)
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
            Some(super::isolation::setup_worktree(&definition.name)?)
        } else {
            None
        };

        // 1. Create LLM provider (possibly with a different model)
        let llm = self.create_provider(definition)?;

        // 2. Build the filtered tool set
        let filtered_tools = self.build_filtered_tools(definition);
        let tools = filtered_tools.build_tool_definitions();
        let has_tools = !tools.is_empty() && llm.supports_tools();

        // 3. Build messages: system prompt + user task
        let messages = vec![
            crate::llm::ChatMessage::system(&definition.system_prompt),
            crate::llm::ChatMessage::user(task),
        ];

        // 4. Run the tool-calling loop
        let max_rounds = definition.max_turns.unwrap_or(DEFAULT_MAX_TOOL_ROUNDS);

        let result = if has_tools {
            self.run_with_tools(
                definition, &*llm, &filtered_tools, &messages, &tools, max_rounds, on_tool_event,
            ).await
        } else {
            self.run_without_tools(definition, &*llm, &messages).await
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
            // Map shorthand names to full model IDs
            config.model = Self::resolve_model_name(model);
            tracing::info!(
                agent = %definition.name,
                model = %config.model,
                "Subagent using custom model"
            );
        }

        llm::create_provider(config)
    }

    /// Resolve shorthand model names to full model identifiers.
    ///
    /// Supports Claude Code-style shorthands: "haiku", "sonnet", "opus".
    /// Full model IDs are passed through unchanged.
    fn resolve_model_name(name: &str) -> String {
        match name.to_lowercase().as_str() {
            "haiku" => "claude-3-5-haiku-20241022".to_string(),
            "sonnet" => "claude-sonnet-4-20250514".to_string(),
            "opus" => "claude-opus-4-20250514".to_string(),
            "inherit" => name.to_string(), // Will use parent's model
            _ => name.to_string(),         // Full model ID
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
    ) -> Result<SubagentResult> {
        let response = llm.chat(messages, None).await?;

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
    async fn run_with_tools(
        &self,
        definition: &SubagentDefinition,
        llm: &dyn LlmApi,
        builtin: &BuiltinToolRegistry,
        messages: &[crate::llm::ChatMessage],
        tools: &[serde_json::Value],
        max_rounds: usize,
        on_tool_event: Option<&ToolEventCallback>,
    ) -> Result<SubagentResult> {
        let mut tool_history: Vec<ToolRound> = Vec::new();
        let mut total_usage = TokenUsage::default();
        let mut duplicate_detector = DuplicateDetector::new();

        for round in 0..max_rounds {
            let response = llm
                .chat_with_tools(messages, tools, &tool_history, None)
                .await?;

            if let Some(ref usage) = response.usage {
                total_usage.accumulate(usage);
            }

            // If no tool calls, we have the final response
            if response.tool_calls.is_empty() {
                tracing::info!(
                    agent = %definition.name,
                    rounds = round,
                    content_len = response.content.len(),
                    "Subagent completed with tools"
                );

                return Ok(SubagentResult {
                    agent_name: definition.name.clone(),
                    content: response.content,
                    usage: Some(total_usage),
                    tool_rounds: tool_history.len(),
                });
            }

            // Execute one round of tool calls and collect results
            let tool_calls = response.tool_calls;
            let mut responses = Self::execute_tool_round_inner(
                definition, builtin, &tool_calls, round + 1, on_tool_event,
            ).await;

            // Duplicate-call guard: warn on streaks of WARN_THRESHOLD,
            // hard-stop on streaks of STOP_THRESHOLD.
            match duplicate_detector.record_round(&tool_calls) {
                DuplicateAction::Warn(warnings) => {
                    for w in &warnings {
                        tracing::warn!(
                            agent = %definition.name,
                            tool = %w.tool_name,
                            streak = w.count,
                            round = round + 1,
                            "Subagent repeated identical tool call"
                        );
                    }
                    annotate_responses(&tool_calls, &mut responses, &warnings);
                }
                DuplicateAction::Stop(w) => {
                    tracing::error!(
                        agent = %definition.name,
                        tool = %w.tool_name,
                        streak = w.count,
                        round = round + 1,
                        "Subagent force-stopped due to duplicate tool calls"
                    );
                    tool_history.push(ToolRound {
                        calls: tool_calls,
                        responses,
                    });
                    return Ok(SubagentResult {
                        agent_name: definition.name.clone(),
                        content: format!(
                            "[Subagent '{}' stopped: {}]",
                            definition.name,
                            w.stop_message()
                        ),
                        usage: Some(total_usage),
                        tool_rounds: tool_history.len(),
                    });
                }
                DuplicateAction::Ok => {}
            }

            tool_history.push(ToolRound {
                calls: tool_calls,
                responses,
            });
        }

        // Exceeded max rounds — return what we have
        tracing::warn!(
            agent = %definition.name,
            max_rounds = max_rounds,
            "Subagent exceeded maximum tool-calling rounds"
        );

        Ok(SubagentResult {
            agent_name: definition.name.clone(),
            content: format!(
                "[Subagent '{}' reached maximum tool-calling rounds ({}). \
                 Last tool history has {} rounds of context.]",
                definition.name,
                max_rounds,
                tool_history.len()
            ),
            usage: Some(total_usage),
            tool_rounds: tool_history.len(),
        })
    }

    /// Execute a single round of tool calls in parallel and emit progress events.
    ///
    /// Splits out the execution so the outer loop can inspect the calls for
    /// duplicate detection before building the final `ToolRound`.
    async fn execute_tool_round_inner(
        definition: &SubagentDefinition,
        builtin: &BuiltinToolRegistry,
        tool_calls: &[crate::llm::ToolCall],
        round_number: usize,
        on_tool_event: Option<&ToolEventCallback>,
    ) -> Vec<crate::llm::ToolResponse> {
        tracing::info!(
            agent = %definition.name,
            round = round_number,
            tool_calls = tool_calls.len(),
            "Subagent requesting tool calls"
        );

        // Emit round start event
        Self::emit_event(on_tool_event, ToolEvent::RoundStart { round: round_number });

        // Emit tool call start events
        for tc in tool_calls {
            Self::emit_event(on_tool_event, ToolEvent::ToolCallStart {
                tool_name: tc.function_name.clone(),
                source: format!("subagent:{}", definition.name),
                arguments: tc.arguments.clone(),
            });
        }

        // Execute all tool calls in parallel
        let futures: Vec<_> = tool_calls
            .iter()
            .map(|tc| async {
                if builtin.has_tool(&tc.function_name) {
                    match builtin.call_tool(&tc.function_name, tc.arguments.clone()).await {
                        Ok(result) => crate::llm::ToolResponse::new(&tc.call_id, result),
                        Err(e) => crate::llm::ToolResponse::error(
                            &tc.call_id,
                            format!("Error: {}", e),
                        ),
                    }
                } else {
                    crate::llm::ToolResponse::error(
                        &tc.call_id,
                        format!("Tool '{}' not available in this subagent", tc.function_name),
                    )
                }
            })
            .collect();

        let responses = futures::future::join_all(futures).await;

        // Emit tool call completion events
        for (tc, resp) in tool_calls.iter().zip(responses.iter()) {
            Self::emit_event(on_tool_event, ToolEvent::ToolCallComplete {
                tool_name: tc.function_name.clone(),
                success: resp.success,
                result_content: resp.content.clone(),
            });
        }
        Self::emit_event(on_tool_event, ToolEvent::RoundComplete {
            tool_count: responses.len(),
        });

        responses
    }

    /// Emit a tool event to the optional callback.
    fn emit_event(callback: Option<&ToolEventCallback>, event: ToolEvent) {
        if let Some(cb) = callback {
            cb(event);
        }
    }

    /// Execute multiple subagent tasks in parallel and return all results.
    ///
    /// This is the core of the "Agent Teams" feature. Each task is assigned
    /// to a named subagent and executed concurrently.
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
                    self.run(&def, &task_str, on_tool_event).await
                }
            })
            .collect();

        futures::future::join_all(futures).await
    }
}
