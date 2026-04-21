//! ACP Client — sends requests to local or remote agents.
//!
//! The `AcpClient` is the primary way to interact with ACP-compatible agents.
//! It manages a registry of known agents (via their `AgentCard`s) and routes
//! task requests to the appropriate server.
//!
//! ## Local + Remote Support
//!
//! The client supports both local agents (in-process `AcpServer` implementations)
//! and remote agents (via `RemoteAcpServer` HTTP transport). Both are registered
//! through the same `register()` method and accessed through the same API.
//!
//! ## Usage
//!
//! ```ignore
//! let mut client = AcpClient::new("main-agent");
//!
//! // Register local agents
//! client.register(local_server);
//!
//! // Register remote agents (Phase 2)
//! let remote = RemoteAcpServer::connect("https://agent.example.com").await?;
//! client.register(Arc::new(remote));
//!
//! // Send a task request — works the same for local and remote
//! let response = client.send_task("code-reviewer", "Review this PR").await?;
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use super::agent_card::AgentCard;
use super::server::{AcpServer, TaskEventCallback};
use super::types::{AcpError, AcpErrorCode, TaskRequest, TaskResponse};

/// The ACP client — discovers agents and sends task requests.
///
/// Maintains a registry of known `AcpServer` instances (local and remote)
/// and provides methods for task delegation and agent discovery.
///
/// Both local and remote agents implement the `AcpServer` trait, so the
/// client treats them identically. Use `register()` for any agent, and
/// `connect_remote()` as a convenience for remote agents.
pub struct AcpClient {
    /// The name of the agent that owns this client (used as `sender` in messages).
    agent_name: String,
    /// Registry of known ACP servers, keyed by agent name.
    servers: HashMap<String, Arc<dyn AcpServer>>,
}

impl AcpClient {
    /// Create a new ACP client for the given agent.
    pub fn new(agent_name: impl Into<String>) -> Self {
        Self {
            agent_name: agent_name.into(),
            servers: HashMap::new(),
        }
    }

    /// Register a local ACP server.
    ///
    /// The server's agent card name is used as the registry key.
    /// If an agent with the same name already exists, it is replaced.
    pub fn register(&mut self, server: Arc<dyn AcpServer>) {
        let name = server.card().name.clone();
        tracing::info!(
            agent = %name,
            client = %self.agent_name,
            "Registered ACP server"
        );
        self.servers.insert(name, server);
    }

    /// Unregister an agent by name.
    ///
    /// Returns `true` if the agent was found and removed.
    pub fn unregister(&mut self, agent_name: &str) -> bool {
        self.servers.remove(agent_name).is_some()
    }

    /// Discover all registered agents and return their cards.
    pub fn discover(&self) -> Vec<AgentCard> {
        self.servers.values().map(|s| s.card().clone()).collect()
    }

    /// Get a specific agent's card by name.
    pub fn get_card(&self, agent_name: &str) -> Option<AgentCard> {
        self.servers.get(agent_name).map(|s| s.card().clone())
    }

    /// Check if an agent is registered.
    pub fn has_agent(&self, agent_name: &str) -> bool {
        self.servers.contains_key(agent_name)
    }

    /// Return the number of registered agents.
    pub fn agent_count(&self) -> usize {
        self.servers.len()
    }

    /// Send a task to a named agent and wait for the response.
    ///
    /// This is the primary method for task delegation. It constructs a
    /// `TaskRequest`, routes it to the appropriate server, and returns
    /// the final `TaskResponse`.
    ///
    /// # Errors
    ///
    /// Returns `AcpError::AgentNotFound` if no agent with the given name
    /// is registered.
    pub async fn send_task(
        &self,
        agent_name: &str,
        task: &str,
    ) -> Result<TaskResponse, AcpError> {
        self.send_task_with_events(agent_name, task, None).await
    }

    /// Send a task with streaming event callbacks.
    ///
    /// Like `send_task`, but allows the caller to receive real-time
    /// progress events during task processing.
    pub async fn send_task_with_events(
        &self,
        agent_name: &str,
        task: &str,
        on_event: Option<&TaskEventCallback>,
    ) -> Result<TaskResponse, AcpError> {
        let server = self.servers.get(agent_name).ok_or_else(|| {
            let available: Vec<String> = self.servers.keys().cloned().collect();
            AcpError::new(
                AcpErrorCode::AgentNotFound,
                format!(
                    "Agent '{}' not found. Available agents: {}",
                    agent_name,
                    if available.is_empty() {
                        "(none)".to_string()
                    } else {
                        available.join(", ")
                    }
                ),
            )
        })?;

        let request = TaskRequest::new(&self.agent_name, agent_name, task);

        tracing::info!(
            client = %self.agent_name,
            server = %agent_name,
            task_id = %request.metadata.task_id,
            task_len = task.len(),
            "Sending ACP task request"
        );

        server.handle_task_with_events(request, on_event).await
    }

