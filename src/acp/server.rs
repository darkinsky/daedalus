//! ACP Server trait — the standard interface for agents that receive requests.
//!
//! Any agent that wants to participate in the ACP protocol as a service
//! provider implements the `AcpServer` trait. This is the server-side
//! counterpart to `AcpClient`.
//!
//! ## Design
//!
//! The trait is intentionally minimal — just three methods:
//!
//! 1. `card()` — return the agent's capability descriptor
//! 2. `handle_task()` — process a task request and return a response
//! 3. `handle_task_with_events()` — process with streaming event callback
//!
//! This follows Daedalus's "trait-first" design principle: the protocol
//! defines the interface, implementations provide the behavior.
//!
//! ## Implementations
//!
//! Phase 1 provides `LocalAcpServer`, which wraps the existing
//! `SubagentRunner` to make subagents ACP-compatible without changes.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use super::agent_card::AgentCard;
use super::types::{AcpError, AcpErrorCode, TaskEvent, TaskRequest, TaskResponse, TaskState, TaskUsage};

/// Callback type for receiving streaming task events.
///
/// Similar to `ToolEventCallback` but for ACP-level events.
pub type TaskEventCallback = Arc<dyn Fn(TaskEvent) + Send + Sync>;

/// The ACP server trait — implemented by any agent that can receive task requests.
///
/// This is the core abstraction of the ACP protocol's server side.
/// Implementations range from local subagent wrappers to future HTTP
/// server endpoints.
#[async_trait]
pub trait AcpServer: Send + Sync {
    /// Return this agent's capability card.
    ///
    /// The card is used for discovery and routing — clients inspect it
    /// to decide whether this agent is suitable for a given task.
    fn card(&self) -> &AgentCard;

    /// Process a task request and return the final response.
    ///
    /// This is the simple, non-streaming interface. The agent processes
    /// the entire task and returns the result when done.
    async fn handle_task(&self, request: TaskRequest) -> Result<TaskResponse, AcpError>;

    /// Process a task request with streaming event callbacks.
    ///
    /// The default implementation delegates to `handle_task()` with
    /// synthetic state-change events. Override for true streaming support.
    async fn handle_task_with_events(
        &self,
        request: TaskRequest,
        on_event: Option<&TaskEventCallback>,
    ) -> Result<TaskResponse, AcpError> {
        // Emit "processing" state change
        if let Some(cb) = on_event {
            cb(TaskEvent::StateChange {
                state: TaskState::Processing,
                message: Some(format!("Agent '{}' started processing", self.card().name)),
            });
        }

        let result = self.handle_task(request).await;

        // Emit terminal state change
        if let Some(cb) = on_event {
            match &result {
                Ok(resp) => {
                    cb(TaskEvent::StateChange {
                        state: resp.state.clone(),
                        message: None,
                    });
                }
                Err(err) => {
                    cb(TaskEvent::StateChange {
                        state: TaskState::Failed,
                        message: Some(err.message.clone()),
                    });
                }
            }
        }

        result
    }

    /// Return the agent's name (convenience, delegates to card).
    fn name(&self) -> &str {
        &self.card().name
    }
}

// ════════════════════════════════════════════════════════════
// LocalAcpServer — wraps SubagentRunner for ACP compatibility
// ════════════════════════════════════════════════════════════

/// A local ACP server that wraps an existing `SubagentDefinition` + `SubagentRunner`.
///
/// This is the bridge between the existing subagent system and the ACP protocol.
/// It allows any subagent to be used as an ACP server without modifications
/// to the subagent definition or runner.
///
/// ## Example
///
/// ```ignore
/// let server = LocalAcpServer::new(definition, runner);
/// let request = TaskRequest::new("main-agent", "code-reviewer", "Review this PR");
/// let response = server.handle_task(request).await?;
/// ```
pub struct LocalAcpServer {
    /// The agent's capability card (derived from SubagentDefinition).
    card: AgentCard,
    /// The subagent definition (needed for runner execution).
    definition: crate::subagent::SubagentDefinition,
    /// The subagent runner (shared, handles LLM + tool loop).
    runner: Arc<crate::subagent::SubagentRunner>,
}

impl LocalAcpServer {
    /// Create a new local ACP server from a subagent definition and runner.
    pub fn new(
        definition: crate::subagent::SubagentDefinition,
        runner: Arc<crate::subagent::SubagentRunner>,
    ) -> Self {
        let card = AgentCard::from(&definition);
        Self {
            card,
            definition,
            runner,
        }
    }
}

#[async_trait]
impl AcpServer for LocalAcpServer {
    fn card(&self) -> &AgentCard {
        &self.card
    }

    async fn handle_task(&self, request: TaskRequest) -> Result<TaskResponse, AcpError> {
        tracing::info!(
            agent = %self.card.name,
            task_id = %request.metadata.task_id,
            task_len = request.task.len(),
            "ACP LocalServer handling task"
        );

        // Build the full task description with optional context
        let full_task = if let Some(ref context) = request.context {
            format!("{}\n\nContext:\n{}", request.task, context)
        } else {
            request.task.clone()
        };

        // Delegate to the existing SubagentRunner
        match self.runner.run(&self.definition, &full_task, None, None).await {
            Ok(result) => {
                let mut usage = TaskUsage::default();
                if let Some(ref token_usage) = result.usage {
                    usage = TaskUsage::from(token_usage);
                }
                usage.tool_rounds = Some(result.tool_rounds);

                let mut response = TaskResponse::completed(
                    &request,
                    &self.card.name,
                    result.content,
                );
                response = response.with_usage(usage);

                tracing::info!(
                    agent = %self.card.name,
                    task_id = %request.metadata.task_id,
                    tool_rounds = result.tool_rounds,
                    "ACP task completed successfully"
                );

                Ok(response)
            }
            Err(e) => {
                tracing::error!(
                    agent = %self.card.name,
                    task_id = %request.metadata.task_id,
                    error = %e,
                    "ACP task failed"
                );

                Err(AcpError::new(
                    AcpErrorCode::InternalError,
                    format!("Agent '{}' failed: {}", self.card.name, e),
                ))
            }
        }
    }
}

// ════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_local_acp_server_card() {
        let def = crate::subagent::SubagentDefinition {
            name: "test-agent".to_string(),
            description: "A test agent".to_string(),
            system_prompt: "You are a test agent.".to_string(),
            model: None,
            tools: None,
            disallowed_tools: None,
            permission_mode: crate::subagent::PermissionMode::Default,
            max_turns: None,
            source: crate::subagent::SubagentSource::Builtin,
            isolation: crate::subagent::IsolationMode::None,
            on_start: None,
            on_complete: None,
            shared_context: None,
            context_budget_tokens: None,
        };

        let runner = Arc::new(crate::subagent::SubagentRunner::new(
            crate::llm::LlmConfig::default(),
        ));

        let server = LocalAcpServer::new(def, runner);
        assert_eq!(server.card().name, "test-agent");
        assert_eq!(server.name(), "test-agent");
    }
}
