//! ACP HTTP Transport — exposes `AcpServer` instances over HTTP.
//!
//! Phase 2 of the ACP protocol adds HTTP/SSE transport, allowing agents
//! to communicate across process boundaries. This module provides:
//!
//! - **REST endpoints** for task submission, discovery, and cancellation
//! - **SSE streaming** for real-time `TaskEvent` delivery
//! - **Agent registry** for routing requests to the correct `AcpServer`
//!
//! ## Endpoints
//!
//! | Method | Path                          | Description                    |
//! |--------|-------------------------------|--------------------------------|
//! | GET    | `/.well-known/agent.json`     | Agent card discovery           |
//! | GET    | `/agents`                     | List all registered agents     |
//! | GET    | `/agents/:name`               | Get a specific agent's card    |
//! | POST   | `/agents/:name/tasks`         | Submit a task (JSON response)  |
//! | POST   | `/agents/:name/tasks/stream`  | Submit a task (SSE streaming)  |
//! | POST   | `/agents/:name/tasks/cancel`  | Cancel a running task          |
//! | GET    | `/health`                     | Health check                   |
//!
//! ## Architecture
//!
//! ```text
//! HTTP Client ──► axum Router ──► AcpServerRegistry ──► AcpServer impl
//!                     │                                       │
//!                     │◄──────── SSE Stream ◄────────── TaskEvent
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio_stream::wrappers::ReceiverStream;

use super::agent_card::AgentCard;
use super::server::{AcpServer, TaskEventCallback};
use super::types::{AcpError, AcpErrorCode, TaskEvent, TaskRequest, TaskState};

// ════════════════════════════════════════════════════════════
// Server Registry (shared state for axum handlers)
// ════════════════════════════════════════════════════════════

/// Shared state for the ACP HTTP server.
///
/// Holds a registry of `AcpServer` instances that can be accessed
/// by axum request handlers. Thread-safe via `Arc<RwLock<...>>`.
#[derive(Clone)]
pub struct AcpServerRegistry {
    /// Registered ACP servers, keyed by agent name.
    servers: Arc<RwLock<HashMap<String, Arc<dyn AcpServer>>>>,
    /// Optional server-level metadata (e.g., the "primary" agent card
    /// returned at `/.well-known/agent.json`).
    primary_agent: Arc<RwLock<Option<String>>>,
}

impl AcpServerRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            servers: Arc::new(RwLock::new(HashMap::new())),
            primary_agent: Arc::new(RwLock::new(None)),
        }
    }

    /// Register an ACP server. Returns the agent name.
    pub async fn register(&self, server: Arc<dyn AcpServer>) -> String {
        let name = server.card().name.clone();
        tracing::info!(agent = %name, "Registered ACP server in HTTP registry");
        self.servers.write().await.insert(name.clone(), server);
        name
    }

    /// Set the primary agent (returned at `/.well-known/agent.json`).
    pub async fn set_primary(&self, agent_name: impl Into<String>) {
        *self.primary_agent.write().await = Some(agent_name.into());
    }

    /// Unregister an agent by name.
    pub async fn unregister(&self, agent_name: &str) -> bool {
        self.servers.write().await.remove(agent_name).is_some()
    }

    /// Get a server by name.
    async fn get(&self, agent_name: &str) -> Option<Arc<dyn AcpServer>> {
        self.servers.read().await.get(agent_name).cloned()
    }

    /// Get all agent cards.
    async fn all_cards(&self) -> Vec<AgentCard> {
        self.servers
            .read()
            .await
            .values()
            .map(|s| s.card().clone())
            .collect()
    }

    /// Get the primary agent's card.
    async fn primary_card(&self) -> Option<AgentCard> {
        let primary_name = self.primary_agent.read().await.clone();
        if let Some(name) = primary_name {
            self.get(&name).await.map(|s| s.card().clone())
        } else {
            // Fall back to the first registered agent
            let servers = self.servers.read().await;
            servers.values().next().map(|s| s.card().clone())
        }
    }
}

impl Default for AcpServerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ════════════════════════════════════════════════════════════
// HTTP Request/Response Types
// ════════════════════════════════════════════════════════════

/// HTTP request body for task submission.
#[derive(Debug, Deserialize, Serialize)]
pub struct TaskSubmitRequest {
    /// Natural-language task description.
    pub task: String,
    /// Optional structured input parameters.
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    /// Optional context from the parent conversation.
    #[serde(default)]
    pub context: Option<String>,
    /// Optional list of acceptable output MIME types.
    #[serde(default)]
    pub accept: Vec<String>,
    /// Optional sender name (defaults to "http-client").
    #[serde(default = "default_sender")]
    pub sender: String,
}

