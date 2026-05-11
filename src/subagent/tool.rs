use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use super::registry::SubagentRegistry;
use super::runner::SubagentRunner;
use super::SubagentResult;
use crate::agent_tracing::{SharedTracingHook, TracingHook};
use crate::tools::{truncate_chars, BuiltinTool, ToolEvent, ToolEventCallback};

/// The tool name used for LLM-routed subagent spawn.
const SUBAGENT_TOOL_NAME: &str = "spawn_subagent";

/// The tool name used for parallel multi-agent team execution.
///
/// Only compiled in when the `team` feature is enabled.
#[cfg(feature = "team")]
const TEAM_TOOL_NAME: &str = "spawn_team";

/// Shared container for a tool event callback that can be set at runtime.
///
/// The callback is stored behind `RwLock` so it can be updated by the REPL
/// before each chat call (to bind to the current spinner) and cleared after.
/// `SubagentTool` / `TeamTool` read it during execution.
///
/// ## Why a named struct instead of a type alias?
///
/// A bare `Arc<RwLock<Option<ToolEventCallback>>>` exposes `read()` /
/// `write()` / `lock().unwrap()` to callers and invites lock-handling bugs.
/// This wrapper offers two small methods (`set`, `read`) that handle the
/// lock-poisoned case once, so every caller gets the right behaviour.
#[derive(Clone, Default)]
pub struct SubagentEventSink {
    inner: Arc<std::sync::RwLock<Option<ToolEventCallback>>>,
}

impl SubagentEventSink {
    /// Create an empty sink (no callback bound).
    pub fn new() -> Self {
        Self { inner: Arc::new(std::sync::RwLock::new(None)) }
    }

    /// Replace the current callback (pass `None` to clear).
    ///
    /// Silently no-ops if the lock is poisoned — losing a callback update
    /// is strictly better than panicking in the middle of a REPL turn.
    pub fn set(&self, callback: Option<ToolEventCallback>) {
        if let Ok(mut guard) = self.inner.write() {
            *guard = callback;
        }
    }

    /// Get the currently bound callback, if any.
    ///
    /// Returns `None` if the lock is poisoned or no callback is set.
    pub fn get(&self) -> Option<ToolEventCallback> {
        self.inner.read().ok().and_then(|guard| guard.clone())
    }
}

// ── Shared infrastructure ──

/// Shared state held by both `SubagentTool` and `TeamTool`.
///
/// Extracted to avoid duplicating the same three `Arc` fields and
/// their accessor patterns across both tool implementations.
struct SubagentToolContext {
    registry: Arc<SubagentRegistry>,
    runner: Arc<SubagentRunner>,
    event_sink: SubagentEventSink,
    tracing_hook: SharedTracingHook,
}

impl SubagentToolContext {
    /// Read the current event callback from the shared sink.
    ///
    /// Returns `None` if the lock is poisoned or no callback is set.
    fn read_callback(&self) -> Option<ToolEventCallback> {
        self.event_sink.get()
    }

    /// Emit a `SubagentStart` event via the current callback (if present).
    fn emit_start(&self, agent_name: &str, task: &str) {
        if let Some(ref cb) = self.read_callback() {
            cb(ToolEvent::SubagentStart {
                agent_name: agent_name.to_string(),
                task_preview: truncate_chars(task, 100),
            });
        }
    }

    /// Emit a `SubagentComplete` event based on the execution result.
    fn emit_complete(&self, agent_name: &str, result: &Result<SubagentResult>, elapsed_ms: u64) {
        if let Some(ref cb) = self.read_callback() {
            match result {
                Ok(r) => {
                    cb(ToolEvent::SubagentComplete {
                        agent_name: agent_name.to_string(),
                        success: true,
                        tool_rounds: r.tool_rounds,
                        result_preview: truncate_chars(&r.content, 120),
                        usage: r.usage.clone(),
                        elapsed_ms,
                    });
                }
                Err(e) => {
                    cb(ToolEvent::SubagentComplete {
                        agent_name: agent_name.to_string(),
                        success: false,
                        tool_rounds: 0,
                        result_preview: format!("Error: {}", e),
                        usage: None,
                        elapsed_ms,
                    });
                }
            }
        }
    }
}

// ── Shared helpers ──

/// Maximum output size (in characters) for a subagent result returned to the
/// lead agent. This prevents the lead agent's synthesis round from being
/// overwhelmed when multiple subagents each produce very long outputs.
///
/// 20K chars ≈ 5K-7K tokens. With 5-6 parallel subagents, the lead agent's
/// synthesis prompt stays under ~40K tokens (manageable for any model).
const MAX_SUBAGENT_OUTPUT_CHARS: usize = 20_000;

