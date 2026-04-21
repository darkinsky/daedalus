pub mod bash;
mod edit_file;
mod fs_utils;
mod get_file_info;
mod grep_search;
mod list_directory;
mod multi_edit;
pub(crate) mod recall_history;
mod read_file;
mod search_files;
pub(crate) mod text_utils;
mod write_file;

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

// Re-export shared text utilities so callers can use `crate::tools::truncate_chars`
// without knowing about the internal `text_utils` module layout.
pub(crate) use text_utils::{truncate_at_char_boundary, truncate_chars};
pub(crate) use text_utils::maybe_truncate_for_display;

// Re-export rg pre-initialization for eager startup outside async context.
pub(crate) use grep_search::ensure_rg_init;

// ── Tool execution events ──

/// Events emitted during tool execution, allowing the CLI layer
/// to display real-time progress of the tool-calling loop.
///
/// Defined in `tools` (not `agent`) so that both `agent` and `subagent`
/// can depend on it without creating a circular dependency.
#[derive(Debug, Clone)]
pub enum ToolEvent {
    /// A new tool-calling round has started.
    RoundStart {
        /// 1-based round number.
        round: usize,
    },
    /// A tool call is about to be executed.
    ToolCallStart {
        /// The tool name being called.
        tool_name: String,
        /// Which source handles this tool ("built-in" or MCP server name).
        source: String,
        /// Arguments passed to the tool (raw JSON as sent by the LLM).
        ///
        /// Used by the CLI renderer to produce a human-readable one-line
        /// summary (e.g. "read_file  src/foo.rs:100-200", "bash  $ cargo build")
        /// and by `stream-json` consumers that want to inspect inputs.
        arguments: serde_json::Value,
    },
    /// A tool call has completed.
    ToolCallComplete {
        /// The tool name that was called.
        tool_name: String,
        /// Whether the call succeeded.
        success: bool,
        /// Full result content (used for both CLI display and stream-json consumers).
        result_content: String,
        /// Wall-clock time the tool call took, in milliseconds.
        elapsed_ms: u64,
    },
    /// All tool calls in a round have completed.
    RoundComplete {
        /// Number of tool calls executed in this round.
        tool_count: usize,
        /// Wall-clock time the entire round took (parallel execution), in milliseconds.
        elapsed_ms: u64,
    },
    /// Intermediate LLM response during tool-calling rounds.
    ///
    /// Emitted after each LLM call that produces tool calls, so the CLI
    /// can display the model's reasoning and any partial content in real time.
    LlmResponse {
        /// The round number (1-based) this response belongs to.
        round: usize,
        /// Optional reasoning/thinking content from the model.
        reasoning: Option<String>,
        /// The text content of the response (may be empty if only tool calls).
        content: String,
        /// Token usage for this individual round.
        usage: Option<crate::llm::TokenUsage>,
        /// Wall-clock time the LLM call took, in milliseconds.
        elapsed_ms: u64,
    },
    /// A subagent has started execution.
    SubagentStart {
        /// The subagent name.
        agent_name: String,
        /// The task description (truncated).
        task_preview: String,
    },
    /// A subagent has completed execution.
    SubagentComplete {
        /// The subagent name.
        agent_name: String,
        /// Whether the execution succeeded.
        success: bool,
        /// Number of tool rounds the subagent executed.
        tool_rounds: usize,
        /// Brief result summary (truncated).
        result_preview: String,
        /// Token usage statistics for this subagent (accumulated across all rounds).
        usage: Option<crate::llm::TokenUsage>,
        /// Wall-clock time the subagent took, in milliseconds.
        elapsed_ms: u64,
    },
}

/// Callback type for receiving tool execution events.
///
/// The callback is wrapped in `Arc` so it can be shared across async boundaries.
/// It takes a `ToolEvent` and renders it to the terminal (or ignores it).
pub type ToolEventCallback = Arc<dyn Fn(ToolEvent) + Send + Sync>;

// ── Tool metadata ──

/// A tool description for CLI display, prompt building, and tool routing.
///
/// This is the canonical definition of tool metadata, shared across all
/// modules that need to describe tools (agent, prompt, CLI, MCP).
#[derive(Debug, Clone)]
pub struct ToolInfo {
    /// The tool name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Which source provides this tool (e.g., "built-in", MCP server name).
    pub source: String,
}

