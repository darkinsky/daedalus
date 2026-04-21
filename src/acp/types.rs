//! ACP protocol message types — the lingua franca of inter-agent communication.
//!
//! All messages exchanged between agents flow through these types. The design
//! follows a request/response pattern with support for streaming events,
//! mirroring the A2A protocol's task lifecycle.
//!
//! ## Message Flow
//!
//! ```text
//! Client                          Server
//!   │                               │
//!   │── TaskRequest ──────────────►│
//!   │                               │── (processing)
//!   │◄── TaskEvent::Progress ──────│  (optional, streaming)
//!   │◄── TaskEvent::Progress ──────│  (optional, streaming)
//!   │◄── TaskEvent::Artifact ──────│  (optional, streaming)
//!   │◄── TaskResponse ────────────│  (final)
//!   │                               │
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ════════════════════════════════════════════════════════════
// Message Metadata
// ════════════════════════════════════════════════════════════

/// Metadata attached to every ACP message for tracing and correlation.
///
/// Inspired by OpenTelemetry context propagation — every message carries
/// enough information to reconstruct the full request chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageMetadata {
    /// Unique message identifier (UUID v4).
    pub message_id: String,
    /// Task identifier — all messages in a task lifecycle share this ID.
    /// The client generates it with the initial `TaskRequest`.
    pub task_id: String,
    /// Timestamp when the message was created.
    pub timestamp: DateTime<Utc>,
    /// The agent that sent this message (agent card name).
    pub sender: String,
    /// The intended recipient agent (agent card name).
    pub recipient: String,
    /// Optional correlation ID for linking related tasks across agents.
    /// Used when a task spawns sub-tasks on other agents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

impl MessageMetadata {
    /// Create new metadata for an outgoing message.
    pub fn new(task_id: impl Into<String>, sender: impl Into<String>, recipient: impl Into<String>) -> Self {
        Self {
            message_id: Uuid::new_v4().to_string(),
            task_id: task_id.into(),
            timestamp: Utc::now(),
            sender: sender.into(),
            recipient: recipient.into(),
            correlation_id: None,
        }
    }

    /// Create metadata with a new task ID (for initiating a new task).
    pub fn new_task(sender: impl Into<String>, recipient: impl Into<String>) -> Self {
        Self::new(Uuid::new_v4().to_string(), sender, recipient)
    }

    /// Attach a correlation ID for cross-agent task linking.
    pub fn with_correlation(mut self, correlation_id: impl Into<String>) -> Self {
        self.correlation_id = Some(correlation_id.into());
        self
    }
}

// ════════════════════════════════════════════════════════════
// Task State Machine
// ════════════════════════════════════════════════════════════

/// The lifecycle state of a task, following a strict state machine.
///
/// ```text
/// ┌──────────┐     ┌────────────┐     ┌───────────┐
/// │ Submitted │────►│ Processing │────►│ Completed │
/// └──────────┘     └────────────┘     └───────────┘
///       │               │                    │
///       │               ▼                    │
///       │          ┌─────────┐               │
///       └─────────►│ Failed  │◄──────────────┘
///                  └─────────┘
///                       │
///                       ▼
///                  ┌──────────┐
///                  │ Canceled │
///                  └──────────┘
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Task has been received but not yet started.
    Submitted,
    /// Task is actively being processed by the agent.
    Processing,
    /// Task completed successfully.
    Completed,
    /// Task failed with an error.
    Failed,
    /// Task was canceled by the client or server.
    Canceled,
}

impl std::fmt::Display for TaskState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Submitted => write!(f, "submitted"),
            Self::Processing => write!(f, "processing"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Canceled => write!(f, "canceled"),
        }
    }
}

// ════════════════════════════════════════════════════════════
// Task Artifacts
// ════════════════════════════════════════════════════════════

/// An artifact produced by a task — a discrete unit of output.
///
/// Artifacts represent structured outputs beyond plain text: files created,
/// code generated, data extracted, etc. They are delivered either inline
/// in the final `TaskResponse` or incrementally via `TaskEvent::Artifact`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    /// Human-readable name for this artifact (e.g., "generated_code.rs").
    pub name: String,
    /// MIME type of the artifact content (e.g., "text/plain", "application/json").
    #[serde(default = "default_mime_type")]
    pub mime_type: String,
    /// The artifact content (text-based).
    pub content: String,
    /// Optional description of what this artifact represents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