/// Format a successful SubagentResult into the output string for the main agent.
///
/// If the subagent's output exceeds `MAX_SUBAGENT_OUTPUT_CHARS`, it is truncated
/// with a notice. This ensures the lead agent's synthesis round doesn't exceed
/// its context budget when aggregating results from many parallel subagents.
fn format_result(result: &SubagentResult) -> String {
    let content = if result.content.len() > MAX_SUBAGENT_OUTPUT_CHARS {
        let truncated = crate::tools::truncate_at_char_boundary(
            &result.content,
            MAX_SUBAGENT_OUTPUT_CHARS,
        );
        format!(
            "{}...\n\n[Output truncated: {} chars total, showing first {}]",
            truncated,
            result.content.len(),
            MAX_SUBAGENT_OUTPUT_CHARS,
        )
    } else {
        result.content.clone()
    };

    format!(
        "[Subagent '{}' completed — {} tool rounds, {} tokens used]\n\n{}",
        result.agent_name,
        result.tool_rounds,
        result
            .usage
            .as_ref()
            .and_then(|u| u.total_tokens)
            .map(|t| t.to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        content,
    )
}

/// Extract `<shared_context>...</shared_context>` from a task description.
///
/// The orchestrator can embed project-wide context in the task description
/// using this XML-like tag. We extract it so it can be injected into the
/// subagent's system prompt (via `build_effective_prompt_with_context`),
/// which enables:
/// 1. **Breaking information silos** — all parallel subagents share the same
///    project overview without needing to re-explore
/// 2. **Prefix caching** — shared context in the system prompt is cached
///    across all tool-calling rounds (vs. user message which isn't)
/// 3. **Cross-module awareness** — subagents can understand interfaces and
///    dependencies with modules outside their assigned scope
///
/// Returns `(effective_task, shared_context)` where `effective_task` is the
/// task with the shared_context block removed.
fn extract_shared_context(task: &str) -> (String, Option<String>) {
    const OPEN_TAG: &str = "<shared_context>";
    const CLOSE_TAG: &str = "</shared_context>";

    if let Some(start) = task.find(OPEN_TAG) {
        if let Some(end) = task.find(CLOSE_TAG) {
            // Ensure close tag comes after open tag (malformed input guard)
            if end <= start {
                return (task.to_string(), None);
            }
            let context_start = start + OPEN_TAG.len();
            let context = task[context_start..end].trim().to_string();

            // Remove the shared_context block from the task
            let mut effective_task = String::with_capacity(task.len());
            effective_task.push_str(task[..start].trim());
            let after = task[end + CLOSE_TAG.len()..].trim();
            if !after.is_empty() {
                if !effective_task.is_empty() {
                    effective_task.push_str("\n\n");
                }
                effective_task.push_str(after);
            }

            if context.is_empty() {
                return (effective_task, None);
            }
            return (effective_task, Some(context));
        }
    }

    (task.to_string(), None)
}

// ── Factory functions (moved from registry.rs to eliminate circular dependency) ──

/// Build a `BuiltinTool` for the `spawn_subagent` tool.
///
/// Returns `None` if no subagents are loaded.
pub fn build_subagent_tool(
    registry: &Arc<SubagentRegistry>,
    runner: Arc<SubagentRunner>,
    event_sink: SubagentEventSink,
    tracing_hook: SharedTracingHook,
) -> Option<Box<dyn BuiltinTool>> {
    if registry.agent_count() == 0 {
        return None;
    }
    Some(Box::new(SubagentTool {
        ctx: SubagentToolContext {
            registry: Arc::clone(registry),
            runner,
            event_sink,
            tracing_hook,
        },
    }))
}

/// Build a `BuiltinTool` for the `spawn_team` tool.
///
/// Returns `None` if fewer than 2 subagents are loaded (teams need
/// at least 2 agents to be useful). Only compiled in when the `team`
/// feature is enabled.
#[cfg(feature = "team")]
pub fn build_team_tool(
    registry: &Arc<SubagentRegistry>,
    runner: Arc<SubagentRunner>,
    event_sink: SubagentEventSink,
    tracing_hook: SharedTracingHook,
) -> Option<Box<dyn BuiltinTool>> {
    if registry.agent_count() < 2 {
        return None;
    }
    Some(Box::new(TeamTool {
        ctx: SubagentToolContext {
            registry: Arc::clone(registry),
            runner,
            event_sink,
            tracing_hook,
        },
    }))
}

/// Build the OpenAI function-calling JSON definition for the `spawn_subagent` tool.
///
/// The tool description includes a catalog of all available subagents
/// and their descriptions, so the LLM can make informed routing decisions.
///
/// Returns `None` if no subagents are loaded.
pub fn build_tool_definition(registry: &SubagentRegistry) -> Option<serde_json::Value> {
    if registry.agent_count() == 0 {
        return None;
    }

    // Build the subagent catalog for the tool description
    let agent_list: Vec<String> = registry
        .agent_infos()
        .iter()
        .map(|info| format!("  - **{}**: {}", info.name, info.description))
        .collect();

    let description = format!(
        "Spawn a specialized subagent to handle a task that runs in an isolated context. \
         The subagent has its own system prompt, tools, and conversation history — \
         completely separate from the main conversation.\n\n\
         Available subagents:\n{}\n\n\
         Use this tool when:\n\
         - A task matches a subagent's specialization\n\
         - You want to isolate a sub-task to preserve main conversation context\n\
         - The task produces large intermediate output that would clutter the main conversation\n\n\
         IMPORTANT — Context isolation:\n\
         - Each subagent has a completely independent context. If you spawn subagent A \
         then subagent B, B cannot see any files or data that A read.\n\
         - Do NOT spawn an 'explore' subagent to read code and then spawn 'code-reviewer' \
         to review it — the reviewer would have to re-read everything from scratch, \
         doubling the cost. Instead, spawn the specialized subagent directly.\n\
         - For code review tasks, spawn 'code-reviewer' directly — it has all the tools \
         it needs (read_file, grep_search, bash, etc.) to explore and review independently.\n\n\
         ORCHESTRATOR EFFICIENCY — Minimize exploration before dispatching:\n\
         - Do NOT spend multiple rounds exploring the codebase before spawning subagents.\n\
         - Use ONE bash command to gather all needed info (file list + LOC stats) in a single call.\n\
         - Maximum 2 rounds of exploration before you MUST start dispatching subagents.\n\
         - If you need a plan agent to partition work, spawn it immediately — don't pre-explore.\n\n\
         SYNTHESIS & VALIDATION — When collecting subagent results:\n\
         - Subagents annotate findings with confidence levels: [HIGH], [MEDIUM], [LOW].\n\
         - For Critical/High-severity findings marked [MEDIUM] or [LOW], verify them yourself \
         (read the cited file:line) before including in your final output.\n\
         - Cross-reference findings across subagents: if subagent A reports an issue in a \
         module that subagent B also reviewed, check for consistency.\n\
         - Do NOT blindly concatenate subagent outputs — synthesize, deduplicate, and validate.\n\n\
         The subagent will execute the task independently and return a summary of results.",
        agent_list.join("\n")
    );

    // Build the enum of valid subagent names
    let agent_names: Vec<serde_json::Value> = registry
        .agent_infos()
        .iter()
        .map(|info| serde_json::Value::String(info.name.clone()))
        .collect();

    Some(serde_json::json!({
        "type": "function",
        "function": {
            "name": SUBAGENT_TOOL_NAME,
            "description": description,
            "parameters": {
                "type": "object",
                "properties": {
                    "agent_name": {
                        "type": "string",
                        "description": "The name of the subagent to spawn the task to. Must be one of the available subagents.",
                        "enum": agent_names,
                    },
                    "task": {
                        "type": "string",
                        "description": "A clear, self-contained description of the task for the subagent. Include all necessary context since the subagent has no access to the main conversation history.",
                    },
                    "max_rounds": {
                        "type": "integer",
                        "description": "Optional: override the maximum number of tool-calling rounds for this task. Use a lower value (10-20) for simple/focused tasks, higher (40-80) for complex multi-file tasks. If not specified, uses the subagent's default.",
                    },
                    "model": {
                        "type": "string",
                        "description": "Optional: override the model for this task. Use 'haiku' for simple exploration/listing tasks, 'sonnet' for complex reasoning/review tasks. If not specified, uses the subagent's configured model.",
                    }
                },
                "required": ["agent_name", "task"],
            }
        }
    }))
}

// ── SubagentTool ──

/// A `BuiltinTool` implementation that wraps the `SubagentRegistry` and `SubagentRunner`.
///
/// This allows the `ToolRouter` to treat subagent spawn as a regular
/// built-in tool call. The LLM sees `spawn_subagent` as just another
/// tool in the tool list and decides when to use it based on the subagent
/// descriptions in the tool definition.
pub struct SubagentTool {
    ctx: SubagentToolContext,
}

#[async_trait]
impl BuiltinTool for SubagentTool {
    fn name(&self) -> &str {
        SUBAGENT_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Spawn a specialized subagent to handle a task running in an isolated context"
    }

    fn input_schema(&self) -> serde_json::Value {
        // NOTE: Overridden by to_openai_json() below. Kept for trait compliance.
        let agent_names: Vec<serde_json::Value> = self
            .ctx.registry
            .agent_infos()
            .iter()
            .map(|info| serde_json::Value::String(info.name.clone()))
            .collect();

        serde_json::json!({
            "type": "object",
            "properties": {
                "agent_name": {
                    "type": "string",
                    "description": "The name of the subagent to spawn the task to.",
                    "enum": agent_names,
                },
                "task": {
                    "type": "string",
                    "description": "A clear, self-contained task description. Include all necessary context since the subagent has no access to the main conversation.",
                }
            },
            "required": ["agent_name", "task"],
        })
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String> {
        let agent_name = arguments
            .get("agent_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: agent_name"))?;

        let task = arguments
            .get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: task"))?;

        // Optional: LLM can override max_rounds based on task complexity
        let max_rounds_override = arguments
            .get("max_rounds")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);

        // Optional: LLM can override model for this specific task
        let model_override = arguments
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        tracing::info!(
            agent = %agent_name,
            task_len = task.len(),
            max_rounds_override = ?max_rounds_override,
            model_override = ?model_override,
            "LLM invoked spawn_subagent"
        );

        // Look up the subagent definition
        let definition = self.ctx.registry.get(agent_name).ok_or_else(|| {
            let available: Vec<String> = self
                .ctx.registry
                .agent_infos()
                .iter()
                .map(|info| info.name.clone())
                .collect();
            anyhow::anyhow!(
                "Subagent '{}' not found. Available subagents: {}",
                agent_name,
                if available.is_empty() {
                    "(none)".to_string()
                } else {
                    available.join(", ")
                }
            )
        })?;

        // Extract <shared_context>...</shared_context> from the task if present.
        // The orchestrator can embed project-wide context in the task description
        // using this tag. We extract it and inject it into the definition so it
        // appears in the subagent's system prompt (not the user message), enabling
        // prefix caching across rounds.
        let (effective_task, shared_context) = extract_shared_context(task);
        let mut definition = definition.clone();
        if shared_context.is_some() {
            definition.shared_context = shared_context;
        }

        // Apply runtime overrides from the LLM's tool call arguments.
        // These allow the orchestrator to dynamically tune subagent behavior
        // based on task complexity without changing the static YAML config.
        if let Some(rounds) = max_rounds_override {
            // Clamp to reasonable bounds: min 5, max 200
            definition.max_turns = Some(rounds.clamp(5, 200));
        }
        if let Some(ref model) = model_override {
            definition.model = Some(model.clone());
        }

        self.ctx.emit_start(agent_name, &effective_task);

        // Start subagent tracing span if trace context is available.
        // We use the shared context to create the subagent_call span (so it
        // nests under the current tool_call span), then fork the context with
        // the subagent_call span as the initial parent. This gives the subagent
        // an isolated span stack, preventing parallel subagents from interfering
        // with each other's parent-child relationships.
        let trace_ctx = self.ctx.tracing_hook.read();
        let mut subagent_span = if let Some(ref ctx) = trace_ctx {
            if ctx.is_enabled() {
                Some(ctx.start_subagent_call(agent_name, &effective_task).await)
            } else {
                None
            }
        } else {
            None
        };

        // Build tracing hook for the subagent's tool loop.
        // Fork the context so the subagent gets its own span stack, seeded
        // with the subagent_call span as the root parent.
        let subagent_tracing_hook = if let (Some(ctx), Some(span)) = (&trace_ctx, &subagent_span) {
            let forked = ctx.fork(Some(span.span_id().to_string()));
            Some(TracingHook::new(Arc::new(forked)))
        } else {
            None
        };

        // Execute the subagent task (pass through the event callback)
        let callback = self.ctx.read_callback();
        let subagent_start = std::time::Instant::now();
        let result = self.ctx.runner.run(
            &definition, &effective_task, callback.as_ref(), subagent_tracing_hook.as_ref(),
        ).await;
        let subagent_elapsed_ms = subagent_start.elapsed().as_millis() as u64;

        // Finish subagent tracing span
        if let Some(ref mut span) = subagent_span {
            match &result {
                Ok(r) => span.set_subagent_result(&r.content, r.usage.as_ref(), r.tool_rounds),
                Err(e) => span.set_subagent_result(&format!("Error: {}", e), None, 0),
            }
        }
        if let Some(span) = subagent_span {
            match &result {
                Ok(_) => span.finish_ok().await,
                Err(e) => span.finish_error(e.to_string()).await,
            }
        }

        self.ctx.emit_complete(agent_name, &result, subagent_elapsed_ms);

        let result = result?;
        Ok(format_result(&result))
    }

    /// Override the default `to_openai_json` to use the rich description
    /// from `build_tool_definition`.
    fn to_openai_json(&self) -> serde_json::Value {
        build_tool_definition(&self.ctx.registry)
            .unwrap_or_else(|| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": self.name(),
                        "description": self.description(),
                        "parameters": self.input_schema(),
                    }
                })
            })
    }
}