/// A built-in tool that can be called directly without an external MCP server.
///
/// Built-in tools follow the same OpenAI function-calling JSON format as MCP tools,
/// making them seamlessly interchangeable from the LLM's perspective.
#[async_trait]
pub trait BuiltinTool: Send + Sync {
    /// Return the tool name (unique identifier).
    fn name(&self) -> &str;

    /// Return a human-readable description of the tool.
    fn description(&self) -> &str;

    /// Return the JSON Schema for the tool's input parameters.
    fn input_schema(&self) -> serde_json::Value;

    /// Execute the tool with the given arguments and return the result as text.
    async fn execute(&self, arguments: serde_json::Value) -> Result<String>;

    /// Convert to OpenAI function-calling JSON format.
    fn to_openai_json(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name(),
                "description": self.description(),
                "parameters": self.input_schema(),
            }
        })
    }
}

/// Registry that manages all built-in tools.
///
/// Provides tool discovery, definition generation, and execution routing —
/// the same interface pattern as `McpManager` but for local tools.
pub struct BuiltinToolRegistry {
    tools: Vec<Box<dyn BuiltinTool>>,
}

impl BuiltinToolRegistry {
    /// Create a new registry with the default set of built-in tools.
    pub fn new() -> Self {
        let tools: Vec<Box<dyn BuiltinTool>> = vec![
            Box::new(read_file::ReadFileTool),
            Box::new(write_file::WriteFileTool),
            Box::new(edit_file::EditFileTool),
            Box::new(multi_edit::MultiEditTool),
            Box::new(list_directory::ListDirectoryTool),
            Box::new(search_files::SearchFilesTool),
            Box::new(grep_search::GrepSearchTool),
            Box::new(get_file_info::GetFileInfoTool),
            Box::new(bash::BashTool),
        ];

        tracing::info!(
            tool_count = tools.len(),
            "Built-in tool registry initialized"
        );

        Self { tools }
    }

    /// Create an empty registry (no default tools).
    ///
    /// Used by `SubagentRunner` to build a filtered tool set by
    /// selectively registering tools from the full registry.
    pub fn new_empty() -> Self {
        Self { tools: Vec::new() }
    }

    /// Consume the registry and return the owned tool list.
    ///
    /// Used by `SubagentRunner` to iterate over tools and selectively
    /// register them into a filtered registry.
    pub fn into_tools(self) -> Vec<Box<dyn BuiltinTool>> {
        self.tools
    }

    /// Register an additional built-in tool dynamically.
    ///
    /// This is used to add tools at runtime (e.g., the `use_skill` tool
    /// after skills are loaded from disk).
    pub fn register_tool(&mut self, tool: Box<dyn BuiltinTool>) {
        tracing::info!(
            tool = tool.name(),
            "Registered dynamic built-in tool"
        );
        self.tools.push(tool);
    }

    /// Return the total number of built-in tools.
    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }

    /// Build tool definitions in OpenAI function-calling JSON format.
    pub fn build_tool_definitions(&self) -> Vec<serde_json::Value> {
        self.tools.iter().map(|t| t.to_openai_json()).collect()
    }

    /// Return tool metadata for CLI display.
    pub fn tool_infos(&self) -> Vec<ToolInfo> {
        self.tools
            .iter()
            .map(|t| ToolInfo {
                name: t.name().to_string(),
                description: t.description().to_string(),
                source: "built-in".to_string(),
            })
            .collect()
    }

    /// Check if a tool with the given name exists in this registry.
    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.iter().any(|t| t.name() == name)
    }

    /// Execute a built-in tool by name.
    ///
    /// Returns the tool output as a string, or an error if the tool is not found
    /// or execution fails.
    pub async fn call_tool(&self, name: &str, arguments: serde_json::Value) -> Result<String> {
        let tool = self
            .tools
            .iter()
            .find(|t| t.name() == name)
            .ok_or_else(|| anyhow::anyhow!("Built-in tool '{}' not found", name))?;

        tool.execute(arguments).await
    }
}