fn default_sender() -> String {
    "http-client".to_string()
}

/// HTTP request body for task cancellation.
#[derive(Debug, Deserialize)]
pub struct TaskCancelRequest {
    /// The task ID to cancel.
    pub task_id: String,
}

/// HTTP response for task completion.
#[derive(Debug, Serialize, Deserialize)]
pub struct TaskSubmitResponse {
    /// Whether the request was successful.
    pub success: bool,
    /// The task response (present on success).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<super::types::TaskResponse>,
    /// Error details (present on failure).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorResponse>,
}

/// HTTP error response body.
#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorResponse {
    /// Error code.
    pub code: String,
    /// Human-readable error message.
    pub message: String,
    /// Optional structured details.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl From<AcpError> for ErrorResponse {
    fn from(err: AcpError) -> Self {
        Self {
            code: err.code.to_string(),
            message: err.message,
            details: err.details,
        }
    }
}

/// HTTP response for agent listing.
#[derive(Debug, Serialize)]
pub struct AgentListResponse {
    /// List of agent cards.
    pub agents: Vec<AgentCard>,
    /// Total number of agents.
    pub count: usize,
}

// ════════════════════════════════════════════════════════════
// Router Builder
// ════════════════════════════════════════════════════════════

/// Build the ACP HTTP router with all endpoints.
///
/// The returned `Router` can be composed with other axum routers
/// or served directly.
///
/// ## Example
///
/// ```ignore
/// let registry = AcpServerRegistry::new();
/// registry.register(my_server).await;
///
/// let app = acp_router(registry);
/// let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
/// axum::serve(listener, app).await?;
/// ```
pub fn acp_router(registry: AcpServerRegistry) -> Router {
    Router::new()
        // Discovery endpoints
        .route("/.well-known/agent.json", get(handle_well_known))
        .route("/agents", get(handle_list_agents))
        .route("/agents/{name}", get(handle_get_agent))
        // Task endpoints
        .route("/agents/{name}/tasks", post(handle_submit_task))
        .route("/agents/{name}/tasks/stream", post(handle_submit_task_stream))
        .route("/agents/{name}/tasks/cancel", post(handle_cancel_task))
        // Health check
        .route("/health", get(handle_health))
        .with_state(registry)
}

/// Start the ACP HTTP server on the given address.
///
/// This is a convenience function that binds to the address and serves
/// the ACP router. It blocks until the server is shut down.
pub async fn serve(
    registry: AcpServerRegistry,
    addr: &str,
) -> anyhow::Result<()> {
    let app = acp_router(registry);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(addr = %addr, "ACP HTTP server listening");
    axum::serve(listener, app).await?;
    Ok(())
}

// ════════════════════════════════════════════════════════════
// Request Handlers
// ════════════════════════════════════════════════════════════

/// GET `/.well-known/agent.json` — return the primary agent's card.
///
/// This follows the A2A convention for agent discovery via a well-known URL.
async fn handle_well_known(
    State(registry): State<AcpServerRegistry>,
) -> Response {
    match registry.primary_card().await {
        Some(card) => Json(card).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                code: "no_primary_agent".to_string(),
                message: "No primary agent configured".to_string(),
                details: None,
            }),
        )
            .into_response(),
    }
}

/// GET `/agents` — list all registered agents.
async fn handle_list_agents(
    State(registry): State<AcpServerRegistry>,
) -> Json<AgentListResponse> {
    let agents = registry.all_cards().await;
    let count = agents.len();
    Json(AgentListResponse { agents, count })
}

/// GET `/agents/:name` — get a specific agent's card.
async fn handle_get_agent(
    State(registry): State<AcpServerRegistry>,
    Path(name): Path<String>,
) -> Response {
    match registry.get(&name).await {
        Some(server) => Json(server.card().clone()).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                code: AcpErrorCode::AgentNotFound.to_string(),
                message: format!("Agent '{}' not found", name),
                details: None,
            }),
        )
            .into_response(),
    }
}

