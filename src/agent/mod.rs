mod chat;
pub(crate) mod tool_router;

pub use chat::ChatAgent;

use anyhow::Result;
use async_trait::async_trait;

use crate::llm::{ChatResponse, ToolInfo};
use crate::mcp::McpManager;
use crate::session::Session;

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
    async fn chat(&mut self, user_input: &str) -> Result<ChatResponse>;

    /// Attach an MCP manager to enable tool calling.
    ///
    /// The default implementation does nothing (for modes that don't support tools).
    fn attach_mcp(&mut self, _mcp: McpManager) {}

    /// Return true if this agent has MCP tools available.
    fn has_tools(&self) -> bool {
        false
    }

    /// Return the number of MCP tools available.
    fn tool_count(&self) -> usize {
        0
    }

    /// Return descriptions of all available tools (for CLI display).
    fn tool_descriptions(&self) -> Vec<ToolInfo> {
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
}