fn default_mime_type() -> String {
    "text/plain".to_string()
}

impl Artifact {
    /// Create a plain text artifact.
    pub fn text(name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            mime_type: "text/plain".to_string(),
            content: content.into(),
            description: None,
        }
    }

    /// Create a JSON artifact.
    pub fn json(name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            mime_type: "application/json".to_string(),
            content: content.into(),
            description: None,
        }
    }

    /// Attach a description to this artifact.
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }
}

// ════════════════════════════════════════════════════════════
// Core Message Types
// ════════════════════════════════════════════════════════════

/// A task request sent from a client to a server agent.
///
/// This is the primary way to ask an agent to do something. The request
/// contains a natural-language task description plus optional structured
/// parameters and context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRequest {
    /// Protocol metadata (IDs, timestamps, routing).
    pub metadata: MessageMetadata,
    /// Natural-language description of the task to perform.
    pub task: String,
    /// Optional structured input parameters (tool-call style).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    /// Optional context from the parent conversation or task chain.
    /// Allows the server agent to understand the broader context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Optional list of acceptable output MIME types.
    /// The server should prefer these formats when producing artifacts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accept: Vec<String>,
}

impl TaskRequest {
    /// Create a new task request with minimal parameters.
    pub fn new(
        sender: impl Into<String>,
        recipient: impl Into<String>,
        task: impl Into<String>,
    ) -> Self {
        Self {
            metadata: MessageMetadata::new_task(sender, recipient),
            task: task.into(),
            params: None,
            context: None,
            accept: vec![],
        }
    }

    /// Attach structured parameters to the request.
    pub fn with_params(mut self, params: serde_json::Value) -> Self {
        self.params = Some(params);
        self
    }

    /// Attach context from the parent conversation.
    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
    }

    /// Specify acceptable output MIME types.
    pub fn with_accept(mut self, accept: Vec<String>) -> Self {
        self.accept = accept;
        self
    }
}

/// A task response — the final result of a completed (or failed) task.
///
/// Sent by the server agent when the task reaches a terminal state
/// (Completed, Failed, or Canceled).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResponse {
    /// Protocol metadata (IDs, timestamps, routing).
    pub metadata: MessageMetadata,
    /// Final state of the task.
    pub state: TaskState,
    /// The primary text content of the response.
    pub content: String,
    /// Artifacts produced by the task (files, structured data, etc.).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<Artifact>,
    /// Token usage statistics (if the task involved LLM calls).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<TaskUsage>,
    /// Error details (populated when `state` is `Failed`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<AcpError>,
}

impl TaskResponse {
    /// Create a successful task response.
    pub fn completed(
        request: &TaskRequest,
        sender: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            metadata: MessageMetadata::new(
                &request.metadata.task_id,
                sender,
                &request.metadata.sender,
            ),
            state: TaskState::Completed,
            content: content.into(),
            artifacts: vec![],
            usage: None,
            error: None,
        }
    }

    /// Create a failed task response.
    pub fn failed(
        request: &TaskRequest,
        sender: impl Into<String>,
        error: AcpError,
    ) -> Self {
        Self {
            metadata: MessageMetadata::new(
                &request.metadata.task_id,
                sender,
                &request.metadata.sender,
            ),
            state: TaskState::Failed,
            content: String::new(),
            artifacts: vec![],
            usage: None,
            error: Some(error),
        }
    }

    /// Attach artifacts to the response.
    pub fn with_artifacts(mut self, artifacts: Vec<Artifact>) -> Self {
        self.artifacts = artifacts;
        self
    }

    /// Attach token usage statistics.
    pub fn with_usage(mut self, usage: TaskUsage) -> Self {
        self.usage = Some(usage);
        self
    }
}

/// Streaming events emitted during task processing.
///
/// These events allow the client to observe progress in real time.
/// In Phase 1, they are delivered via in-process callbacks.
/// Phase 2 will add SSE transport for remote streaming.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskEvent {
    /// Task state has changed.
    StateChange {
        /// The new state.
        state: TaskState,
        /// Optional human-readable message about the transition.
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// Incremental progress update (e.g., "Analyzing file 3/10...").
    Progress {
        /// Progress description.
        message: String,
        /// Optional progress percentage (0.0 to 1.0).
        #[serde(skip_serializing_if = "Option::is_none")]
        progress: Option<f64>,
    },
    /// An artifact has been produced (streamed incrementally).
    Artifact {
        /// The artifact data.
        artifact: Artifact,
    },
    /// A log/debug message from the agent (for observability).
    Log {
        /// Log level (info, warn, error, debug).
        level: String,
        /// Log message content.
        message: String,
    },
}

