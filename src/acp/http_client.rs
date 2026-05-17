//! ACP HTTP Client — remote agent access via HTTP transport.
//!
//! `RemoteAcpServer` implements the `AcpServer` trait by forwarding requests
//! to a remote ACP HTTP server. This allows the `AcpClient` to transparently
//! interact with both local and remote agents through the same interface.
//!
//! ## Usage
//!
//! ```ignore
//! // Connect to a remote agent
//! let remote = RemoteAcpServer::connect("https://agent.example.com").await?;
//!
//! // Use it like any other AcpServer
//! let request = TaskRequest::new("local-agent", &remote.card().name, "Analyze this");
//! let response = remote.handle_task(request).await?;
//! ```
//!
//! ## SSE Streaming
//!
//! `RemoteAcpServer` supports SSE streaming for real-time task events.
//! Use `handle_task_with_events()` to receive incremental progress updates.

use async_trait::async_trait;
use reqwest::Client;

use super::agent_card::AgentCard;
use super::server::{AcpServer, TaskEventCallback};
use super::transport::{ErrorResponse, TaskSubmitRequest, TaskSubmitResponse};
use super::types::{AcpError, AcpErrorCode, TaskEvent, TaskRequest, TaskResponse};

// ════════════════════════════════════════════════════════════
// RemoteAcpServer
// ════════════════════════════════════════════════════════════

/// An ACP server proxy that communicates with a remote agent over HTTP.
///
/// Implements the `AcpServer` trait, making remote agents indistinguishable
/// from local ones at the API level. The `AcpClient` can register both
/// local and remote servers in its registry.
pub struct RemoteAcpServer {
    /// The remote agent's card (fetched during `connect()`).
    card: AgentCard,
    /// Base URL of the remote ACP server (e.g., "https://agent.example.com").
    base_url: String,
    /// HTTP client for making requests.
    http_client: Client,
}

impl RemoteAcpServer {
    /// Connect to a remote ACP server and fetch its agent card.
    ///
    /// This performs agent discovery by fetching the card from the
    /// well-known URL or the agent-specific endpoint.
    ///
    /// # Arguments
    ///
    /// * `base_url` — The base URL of the remote ACP server.
    ///
    /// # Errors
    ///
    /// Returns an error if the remote server is unreachable or returns
    /// an invalid agent card.
    pub async fn connect(base_url: impl Into<String>) -> Result<Self, AcpError> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let http_client = Client::new();

        // Fetch the agent card from the well-known URL
        let card_url = format!("{}/.well-known/agent.json", base_url);
        tracing::info!(url = %card_url, "Connecting to remote ACP server");

        let response = http_client
            .get(&card_url)
            .send()
            .await
            .map_err(|e| {
                AcpError::new(
                    AcpErrorCode::InternalError,
                    format!("Failed to connect to remote agent at {}: {}", base_url, e),
                )
            })?;

        if !response.status().is_success() {
            return Err(AcpError::new(
                AcpErrorCode::AgentNotFound,
                format!(
                    "Remote agent at {} returned status {}",
                    base_url,
                    response.status()
                ),
            ));
        }

        let card: AgentCard = response.json().await.map_err(|e| {
            AcpError::new(
                AcpErrorCode::InternalError,
                format!("Failed to parse agent card from {}: {}", base_url, e),
            )
        })?;

        tracing::info!(
            agent = %card.name,
            url = %base_url,
            "Connected to remote ACP server"
        );