    /// Send a task request with full control over the request parameters.
    ///
    /// Unlike `send_task`, this method accepts a pre-built `TaskRequest`,
    /// allowing the caller to set context, params, and accept types.
    pub async fn send_request(
        &self,
        request: TaskRequest,
    ) -> Result<TaskResponse, AcpError> {
        let agent_name = &request.metadata.recipient;
        let server = self.servers.get(agent_name.as_str()).ok_or_else(|| {
            AcpError::agent_not_found(agent_name)
        })?;

        tracing::info!(
            client = %self.agent_name,
            server = %agent_name,
            task_id = %request.metadata.task_id,
            "Sending ACP task request (full)"
        );

        server.handle_task(request).await
    }

    /// Send tasks to multiple agents in parallel and collect all results.
    ///
    /// This is the ACP equivalent of the "Agent Teams" feature. Each task
    /// is sent to a different agent and all execute concurrently.
    ///
    /// Returns results in the same order as the input tasks. Failed tasks
    /// return `Err(AcpError)` without affecting other tasks.
    pub async fn send_parallel(
        &self,
        tasks: &[(&str, &str)], // (agent_name, task)
    ) -> Vec<Result<TaskResponse, AcpError>> {
        let futures: Vec<_> = tasks
            .iter()
            .map(|(agent_name, task)| self.send_task(agent_name, task))
            .collect();

        futures::future::join_all(futures).await
    }

    /// Find agents whose cards match a predicate.
    ///
    /// Useful for capability-based routing: find all agents that support
    /// a specific skill, tag, or capability.
    pub fn find_agents<F>(&self, predicate: F) -> Vec<AgentCard>
    where
        F: Fn(&AgentCard) -> bool,
    {
        self.servers
            .values()
            .map(|s| s.card())
            .filter(|card| predicate(card))
            .cloned()
            .collect()
    }

    /// Find agents that have a specific skill.
    pub fn find_by_skill(&self, skill_name: &str) -> Vec<AgentCard> {
        self.find_agents(|card| card.has_skill(skill_name))
    }

    /// Find agents that have a specific tag.
    pub fn find_by_tag(&self, tag: &str) -> Vec<AgentCard> {
        self.find_agents(|card| card.tags.iter().any(|t| t == tag))
    }

    // ── Phase 2: Remote Agent Support ──

    /// Connect to a remote ACP server and register it.
    ///
    /// This is a convenience method that combines `RemoteAcpServer::connect()`
    /// with `register()`. The remote agent's card is fetched from the
    /// well-known URL and the agent is registered in the local registry.
    ///
    /// # Example
    ///
    /// ```ignore
    /// client.connect_remote("https://agent.example.com").await?;
    /// // Now you can send tasks to the remote agent by name
    /// ```
    pub async fn connect_remote(
        &mut self,
        base_url: impl Into<String>,
    ) -> Result<AgentCard, AcpError> {
        let remote = super::http_client::RemoteAcpServer::connect(base_url).await?;
        let card = remote.card().clone();
        self.register(Arc::new(remote));
        Ok(card)
    }

    /// Connect to a specific named agent on a remote ACP server.
    ///
    /// Like `connect_remote()`, but targets a specific agent by name
    /// on a multi-agent server.
    pub async fn connect_remote_agent(
        &mut self,
        base_url: impl Into<String>,
        agent_name: &str,
    ) -> Result<AgentCard, AcpError> {
        let remote = super::http_client::RemoteAcpServer::connect_agent(base_url, agent_name).await?;
        let card = remote.card().clone();
        self.register(Arc::new(remote));
        Ok(card)
    }

    /// Discover all agents on a remote ACP server and register them.
    ///
    /// Fetches the agent list from the remote server and registers each
    /// agent as a `RemoteAcpServer` in the local registry.
    ///
    /// Returns the list of discovered agent cards.
    pub async fn discover_remote(
        &mut self,
        base_url: impl Into<String>,
    ) -> Result<Vec<AgentCard>, AcpError> {
        let base_url = base_url.into();
        // First connect to get the primary agent (for discover_all)
        let probe = super::http_client::RemoteAcpServer::connect(&base_url).await?;
        let agents = probe.discover_all().await?;

        let mut cards = Vec::new();
        for agent_card in &agents {
            let remote = super::http_client::RemoteAcpServer::with_card(
                &base_url,
                agent_card.clone(),
            );
            cards.push(remote.card().clone());
            self.register(Arc::new(remote));
        }

        tracing::info!(
            client = %self.agent_name,
            remote_url = %base_url,
            agents_discovered = cards.len(),
            "Discovered and registered remote agents"
        );

        Ok(cards)
    }