// ════════════════════════════════════════════════════════════
// Envelope Types
// ════════════════════════════════════════════════════════════

/// Top-level ACP message envelope — wraps all message types.
///
/// This enum is the single entry point for serialization/deserialization
/// of ACP messages, making it easy to route messages by type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AcpMessage {
    /// A task request from client to server.
    TaskRequest(TaskRequest),
    /// A task response from server to client.
    TaskResponse(TaskResponse),
    /// A streaming event during task processing.
    TaskEvent {
        /// Task ID this event belongs to.
        task_id: String,
        /// The event payload.
        event: TaskEvent,
    },
    /// Request an agent's card (capability discovery).
    DiscoverRequest {
        /// Protocol metadata.
        metadata: MessageMetadata,
    },
    /// Response with the agent's card.
    DiscoverResponse {
        /// Protocol metadata.
        metadata: MessageMetadata,
        /// The agent's capability card.
        card: super::agent_card::AgentCard,
    },
    /// Cancel a running task.
    CancelRequest {
        /// Protocol metadata (task_id identifies the task to cancel).
        metadata: MessageMetadata,
    },
}

/// Top-level ACP response — the result of processing any AcpMessage.
///
/// This is a convenience wrapper for the common request/response pattern.
/// For streaming scenarios, use `TaskEvent` directly.
#[derive(Debug, Clone)]
pub enum AcpResponse {
    /// A task completed (or failed) with a final response.
    Task(TaskResponse),
    /// Agent card discovery result.
    Card(super::agent_card::AgentCard),
    /// Task was canceled.
    Canceled { task_id: String },
    /// Protocol-level error (not a task failure).
    Error(AcpError),
}

// ════════════════════════════════════════════════════════════
// Error Types
// ════════════════════════════════════════════════════════════

/// ACP error codes — categorized protocol and application errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpErrorCode {
    /// The request was malformed or missing required fields.
    InvalidRequest,
    /// The requested agent was not found.
    AgentNotFound,
    /// The agent does not support the requested capability.
    UnsupportedCapability,
    /// The task was rejected (e.g., rate limit, policy).
    TaskRejected,
    /// The task timed out.
    Timeout,
    /// An internal error occurred in the agent.
    InternalError,
    /// The task was canceled.
    Canceled,
    /// Authentication/authorization failure.
    Unauthorized,
}

impl std::fmt::Display for AcpErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest => write!(f, "invalid_request"),
            Self::AgentNotFound => write!(f, "agent_not_found"),
            Self::UnsupportedCapability => write!(f, "unsupported_capability"),
            Self::TaskRejected => write!(f, "task_rejected"),
            Self::Timeout => write!(f, "timeout"),
            Self::InternalError => write!(f, "internal_error"),
            Self::Canceled => write!(f, "canceled"),
            Self::Unauthorized => write!(f, "unauthorized"),
        }
    }
}

/// A structured ACP error with code, message, and optional details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpError {
    /// Error category code.
    pub code: AcpErrorCode,
    /// Human-readable error message.
    pub message: String,
    /// Optional structured error details (for debugging).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl AcpError {
    /// Create a new ACP error.
    pub fn new(code: AcpErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    /// Attach structured details to the error.
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    /// Convenience: create an internal error.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(AcpErrorCode::InternalError, message)
    }

    /// Convenience: create an agent-not-found error.
    pub fn agent_not_found(agent_name: impl Into<String>) -> Self {
        let name = agent_name.into();
        Self::new(AcpErrorCode::AgentNotFound, format!("Agent '{}' not found", name))
    }

    /// Convenience: create an invalid-request error.
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(AcpErrorCode::InvalidRequest, message)
    }
}

impl std::fmt::Display for AcpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for AcpError {}

// ════════════════════════════════════════════════════════════
// Token Usage (ACP-level)
// ════════════════════════════════════════════════════════════