/// POST `/agents/:name/tasks` — submit a task and wait for completion.
///
/// Returns the full `TaskResponse` as JSON when the task completes.
async fn handle_submit_task(
    State(registry): State<AcpServerRegistry>,
    Path(name): Path<String>,
    Json(body): Json<TaskSubmitRequest>,
) -> Response {
    let server = match registry.get(&name).await {
        Some(s) => s,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(TaskSubmitResponse {
                    success: false,
                    data: None,
                    error: Some(ErrorResponse::from(AcpError::agent_not_found(&name))),
                }),
            )
                .into_response();
        }
    };

    let mut request = TaskRequest::new(&body.sender, &name, &body.task);
    if let Some(params) = body.params {
        request = request.with_params(params);
    }
    if let Some(context) = body.context {
        request = request.with_context(context);
    }
    if !body.accept.is_empty() {
        request = request.with_accept(body.accept);
    }

    tracing::info!(
        agent = %name,
        task_id = %request.metadata.task_id,
        sender = %body.sender,
        "HTTP task submission"
    );

    match server.handle_task(request).await {
        Ok(response) => Json(TaskSubmitResponse {
            success: true,
            data: Some(response),
            error: None,
        })
        .into_response(),
        Err(err) => {
            let status = error_to_status(&err);
            (
                status,
                Json(TaskSubmitResponse {
                    success: false,
                    data: None,
                    error: Some(ErrorResponse::from(err)),
                }),
            )
                .into_response()
        }
    }
}

/// POST `/agents/:name/tasks/stream` — submit a task with SSE streaming.
///
/// Returns a Server-Sent Events stream that emits:
/// - `event: task_event` — incremental `TaskEvent` payloads
/// - `event: task_response` — the final `TaskResponse`
/// - `event: error` — if the task fails
///
/// The stream closes after the final event.
async fn handle_submit_task_stream(
    State(registry): State<AcpServerRegistry>,
    Path(name): Path<String>,
    Json(body): Json<TaskSubmitRequest>,
) -> Response {
    let server = match registry.get(&name).await {
        Some(s) => s,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    code: AcpErrorCode::AgentNotFound.to_string(),
                    message: format!("Agent '{}' not found", name),
                    details: None,
                }),
            )
                .into_response();
        }
    };

    let mut request = TaskRequest::new(&body.sender, &name, &body.task);
    if let Some(params) = body.params {
        request = request.with_params(params);
    }
    if let Some(context) = body.context {
        request = request.with_context(context);
    }
    if !body.accept.is_empty() {
        request = request.with_accept(body.accept);
    }

    let task_id = request.metadata.task_id.clone();

    tracing::info!(
        agent = %name,
        task_id = %task_id,
        sender = %body.sender,
        "HTTP streaming task submission"
    );

    // Create a channel for SSE events
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(64);

    // Build the event callback that forwards TaskEvents to the SSE channel
    let tx_events = tx.clone();
    let event_callback: TaskEventCallback = Arc::new(move |event: TaskEvent| {
        let tx = tx_events.clone();
        let json = serde_json::to_string(&event).unwrap_or_default();
        // Non-blocking send — drop events if the channel is full
        let _ = tx.try_send(Ok(Event::default().event("task_event").data(json)));
    });

    // Spawn the task processing in a background task
    let tx_final = tx;
    tokio::spawn(async move {
        let result = server
            .handle_task_with_events(request, Some(&event_callback))
            .await;

        match result {
            Ok(response) => {
                let json = serde_json::to_string(&response).unwrap_or_default();
                let _ = tx_final
                    .send(Ok(Event::default().event("task_response").data(json)))
                    .await;
            }
            Err(err) => {
                let json = serde_json::to_string(&ErrorResponse::from(err)).unwrap_or_default();
                let _ = tx_final
                    .send(Ok(Event::default().event("error").data(json)))
                    .await;
            }
        }
        // Channel drops here, closing the SSE stream
    });

    let stream = ReceiverStream::new(rx);
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// POST `/agents/:name/tasks/cancel` — cancel a running task.
///
/// Note: Task cancellation requires the server to support it.
/// In Phase 2, this is a best-effort operation.
async fn handle_cancel_task(
    State(registry): State<AcpServerRegistry>,
    Path(name): Path<String>,
    Json(body): Json<TaskCancelRequest>,
) -> Response {
    let _server = match registry.get(&name).await {
        Some(s) => s,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    code: AcpErrorCode::AgentNotFound.to_string(),
                    message: format!("Agent '{}' not found", name),
                    details: None,
                }),
            )
                .into_response();
        }
    };

    tracing::info!(
        agent = %name,
        task_id = %body.task_id,
        "HTTP task cancellation request (not yet implemented)"
    );

    // Phase 2: cancellation is not yet enforced.
    // Return 501 to honestly indicate the operation is not implemented,
    // rather than returning success which misleads callers.
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "success": false,
            "task_id": body.task_id,
            "state": TaskState::Canceled.to_string(),
            "message": "Task cancellation is not yet implemented (Phase 2)"
        })),
    )
    .into_response()
}