    /// Find all remote agents in the registry.
    pub fn remote_agents(&self) -> Vec<AgentCard> {
        self.find_agents(|card| card.is_remote())
    }

    /// Find all local agents in the registry.
    pub fn local_agents(&self) -> Vec<AgentCard> {
        self.find_agents(|card| !card.is_remote())
    }
}

// ════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::types::TaskState;

    /// A mock ACP server for testing.
    struct MockAcpServer {
        card: AgentCard,
    }

    impl MockAcpServer {
        fn new(name: &str, description: &str) -> Self {
            Self {
                card: AgentCard::new(name, description)
                    .with_skill(super::super::agent_card::AgentSkill::new(
                        "test_skill",
                        "A test skill",
                    )),
            }
        }
    }

    #[async_trait::async_trait]
    impl AcpServer for MockAcpServer {
        fn card(&self) -> &AgentCard {
            &self.card
        }

        async fn handle_task(
            &self,
            request: super::super::types::TaskRequest,
        ) -> Result<super::super::types::TaskResponse, AcpError> {
            Ok(super::super::types::TaskResponse::completed(
                &request,
                &self.card.name,
                format!("Mock response to: {}", request.task),
            ))
        }
    }

    #[test]
    fn test_client_creation() {
        let client = AcpClient::new("main-agent");
        assert_eq!(client.agent_count(), 0);
        assert!(client.discover().is_empty());
    }

    #[test]
    fn test_client_register_and_discover() {
        let mut client = AcpClient::new("main-agent");
        let server = Arc::new(MockAcpServer::new("test-agent", "A test agent"));
        client.register(server);

        assert_eq!(client.agent_count(), 1);
        assert!(client.has_agent("test-agent"));
        assert!(!client.has_agent("nonexistent"));

        let cards = client.discover();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].name, "test-agent");
    }

    #[test]
    fn test_client_unregister() {
        let mut client = AcpClient::new("main-agent");
        let server = Arc::new(MockAcpServer::new("test-agent", "A test agent"));
        client.register(server);

        assert!(client.unregister("test-agent"));
        assert!(!client.has_agent("test-agent"));
        assert!(!client.unregister("test-agent")); // Already removed
    }

    #[test]
    fn test_client_find_by_skill() {
        let mut client = AcpClient::new("main-agent");
        let server = Arc::new(MockAcpServer::new("test-agent", "A test agent"));
        client.register(server);

        let found = client.find_by_skill("test_skill");
        assert_eq!(found.len(), 1);

        let not_found = client.find_by_skill("nonexistent_skill");
        assert!(not_found.is_empty());
    }

    #[tokio::test]
    async fn test_client_send_task() {
        let mut client = AcpClient::new("main-agent");
        let server = Arc::new(MockAcpServer::new("test-agent", "A test agent"));
        client.register(server);

        let response = client.send_task("test-agent", "Do something").await.unwrap();
        assert_eq!(response.state, TaskState::Completed);
        assert!(response.content.contains("Mock response to: Do something"));
    }

    #[tokio::test]
    async fn test_client_send_task_agent_not_found() {
        let client = AcpClient::new("main-agent");
        let result = client.send_task("nonexistent", "Do something").await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, AcpErrorCode::AgentNotFound);
    }

    #[tokio::test]
    async fn test_client_send_parallel() {
        let mut client = AcpClient::new("main-agent");
        client.register(Arc::new(MockAcpServer::new("agent-a", "Agent A")));
        client.register(Arc::new(MockAcpServer::new("agent-b", "Agent B")));

        let results = client.send_parallel(&[
            ("agent-a", "Task for A"),
            ("agent-b", "Task for B"),
        ]).await;

        assert_eq!(results.len(), 2);
        assert!(results[0].is_ok());
        assert!(results[1].is_ok());
        assert!(results[0].as_ref().unwrap().content.contains("Task for A"));
        assert!(results[1].as_ref().unwrap().content.contains("Task for B"));
    }

    #[tokio::test]
    async fn test_client_send_parallel_partial_failure() {
        let mut client = AcpClient::new("main-agent");
        client.register(Arc::new(MockAcpServer::new("agent-a", "Agent A")));

        let results = client.send_parallel(&[
            ("agent-a", "Task for A"),
            ("nonexistent", "Task for nobody"),
        ]).await;

        assert_eq!(results.len(), 2);
        assert!(results[0].is_ok());
        assert!(results[1].is_err());
    }
}