/// Token usage statistics for an ACP task.
///
/// Mirrors `crate::llm::TokenUsage` but is ACP-specific to avoid
/// coupling the protocol layer to the LLM layer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskUsage {
    /// Number of prompt/input tokens consumed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    /// Number of completion/output tokens generated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    /// Total tokens (prompt + completion).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    /// Number of tool-calling rounds executed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_rounds: Option<usize>,
}

impl From<&crate::llm::TokenUsage> for TaskUsage {
    fn from(usage: &crate::llm::TokenUsage) -> Self {
        Self {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
            tool_rounds: None,
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
    fn test_task_request_creation() {
        let req = TaskRequest::new("client-agent", "server-agent", "Analyze this code");
        assert_eq!(req.metadata.sender, "client-agent");
        assert_eq!(req.metadata.recipient, "server-agent");
        assert_eq!(req.task, "Analyze this code");
        assert!(!req.metadata.task_id.is_empty());
        assert!(!req.metadata.message_id.is_empty());
    }

    #[test]
    fn test_task_response_completed() {
        let req = TaskRequest::new("client", "server", "Do something");
        let resp = TaskResponse::completed(&req, "server", "Done!");
        assert_eq!(resp.state, TaskState::Completed);
        assert_eq!(resp.content, "Done!");
        assert_eq!(resp.metadata.task_id, req.metadata.task_id);
        assert_eq!(resp.metadata.sender, "server");
        assert_eq!(resp.metadata.recipient, "client");
        assert!(resp.error.is_none());
    }

    #[test]
    fn test_task_response_failed() {
        let req = TaskRequest::new("client", "server", "Do something");
        let error = AcpError::internal("Something went wrong");
        let resp = TaskResponse::failed(&req, "server", error);
        assert_eq!(resp.state, TaskState::Failed);
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, AcpErrorCode::InternalError);
    }

    #[test]
    fn test_artifact_creation() {
        let artifact = Artifact::text("output.txt", "Hello, world!")
            .with_description("Test output");
        assert_eq!(artifact.name, "output.txt");
        assert_eq!(artifact.mime_type, "text/plain");
        assert_eq!(artifact.content, "Hello, world!");
        assert_eq!(artifact.description, Some("Test output".to_string()));
    }

    #[test]
    fn test_acp_error_display() {
        let err = AcpError::agent_not_found("code-reviewer");
        assert_eq!(err.to_string(), "[agent_not_found] Agent 'code-reviewer' not found");
    }

    #[test]
    fn test_task_state_display() {
        assert_eq!(TaskState::Submitted.to_string(), "submitted");
        assert_eq!(TaskState::Processing.to_string(), "processing");
        assert_eq!(TaskState::Completed.to_string(), "completed");
        assert_eq!(TaskState::Failed.to_string(), "failed");
        assert_eq!(TaskState::Canceled.to_string(), "canceled");
    }

    #[test]
    fn test_message_metadata_correlation() {
        let meta = MessageMetadata::new_task("sender", "recipient")
            .with_correlation("parent-task-123");
        assert_eq!(meta.correlation_id, Some("parent-task-123".to_string()));
    }

    #[test]
    fn test_task_request_with_params() {
        let req = TaskRequest::new("client", "server", "Review code")
            .with_params(serde_json::json!({"file": "main.rs"}))
            .with_context("User is working on a Rust project");
        assert!(req.params.is_some());
        assert_eq!(req.context, Some("User is working on a Rust project".to_string()));
    }

    #[test]
    fn test_task_usage_from_llm_usage() {
        let llm_usage = crate::llm::TokenUsage {
            prompt_tokens: Some(100),
            completion_tokens: Some(50),
            total_tokens: Some(150),
            cached_tokens: None,
        };
        let task_usage = TaskUsage::from(&llm_usage);
        assert_eq!(task_usage.prompt_tokens, Some(100));
        assert_eq!(task_usage.completion_tokens, Some(50));
        assert_eq!(task_usage.total_tokens, Some(150));
        assert!(task_usage.tool_rounds.is_none());
    }

    #[test]
    fn test_acp_message_serialization() {
        let req = TaskRequest::new("client", "server", "Hello");
        let msg = AcpMessage::TaskRequest(req);
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("task_request"));
        assert!(json.contains("Hello"));
    }
}