        Ok(Self {
            card,
            base_url,
            http_client,
        })
    }

    /// Connect to a specific named agent on a remote ACP server.
    ///
    /// Unlike `connect()`, this fetches the card from `/agents/:name`
    /// instead of the well-known URL.
    pub async fn connect_agent(
        base_url: impl Into<String>,
        agent_name: &str,
    ) -> Result<Self, AcpError> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let http_client = Client::new();

        let card_url = format!("{}/agents/{}", base_url, agent_name);
        tracing::info!(url = %card_url, agent = %agent_name, "Connecting to remote ACP agent");

        let response = http_client
            .get(&card_url)
            .send()
            .await
            .map_err(|e| {
                AcpError::new(
                    AcpErrorCode::InternalError,
                    format!("Failed to connect to remote agent '{}' at {}: {}", agent_name, base_url, e),
                )
            })?;

        if !response.status().is_success() {
            return Err(AcpError::agent_not_found(agent_name));
        }

        let card: AgentCard = response.json().await.map_err(|e| {
            AcpError::new(
                AcpErrorCode::InternalError,
                format!("Failed to parse agent card for '{}': {}", agent_name, e),
            )
        })?;

        Ok(Self {
            card,
            base_url,
            http_client,
        })
    }

    /// Create a `RemoteAcpServer` with a pre-fetched card.
    ///
    /// Useful when the card is already known (e.g., from a previous
    /// discovery call or configuration file).
    pub fn with_card(base_url: impl Into<String>, card: AgentCard) -> Self {
        Self {
            card,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            http_client: Client::new(),
        }
    }

    /// Return the base URL of the remote server.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Discover all agents on the remote server.
    pub async fn discover_all(&self) -> Result<Vec<AgentCard>, AcpError> {
        let url = format!("{}/agents", self.base_url);
        let response = self
            .http_client
            .get(&url)
            .send()
            .await
            .map_err(|e| AcpError::internal(format!("HTTP request failed: {}", e)))?;

        if !response.status().is_success() {
            return Err(AcpError::internal(format!(
                "Remote server returned status {}",
                response.status()
            )));
        }

        #[derive(serde::Deserialize)]
        struct ListResponse {
            agents: Vec<AgentCard>,
        }

        let list: ListResponse = response
            .json()
            .await
            .map_err(|e| AcpError::internal(format!("Failed to parse agent list: {}", e)))?;

        Ok(list.agents)
    }

    /// Build the task submission URL for this agent.
    fn task_url(&self) -> String {
        format!("{}/agents/{}/tasks", self.base_url, self.card.name)
    }

    /// Build the streaming task submission URL for this agent.
    fn stream_url(&self) -> String {
        format!("{}/agents/{}/tasks/stream", self.base_url, self.card.name)
    }
}

#[async_trait]
impl AcpServer for RemoteAcpServer {
    fn card(&self) -> &AgentCard {
        &self.card
    }

    /// Send a task to the remote agent via HTTP POST.
    async fn handle_task(&self, request: TaskRequest) -> Result<TaskResponse, AcpError> {
        let submit = TaskSubmitRequest {
            task: request.task.clone(),
            params: request.params.clone(),
            context: request.context.clone(),
            accept: request.accept.clone(),
            sender: request.metadata.sender.clone(),
        };

        tracing::info!(
            agent = %self.card.name,
            url = %self.base_url,
            task_id = %request.metadata.task_id,
            "Sending task to remote ACP server"
        );

        let response = self
            .http_client
            .post(&self.task_url())
            .json(&submit)
            .send()
            .await
            .map_err(|e| {
                AcpError::new(
                    AcpErrorCode::InternalError,
                    format!("HTTP request to remote agent '{}' failed: {}", self.card.name, e),
                )
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AcpError::new(
                AcpErrorCode::InternalError,
                format!(
                    "Remote agent '{}' returned HTTP {}: {}",
                    self.card.name, status, body
                ),
            ));
        }

        let result: TaskSubmitResponse = response.json().await.map_err(|e| {
            AcpError::new(
                AcpErrorCode::InternalError,
                format!("Failed to parse response from remote agent '{}': {}", self.card.name, e),
            )
        })?;

        if result.success {
            result.data.ok_or_else(|| {
                AcpError::internal("Remote agent returned success but no data")
            })
        } else {
            let err = result.error.unwrap_or(ErrorResponse {
                code: "unknown".to_string(),
                message: "Remote agent returned failure with no error details".to_string(),
                details: None,
            });
            Err(AcpError::new(
                AcpErrorCode::InternalError,
                format!("[{}] {}", err.code, err.message),
            ))
        }
    }