// ── TeamTool (feature-gated) ──

/// A `BuiltinTool` that spawns multiple subagents in parallel.
///
/// This enables "Agent Teams" — the LLM can assign different tasks to
/// different subagents and have them all execute concurrently. Results
/// are collected and returned as a combined summary.
///
/// Only compiled in when the `team` feature is enabled.
#[cfg(feature = "team")]
pub struct TeamTool {
    ctx: SubagentToolContext,
}

#[cfg(feature = "team")]
#[async_trait]
impl BuiltinTool for TeamTool {
    fn name(&self) -> &str {
        TEAM_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Spawn multiple subagents in parallel to handle different tasks concurrently. \
         Use when you have multiple independent sub-tasks that can be executed simultaneously \
         by different specialized subagents."
    }

    fn input_schema(&self) -> serde_json::Value {
        let agent_names: Vec<serde_json::Value> = self
            .ctx.registry
            .agent_infos()
            .iter()
            .map(|info| serde_json::Value::String(info.name.clone()))
            .collect();

        serde_json::json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "description": "An array of task assignments. Each task specifies a subagent and a task description. All tasks execute in parallel.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "agent_name": {
                                "type": "string",
                                "description": "The name of the subagent to assign this task to.",
                                "enum": agent_names,
                            },
                            "task": {
                                "type": "string",
                                "description": "A clear, self-contained task description for this subagent.",
                            }
                        },
                        "required": ["agent_name", "task"],
                    },
                    "minItems": 1,
                }
            },
            "required": ["tasks"],
        })
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String> {
        let tasks_value = arguments
            .get("tasks")
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: tasks"))?;

        let tasks: Vec<super::TeamTask> = serde_json::from_value(tasks_value.clone())
            .map_err(|e| anyhow::anyhow!("Invalid tasks format: {}", e))?;

        if tasks.is_empty() {
            return Ok("No tasks provided.".to_string());
        }

        tracing::info!(
            team_size = tasks.len(),
            "LLM invoked spawn_team"
        );

        // Emit SubagentStart events for all team members
        for task in &tasks {
            self.ctx.emit_start(&format!("team:{}", task.agent_name), &task.task);
        }

        // Execute all tasks in parallel
        let callback = self.ctx.read_callback();
        let team_start = std::time::Instant::now();
        let results = self.ctx.runner.run_team(
            &tasks, &self.ctx.registry, callback.as_ref(),
        ).await;
        let team_elapsed_ms = team_start.elapsed().as_millis() as u64;

        // Format combined results
        let mut output_parts = Vec::new();
        output_parts.push(format!(
            "[Team execution completed — {} tasks]\n",
            results.len()
        ));

        for (i, (task, result)) in tasks.iter().zip(results.iter()).enumerate() {
            let team_agent_name = format!("team:{}", task.agent_name);
            match result {
                Ok(r) => {
                    output_parts.push(format!(
                        "### Task {} — Subagent '{}' ✓ ({} tool rounds, {} tokens)\n\n{}\n",
                        i + 1,
                        r.agent_name,
                        r.tool_rounds,
                        r.usage.as_ref().and_then(|u| u.total_tokens)
                            .map(|t| t.to_string())
                            .unwrap_or_else(|| "unknown".to_string()),
                        r.content,
                    ));
                }
                Err(e) => {
                    output_parts.push(format!(
                        "### Task {} — Subagent '{}' ✗\n\nError: {}\n",
                        i + 1,
                        task.agent_name,
                        e,
                    ));
                }
            }
            self.ctx.emit_complete(&team_agent_name, result, team_elapsed_ms);
        }

        Ok(output_parts.join("\n"))
    }
}
