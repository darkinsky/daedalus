pub mod bash;
pub(crate) mod checkpoint;
pub(crate) mod edit_file;
mod fs_utils;
mod get_file_info;
mod grep_search;
mod list_directory;
mod multi_edit;
pub(crate) mod plan;
pub(crate) mod recall_history;
mod read_file;
mod search_files;
pub(crate) mod take_note;
pub(crate) mod text_utils;
pub(crate) mod web_search;
mod write_file;

use bash::BashConfig;
use web_search::WebSearchConfig;

// ── Unified tool configuration ──

/// Top-level `tools:` section in the YAML configuration file.
///
/// Aggregates per-tool configuration. Only tools that need external
/// credentials or environment-specific tuning have config sections here.
/// All tools work with zero configuration using sensible defaults.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct ToolsConfig {
    /// Web search tool configuration (provider, API key, etc.).
    pub web_search: WebSearchConfig,
    /// Bash tool configuration (timeouts, output limits).
    pub bash: BashConfig,
}

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

// Re-export shared text utilities so callers can use `crate::tools::truncate_chars`
// without knowing about the internal `text_utils` module layout.
pub(crate) use text_utils::{truncate_at_char_boundary, truncate_chars};
pub(crate) use text_utils::maybe_truncate_for_display;

// Re-export format_size for human-readable byte formatting.
pub(crate) use fs_utils::format_size;

// Re-export rg pre-initialization for eager startup outside async context.
pub(crate) use grep_search::ensure_rg_init;

// Re-export file modification tracking for session-level context management.
#[allow(unused_imports)]
pub(crate) use edit_file::get_modified_files;

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
    /// A streaming text chunk from the LLM response.
    ///
    /// Emitted during streaming mode so the CLI can render text
    /// incrementally (typewriter effect) as it arrives from the LLM.
    StreamText {
        /// The incremental text content.
        text: String,
    },
    /// A streaming reasoning/thinking chunk from the LLM.
    StreamReasoning {
        /// The incremental reasoning text.
        text: String,
    },
    /// The streaming response has completed.
    StreamDone,
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
    /// Context budget exceeded — the tool loop is being force-stopped.
    ///
    /// Emitted when the estimated context usage exceeds the hard limit ratio,
    /// signaling that the loop will make one final LLM call and exit.
    ContextBudgetExceeded {
        /// Current context usage percentage (0-100).
        usage_pct: u8,
    },
    /// A line of real-time output from a bash command.
    ///
    /// Emitted during streaming bash execution so the CLI can display
    /// command output as it arrives, rather than waiting for completion.
    BashStreamLine {
        /// The output line (may include "[stderr] " prefix for stderr lines).
        line: String,
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
    /// Optional per-tool usage guidance injected into the system prompt.
    ///
    /// When set, this hint is rendered below the tool's entry in the tool
    /// inventory section, providing specific when-to-use / when-NOT-to-use
    /// guidance for this particular tool.
    pub usage_hint: Option<String>,
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

    /// Whether this tool only reads data without modifying the filesystem or state.
    ///
    /// Used by the permission system to distinguish read-only operations
    /// (which can run without confirmation) from write operations.
    /// Default: `true` (most tools are read-only; write tools override this).
    #[allow(dead_code)]
    fn is_read_only(&self) -> bool {
        true
    }

    /// Whether this tool is safe to run concurrently with other tool calls.
    ///
    /// Tools that modify files should return `false` to prevent race conditions
    /// when the LLM issues parallel tool calls targeting the same resource.
    /// Default: `true` (read-only tools are always concurrency-safe).
    #[allow(dead_code)]
    fn is_concurrency_safe(&self) -> bool {
        self.is_read_only()
    }

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
    /// Bash tool configuration, kept separately so `call_tool_streaming`
    /// can create a streaming `BashTool` with the correct user config
    /// (trait objects cannot be downcast).
    bash_config: BashConfig,
}

impl BuiltinToolRegistry {
    /// Create a new registry with the default set of built-in tools.
    ///
    /// Uses default configuration for all tools. For custom configuration,
    /// use `new_with_config()`.
    pub fn new() -> Self {
        Self::new_with_config(BashConfig::default())
    }

    /// Create a new registry with custom tool configuration.
    pub fn new_with_config(bash_config: BashConfig) -> Self {
        let tools: Vec<Box<dyn BuiltinTool>> = vec![
            Box::new(read_file::ReadFileTool),
            Box::new(write_file::WriteFileTool),
            Box::new(edit_file::EditFileTool),
            Box::new(multi_edit::MultiEditTool),
            Box::new(list_directory::ListDirectoryTool),
            Box::new(search_files::SearchFilesTool),
            Box::new(grep_search::GrepSearchTool),
            Box::new(get_file_info::GetFileInfoTool),
            Box::new(bash::BashTool::new(bash_config.clone())),
            Box::new(take_note::TakeNoteTool::new(take_note::new_shared_notes())),
            Box::new(plan::CreatePlanTool::new()),
            Box::new(plan::UpdatePlanTool::new()),
        ];

        tracing::info!(
            tool_count = tools.len(),
            "Built-in tool registry initialized"
        );

        Self { tools, bash_config }
    }

    /// Create an empty registry (no default tools).
    ///
    /// Used by `SubagentRunner` to build a filtered tool set by
    /// selectively registering tools from the full registry.
    pub fn new_empty() -> Self {
        Self { tools: Vec::new(), bash_config: BashConfig::default() }
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

    /// Replace an existing tool by name with a new instance.
    ///
    /// If a tool with the same name already exists, it is removed and the
    /// new tool is inserted in its place. If no tool with that name exists,
    /// the new tool is simply appended.
    pub fn replace_tool(&mut self, tool: Box<dyn BuiltinTool>) {
        let name = tool.name().to_string();
        if let Some(pos) = self.tools.iter().position(|t| t.name() == name) {
            self.tools[pos] = tool;
            tracing::debug!(tool = %name, "Replaced built-in tool with configured instance");
        } else {
            self.tools.push(tool);
            tracing::debug!(tool = %name, "Registered new built-in tool (no existing to replace)");
        }
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
                usage_hint: None,
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

    /// Execute a built-in tool with optional streaming output callback.
    ///
    /// For the `bash` tool, if `on_output` is provided, uses streaming execution
    /// that sends output lines in real-time. For all other tools, falls back to
    /// the standard `execute()` method.
    pub async fn call_tool_streaming(
        &self,
        name: &str,
        arguments: serde_json::Value,
        on_output: Option<Arc<dyn Fn(String) + Send + Sync>>,
    ) -> Result<String> {
        // Special case: bash tool with streaming callback
        if name == "bash" {
            if let Some(callback) = on_output {
                // We can't downcast trait objects, so we create a new BashTool
                // with the same config stored in the registry.
                let bash_tool = bash::BashTool::new(self.bash_config.clone());
                return bash_tool.execute_streaming(arguments, move |line| {
                    callback(line);
                }).await;
            }
        }

        // Default: non-streaming execution
        self.call_tool(name, arguments).await
    }
}