    /// Send a task to the remote agent with SSE streaming.
    ///
    /// Connects to the streaming endpoint and forwards `TaskEvent`s
    /// to the provided callback as they arrive.
    async fn handle_task_with_events(
        &self,
        request: TaskRequest,
        on_event: Option<&TaskEventCallback>,
    ) -> Result<TaskResponse, AcpError> {
        // If no event callback, fall back to non-streaming
        if on_event.is_none() {
            return self.handle_task(request).await;
        }

        let callback = on_event.unwrap();

        let submit = TaskSubmitRequest {
            task: request.task.clone(),
            params: request.params.clone(),
            context: request.context.clone(),
            accept: request.accept.clone(),
            sender: request.metadata.sender.clone(),
        };

        tracing::info!(
            agent = %self.card.name,
            url = %self.base_url,
            task_id = %request.metadata.task_id,
            "Sending streaming task to remote ACP server"
        );

        let response = self
            .http_client
            .post(&self.stream_url())
            .json(&submit)
            .send()
            .await
            .map_err(|e| {
                AcpError::new(
                    AcpErrorCode::InternalError,
                    format!("HTTP streaming request to '{}' failed: {}", self.card.name, e),
                )
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AcpError::new(
                AcpErrorCode::InternalError,
                format!(
                    "Remote agent '{}' streaming returned HTTP {}: {}",
                    self.card.name, status, body
                ),
            ));
        }

        // Parse the SSE stream incrementally (not buffer-all)
        let mut final_response: Option<TaskResponse> = None;
        let mut final_error: Option<AcpError> = None;
        let mut line_buf = String::new();

        use futures::StreamExt;
        let mut stream = response.bytes_stream();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                AcpError::internal(format!("Failed to read SSE stream chunk: {}", e))
            })?;
            let text = String::from_utf8_lossy(&chunk);
            line_buf.push_str(&text);

            // Process complete lines
            while let Some(newline_pos) = line_buf.find('\n') {
                let line = line_buf[..newline_pos].trim_end_matches('\r').to_string();
                line_buf = line_buf[newline_pos + 1..].to_string();

                if let Some(data) = line.strip_prefix("data: ") {
                    // Try to parse as TaskEvent first
                    if let Ok(event) = serde_json::from_str::<TaskEvent>(data) {
                        callback(event);
                        continue;
                    }

                    // Try to parse as TaskResponse
                    if let Ok(response) = serde_json::from_str::<TaskResponse>(data) {
                        final_response = Some(response);
                        continue;
                    }

                    // Try to parse as ErrorResponse
                    if let Ok(err) = serde_json::from_str::<ErrorResponse>(data) {
                        final_error = Some(AcpError::new(
                            AcpErrorCode::InternalError,
                            format!("[{}] {}", err.code, err.message),
                        ));
                    }
                }
            }
        }

        if let Some(err) = final_error {
            return Err(err);
        }

        final_response.ok_or_else(|| {
            AcpError::internal("SSE stream ended without a final task response")
        })
    }
}

// ════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::agent_card::AgentSkill;
    use super::super::types::TaskState;
    use std::sync::Arc;

    #[test]
    fn test_remote_server_with_card() {
        let card = AgentCard::new("remote-agent", "A remote agent")
            .with_skill(AgentSkill::new("analyze", "Analyze code"))
            .with_url("https://agent.example.com");

        let server = RemoteAcpServer::with_card("https://agent.example.com", card);
        assert_eq!(server.card().name, "remote-agent");
        assert_eq!(server.base_url(), "https://agent.example.com");
        assert!(server.card().is_remote());
    }

    #[test]
    fn test_remote_server_url_normalization() {
        let card = AgentCard::new("test", "Test");
        let server = RemoteAcpServer::with_card("https://example.com/", card);
        assert_eq!(server.base_url(), "https://example.com");
    }

    #[test]
    fn test_task_url_construction() {
        let card = AgentCard::new("my-agent", "My agent");
        let server = RemoteAcpServer::with_card("https://example.com", card);
        assert_eq!(
            server.task_url(),
            "https://example.com/agents/my-agent/tasks"
        );
        assert_eq!(
            server.stream_url(),
            "https://example.com/agents/my-agent/tasks/stream"
        );
    }

    /// Integration test: RemoteAcpServer against a local transport server.
    ///
    /// This test starts a real HTTP server with a mock agent, then uses
    /// RemoteAcpServer to communicate with it — validating the full
    /// HTTP round-trip.
    #[tokio::test]
    async fn test_remote_server_integration() {
        use super::super::transport::AcpServerRegistry;

        // Set up a local HTTP server with a mock agent
        struct MockAgent {
            card: AgentCard,
        }

        #[async_trait]
        impl AcpServer for MockAgent {
            fn card(&self) -> &AgentCard {
                &self.card
            }

            async fn handle_task(
                &self,
                request: TaskRequest,
            ) -> Result<TaskResponse, AcpError> {
                Ok(TaskResponse::completed(
                    &request,
                    &self.card.name,
                    format!("Remote echo: {}", request.task),
                ))
            }
        }

        let registry = AcpServerRegistry::new();
        let mock = Arc::new(MockAgent {
            card: AgentCard::new("echo-agent", "Echoes tasks back"),
        });
        registry.register(mock).await;
        registry.set_primary("echo-agent").await;

        let app = super::super::transport::acp_router(registry);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{}", addr);

        // Start the server in the background
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // Give the server a moment to start
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Connect via RemoteAcpServer
        let remote = RemoteAcpServer::connect(&base_url).await.unwrap();
        assert_eq!(remote.card().name, "echo-agent");

        // Send a task
        let request = TaskRequest::new("test-client", "echo-agent", "Hello, remote!");
        let response = remote.handle_task(request).await.unwrap();
        assert_eq!(response.state, TaskState::Completed);
        assert!(response.content.contains("Remote echo: Hello, remote!"));

        // Discover all agents
        let agents = remote.discover_all().await.unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "echo-agent");
    }
}
