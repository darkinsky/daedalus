mod chat;

pub use chat::ChatAgent;

use anyhow::Result;
use async_trait::async_trait;

use crate::session::Session;

/// The agent mode trait — unified interface for different agent modes.
///
/// Currently we have:
/// - `ChatAgent`: Simple chat mode (multi-turn conversation, no tool use).
///
/// In the future, more modes can be added, such as:
/// - Full agent mode with tool calling, planning, and execution.
#[async_trait]
pub trait AgentMode: Send + Sync {
    /// Send a user message and get the response.
    async fn chat(&mut self, user_input: &str) -> Result<String>;

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
