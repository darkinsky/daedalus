mod chat;
pub(crate) mod core_handler;
pub(crate) mod duplicate_detector;
mod session;
pub(crate) mod tool_loop;
pub(crate) mod tool_router;

pub use chat::ChatAgent;
pub(crate) use session::Session;
pub(crate) use session::SharedMemory;
pub use tool_router::ToolFilter;

use anyhow::Result;
use async_trait::async_trait;

use crate::llm::ChatResponse;
use crate::tools::{ToolInfo, ToolEventCallback};
use crate::mcp::McpManager;
use crate::middleware::builtin::cost::SharedSessionCost;
use crate::skill::SkillInfo;
use crate::subagent::SubagentInfo;

// ── Agent metadata trait (read-only introspection) ──

/// Read-only metadata about an agent — used by the CLI layer for display.
///
/// This trait is separated from `AgentMode` to keep the core trait focused
/// on behavior (chat, session management, shutdown) while metadata methods
/// (tool listing, model info, skill/subagent introspection) live here.
///
/// `AgentMode` requires `AgentMetadata` as a supertrait, so any `dyn AgentMode`
/// automatically provides all metadata methods.
pub trait AgentMetadata {
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

    /// Return metadata for all available subagents.
    fn subagent_infos(&self) -> Vec<SubagentInfo> {
        vec![]
    }

    /// Return the number of loaded subagents.
    fn subagent_count(&self) -> usize {
        0
    }

    /// Return the shared session cost tracker, if available.
    ///
    /// Used by the CLI layer for `/cost` display and turn footers.
    /// The cost is automatically accumulated by `CostTurnMiddleware`.
    fn session_cost(&self) -> Option<&SharedSessionCost> {
        None
    }
}

// ── Agent mode trait (core behavior) ──

/// The agent mode trait — unified interface for different agent modes.
///
/// Focused on core behavior: chat interaction, session lifecycle, and cleanup.
/// Read-only introspection methods are in the `AgentMetadata` supertrait.
///
/// Currently we have:
/// - `ChatAgent`: Multi-turn conversation with optional MCP tool calling.
///
/// In the future, more modes can be added, such as:
/// - Full agent mode with planning and multi-step execution.
#[async_trait]
pub trait AgentMode: AgentMetadata + Send + Sync {
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

    /// Start a new conversation session.
    fn new_session(&mut self);

    /// Set the subagent event callback for real-time progress display.
    ///
    /// Called by the REPL before each chat call to bind the callback
    /// to the current spinner. Pass `None` to clear the callback.
    fn set_subagent_event_callback(&self, _callback: Option<ToolEventCallback>) {}

    /// Persist all memory state to the workspace and perform cleanup.
    ///
    /// Called when the REPL exits (normally or via Ctrl-D).
    /// The default implementation does nothing.
    async fn shutdown(&mut self) -> Result<()> {
        Ok(())
    }

    /// Persist memory state to disk after each turn.
    ///
    /// Called by the REPL after each successful chat turn to ensure
    /// conversation history survives process crashes. The default
    /// implementation does nothing (for modes without persistence).
    async fn persist_memory(&self) {}

    /// Run context compression (compact) on the conversation history.
    ///
    /// Compresses older messages into a summary while preserving recent
    /// messages verbatim. Returns a human-readable status message.
    ///
    /// # Arguments
    /// * `instruction` — Optional user instruction to focus the summary.
    async fn compact(&mut self, _instruction: Option<&str>) -> anyhow::Result<String> {
        Ok("Compact is not supported in this mode.".to_string())
    }
}