/// GET `/health` — health check endpoint.
async fn handle_health(
    State(registry): State<AcpServerRegistry>,
) -> Json<serde_json::Value> {
    let agents = registry.all_cards().await;
    Json(serde_json::json!({
        "status": "ok",
        "protocol": "acp",
        "version": "0.2.0",
        "agents": agents.len(),
    }))
}

// ════════════════════════════════════════════════════════════
// Helpers
// ════════════════════════════════════════════════════════════

/// Map an `AcpError` to an HTTP status code.
fn error_to_status(err: &AcpError) -> StatusCode {
    match err.code {
        AcpErrorCode::InvalidRequest => StatusCode::BAD_REQUEST,
        AcpErrorCode::AgentNotFound => StatusCode::NOT_FOUND,
        AcpErrorCode::UnsupportedCapability => StatusCode::NOT_IMPLEMENTED,
        AcpErrorCode::TaskRejected => StatusCode::TOO_MANY_REQUESTS,
        AcpErrorCode::Timeout => StatusCode::GATEWAY_TIMEOUT,
        AcpErrorCode::InternalError => StatusCode::INTERNAL_SERVER_ERROR,
        AcpErrorCode::Canceled => StatusCode::CONFLICT,
        AcpErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
    }
}

// ════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::agent_card::AgentSkill;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt; // for `oneshot`

    /// A mock ACP server for transport testing.
    struct MockTransportServer {
        card: AgentCard,
    }

    impl MockTransportServer {
        fn new(name: &str) -> Self {
            Self {
                card: AgentCard::new(name, format!("Mock agent: {}", name))
                    .with_skill(AgentSkill::new("test", "A test skill")),
            }
        }
    }

    #[async_trait::async_trait]
    impl AcpServer for MockTransportServer {
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
                format!("HTTP mock response to: {}", request.task),
            ))
        }
    }

    async fn setup_app() -> (Router, AcpServerRegistry) {
        let registry = AcpServerRegistry::new();
        let server = Arc::new(MockTransportServer::new("test-agent"));
        registry.register(server).await;
        registry.set_primary("test-agent").await;
        let app = acp_router(registry.clone());
        (app, registry)
    }

    #[tokio::test]
    async fn test_health_endpoint() {
        let (app, _) = setup_app().await;
        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["agents"], 1);
    }

    #[tokio::test]
    async fn test_well_known_endpoint() {
        let (app, _) = setup_app().await;
        let req = Request::builder()
            .uri("/.well-known/agent.json")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["name"], "test-agent");
    }

    #[tokio::test]
    async fn test_list_agents() {
        let (app, _) = setup_app().await;
        let req = Request::builder()
            .uri("/agents")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["count"], 1);
    }

    #[tokio::test]
    async fn test_get_agent() {
        let (app, _) = setup_app().await;
        let req = Request::builder()
            .uri("/agents/test-agent")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["name"], "test-agent");
    }

    #[tokio::test]
    async fn test_get_agent_not_found() {
        let (app, _) = setup_app().await;
        let req = Request::builder()
            .uri("/agents/nonexistent")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_submit_task() {
        let (app, _) = setup_app().await;
        let body = serde_json::json!({
            "task": "Review this code",
            "sender": "test-client"
        });
        let req = Request::builder()
            .method("POST")
            .uri("/agents/test-agent/tasks")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["success"], true);
        assert!(json["data"]["content"]
            .as_str()
            .unwrap()
            .contains("Review this code"));
    }

    #[tokio::test]
    async fn test_submit_task_agent_not_found() {
        let (app, _) = setup_app().await;
        let body = serde_json::json!({
            "task": "Do something"
        });
        let req = Request::builder()
            .method("POST")
            .uri("/agents/nonexistent/tasks")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_cancel_task() {
        let (app, _) = setup_app().await;
        let body = serde_json::json!({
            "task_id": "test-task-123"
        });
        let req = Request::builder()
            .method("POST")
            .uri("/agents/test-agent/tasks/cancel")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn test_error_to_status_mapping() {
        assert_eq!(
            error_to_status(&AcpError::invalid_request("bad")),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            error_to_status(&AcpError::agent_not_found("x")),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            error_to_status(&AcpError::internal("oops")),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
