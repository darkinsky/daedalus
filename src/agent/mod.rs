mod chat;
pub(crate) mod tool_router;

pub use chat::ChatAgent;

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

use crate::llm::ChatResponse;
use crate::tools::ToolInfo;
use crate::mcp::McpManager;
use crate::session::Session;
use crate::skill::SkillInfo;

// ── Tool execution events (for CLI progress display) ──

/// Events emitted during tool execution, allowing the CLI layer
/// to display real-time progress of the tool-calling loop.
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
    },
    /// A tool call has completed.
    ToolCallComplete {
        /// The tool name that was called.
        tool_name: String,
        /// Whether the call succeeded.
        success: bool,
        /// Brief result summary (truncated).
        result_preview: String,
    },
    /// All tool calls in a round have completed.
    RoundComplete {
        /// Number of tool calls executed in this round.
        tool_count: usize,
    },
}

/// Callback type for receiving tool execution events.
///
/// The callback is wrapped in `Arc` so it can be shared across async boundaries.
/// It takes a `ToolEvent` and renders it to the terminal (or ignores it).
pub type ToolEventCallback = Arc<dyn Fn(ToolEvent) + Send + Sync>;

// ── Agent mode trait ──

/// The agent mode trait — unified interface for different agent modes.
///
/// Currently we have:
/// - `ChatAgent`: Multi-turn conversation with optional MCP tool calling.
///
/// In the future, more modes can be added, such as:
/// - Full agent mode with planning and multi-step execution.
#[async_trait]
pub trait AgentMode: Send + Sync {
    /// Send a user message and get the response (with usage metadata).
    ///
    /// An optional `on_tool_event` callback can be provided to receive
    /// real-time notifications about tool execution progress.
    async fn chat(
        &mut self,
        user_input: &str,
        on_tool_event: Option<&ToolEventCallback>,
    ) -> Result<ChatResponse>;

    /// Attach an MCP manager to enable tool calling.
    ///
    /// The default implementation does nothing (for modes that don't support tools).
    fn attach_mcp(&mut self, _mcp: McpManager) {}

    /// Return true if this agent has any tools available (built-in, skill, or MCP).
    fn has_tools(&self) -> bool {
        false
    }

    /// Return the total number of tools available (built-in, skill, and MCP).
    fn tool_count(&self) -> usize {
        0
    }

    /// Return metadata for all available tools (for CLI display and prompt building).
    fn tool_infos(&self) -> Vec<ToolInfo> {
        vec![]
    }

    /// Start a new conversation session.
    fn new_session(&mut self);

    /// Return a reference to the current session.
    fn session(&self) -> &Session;

    /// Return the provider name.
    fn provider_name(&self) -> &str;

    /// Return the model name.
    fn model_name(&self) -> &str;

    /// Return the mode name (e.g., "chat", "agent").
    fn mode_name(&self) -> &str;

    /// Return metadata for all available skills.
    fn skill_infos(&self) -> Vec<SkillInfo> {
        vec![]
    }

    /// Return the number of loaded skills.
    fn skill_count(&self) -> usize {
        0
    }

    /// Persist all memory state to the workspace and perform cleanup.
    ///
    /// Called when the REPL exits (normally or via Ctrl-D).
    /// The default implementation does nothing.
    async fn shutdown(&mut self) -> Result<()> {
        Ok(())
    }
}
