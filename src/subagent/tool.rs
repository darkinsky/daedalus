use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use super::registry::SubagentRegistry;
use super::runner::SubagentRunner;
use super::SubagentResult;
use crate::agent::{ToolEvent, ToolEventCallback};
use crate::tools::BuiltinTool;

/// The tool name used for LLM-routed subagent spawn.
const SUBAGENT_TOOL_NAME: &str = "spawn_subagent";

/// The tool name used for parallel multi-agent team execution.
const TEAM_TOOL_NAME: &str = "spawn_team";

/// Shared container for a tool event callback that can be set at runtime.
///
/// The callback is stored behind `RwLock` so it can be updated by the REPL
/// before each chat call (to bind to the current spinner) and cleared after.
/// The `SubagentTool` reads it during execution.
pub type SharedEventCallback = Arc<std::sync::RwLock<Option<ToolEventCallback>>>;

// ── Shared infrastructure ──

/// Shared state held by both `SubagentTool` and `TeamTool`.
///
/// Extracted to avoid duplicating the same three `Arc` fields and
/// their accessor patterns across both tool implementations.
struct SubagentToolBase {
    registry: Arc<SubagentRegistry>,
    runner: Arc<SubagentRunner>,
    shared_callback: SharedEventCallback,
}

impl SubagentToolBase {
    /// Read the current event callback from the shared container.
    ///
    /// Returns `None` if the lock is poisoned or no callback is set.
    fn read_callback(&self) -> Option<ToolEventCallback> {
        self.shared_callback.read().ok().and_then(|guard| guard.clone())
    }

    /// Emit a `SubagentStart` event via the current callback (if present).
    fn emit_start(&self, agent_name: &str, task: &str) {
        if let Some(ref cb) = self.read_callback() {
            cb(ToolEvent::SubagentStart {
                agent_name: agent_name.to_string(),
                task_preview: truncate_preview(task, 100),
            });
        }
    }

    /// Emit a `SubagentComplete` event based on the execution result.
    fn emit_complete(&self, agent_name: &str, result: &Result<SubagentResult>) {
        if let Some(ref cb) = self.read_callback() {
            match result {
                Ok(r) => {
                    cb(ToolEvent::SubagentComplete {
                        agent_name: agent_name.to_string(),
                        success: true,
                        tool_rounds: r.tool_rounds,
                        result_preview: truncate_preview(&r.content, 120),
                    });
                }
                Err(e) => {
                    cb(ToolEvent::SubagentComplete {
                        agent_name: agent_name.to_string(),
                        success: false,
                        tool_rounds: 0,
                        result_preview: format!("Error: {}", e),
                    });
                }
            }
        }
    }
}

// ── Shared helpers ──

/// Truncate a string to at most `max_chars` characters, appending "…" if truncated.
///
/// Uses `char_indices` for UTF-8 safe truncation — never panics on
/// multi-byte characters (e.g. Chinese, emoji).
fn truncate_preview(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        Some((byte_pos, _)) => format!("{}…", &s[..byte_pos]),
        None => s.to_string(),
    }
}

/// Format
/// Format a successful SubagentResult into the output string for the main agent.
fn format_result(result: &SubagentResult) -> String {
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
        result.content,
    )
}

// ── Factory functions (moved from registry.rs to eliminate circular dependency) ──

/// Build a `BuiltinTool` for the `spawn_subagent` tool.
///
/// Returns `None` if no subagents are loaded.
pub fn build_subagent_tool(
    registry: &Arc<SubagentRegistry>,
    runner: Arc<SubagentRunner>,
    shared_callback: SharedEventCallback,
) -> Option<Box<dyn BuiltinTool>> {
    if registry.agent_count() == 0 {
        return None;
    }
    Some(Box::new(SubagentTool {
        base: SubagentToolBase {
            registry: Arc::clone(registry),
            runner,
            shared_callback,
        },
    }))
}

/// Build a `BuiltinTool` for the `spawn_team` tool.
///
/// Returns `None` if fewer than 2 subagents are loaded (teams need
/// at least 2 agents to be useful).
pub fn build_team_tool(
    registry: &Arc<SubagentRegistry>,
    runner: Arc<SubagentRunner>,
    shared_callback: SharedEventCallback,
) -> Option<Box<dyn BuiltinTool>> {
    if registry.agent_count() < 2 {
        return None;
    }
    Some(Box::new(TeamTool {
        base: SubagentToolBase {
            registry: Arc::clone(registry),
            runner,
            shared_callback,
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
    base: SubagentToolBase,
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
            .base.registry
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

        tracing::info!(
            agent = %agent_name,
            task_len = task.len(),
            "LLM invoked spawn_subagent"
        );

        // Look up the subagent definition
        let definition = self.base.registry.get(agent_name).ok_or_else(|| {
            let available: Vec<String> = self
                .base.registry
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

        self.base.emit_start(agent_name, task);

        // Execute the subagent task (pass through the event callback)
        let callback = self.base.read_callback();
        let result = self.base.runner.run(definition, task, callback.as_ref()).await;

        self.base.emit_complete(agent_name, &result);

        let result = result?;
        Ok(format_result(&result))
    }

    /// Override the default `to_openai_json` to use the rich description
    /// from `build_tool_definition`.
    fn to_openai_json(&self) -> serde_json::Value {
        build_tool_definition(&self.base.registry)
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

// ── TeamTool ──

/// A `BuiltinTool` that spawns multiple subagents in parallel.
///
/// This enables "Agent Teams" — the LLM can assign different tasks to
/// different subagents and have them all execute concurrently. Results
/// are collected and returned as a combined summary.
pub struct TeamTool {
    base: SubagentToolBase,
}

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
            .base.registry
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
            self.base.emit_start(&format!("team:{}", task.agent_name), &task.task);
        }

        // Execute all tasks in parallel
        let callback = self.base.read_callback();
        let results = self.base.runner.run_team(
            &tasks, &self.base.registry, callback.as_ref(),
        ).await;

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
            self.base.emit_complete(&team_agent_name, result);
        }

        Ok(output_parts.join("\n"))
    }
}
