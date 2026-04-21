//! ACP Tool — exposes ACP agents as a built-in tool for LLM routing.
//!
//! This module provides `AcpTool`, a `BuiltinTool` implementation that allows
//! the LLM to call remote or local ACP agents through the standard tool-calling
//! interface. It is the ACP equivalent of `SubagentTool` (`spawn_subagent`).
//!
//! ## How it works
//!
//! 1. The LLM sees `call_acp_agent` in its tool list with descriptions of
//!    all registered ACP agents.
//! 2. When the LLM decides to delegate a task, it calls `call_acp_agent`
//!    with `agent_name` and `task` parameters.
//! 3. `AcpTool` routes the request through `AcpClient` to the appropriate
//!    `AcpServer` (local or remote).
//! 4. The response is formatted and returned to the LLM.
//!
//! ## Difference from `spawn_subagent`
//!
//! | Feature | `spawn_subagent` | `call_acp_agent` |
//! |---------|-----------------|------------------|
//! | Protocol | Internal (SubagentRunner) | ACP (standardized) |
//! | Transport | In-process only | Local + HTTP/SSE |
//! | Discovery | Static (filesystem) | Dynamic (AgentCard) |
//! | Streaming | Via ToolEvent callback | Via TaskEvent + SSE |

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::tools::BuiltinTool;

use super::client::AcpClient;
use super::agent_card::AgentCard;

/// The tool name exposed to the LLM.
const ACP_TOOL_NAME: &str = "call_acp_agent";

// ════════════════════════════════════════════════════════════
// AcpTool — BuiltinTool adapter for ACP agents
// ════════════════════════════════════════════════════════════

/// A `BuiltinTool` that routes tasks to ACP agents via `AcpClient`.
///
/// Registered into the `ToolRouter` during bootstrap, making ACP agents
/// available to the LLM through the standard tool-calling interface.
pub struct AcpTool {
    /// The ACP client (shared, thread-safe).
    client: Arc<RwLock<AcpClient>>,
    /// Cached agent cards for tool definition generation.
    agent_cards: Vec<AgentCard>,
}

impl AcpTool {
    /// Create a new ACP tool with the given client and pre-fetched agent cards.
    pub fn new(client: Arc<RwLock<AcpClient>>, agent_cards: Vec<AgentCard>) -> Self {
        Self {
            client,
            agent_cards,
        }
    }

    /// Build the tool description including a catalog of available ACP agents.
    fn build_description(&self) -> String {
        let agent_list: Vec<String> = self
            .agent_cards
            .iter()
            .map(|card| {
                let location = if card.is_remote() { "remote" } else { "local" };
                format!("  - **{}** [{}]: {}", card.name, location, card.description)
            })
            .collect();

        format!(
            "Call an ACP (Agent Communication Protocol) agent to handle a task. \
             ACP agents are specialized services that can be local or remote, \
             each with their own capabilities and tools.\n\n\
             Available ACP agents:\n{}\n\n\
             Use this tool when:\n\
             - You need to delegate a task to a specialized agent\n\
             - The task requires capabilities not available in your current tool set\n\
             - You want to leverage a remote agent's expertise\n\n\
             The agent will process the task independently and return the result.",
            agent_list.join("\n")
        )
    }
}

#[async_trait]
impl BuiltinTool for AcpTool {
    fn name(&self) -> &str {
        ACP_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Call an ACP agent to handle a specialized task (local or remote)"
    }

    fn input_schema(&self) -> serde_json::Value {
        let agent_names: Vec<serde_json::Value> = self
            .agent_cards
            .iter()
            .map(|card| serde_json::Value::String(card.name.clone()))
            .collect();

        serde_json::json!({
            "type": "object",
            "properties": {
                "agent_name": {
                    "type": "string",
                    "description": "The name of the ACP agent to call. Must be one of the available agents.",
                    "enum": agent_names,
                },
                "task": {
                    "type": "string",
                    "description": "A clear, self-contained description of the task for the agent. Include all necessary context.",
                },
                "context": {
                    "type": "string",
                    "description": "Optional additional context from the current conversation to help the agent understand the broader situation.",
                }
            },
            "required": ["agent_name", "task"],
        })
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String> {
        let agent_name = arguments
            .get("agent_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: agent_name"))?;

        let task = arguments
            .get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: task"))?;

        let context = arguments
            .get("context")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        tracing::info!(
            agent = %agent_name,
            task_len = task.len(),
            has_context = context.is_some(),
            "LLM invoked call_acp_agent"
        );

        // Build the request with optional context
        let client = self.client.read().await;

        let result = if let Some(ctx) = context {
            let mut request = super::types::TaskRequest::new(
                "daedalus",
                agent_name,
                task,
            );
            request = request.with_context(ctx);
            client.send_request(request).await
        } else {
            client.send_task(agent_name, task).await
        };

        match result {
            Ok(response) => {
                let usage_info = response
                    .usage
                    .as_ref()
                    .map(|u| {
                        format!(
                            "{} tokens, {} tool rounds",
                            u.total_tokens.unwrap_or(0),
                            u.tool_rounds.unwrap_or(0)
                        )
                    })
                    .unwrap_or_else(|| "unknown usage".to_string());

                let location = self
                    .agent_cards
                    .iter()
                    .find(|c| c.name == agent_name)
                    .map(|c| if c.is_remote() { "remote" } else { "local" })
                    .unwrap_or("unknown");

                Ok(format!(
                    "[ACP Agent '{}' ({}) completed — {}]\n\n{}",
                    agent_name, location, usage_info, response.content
                ))
            }
            Err(err) => {
                tracing::error!(
                    agent = %agent_name,
                    error = %err,
                    "ACP agent call failed"
                );
                Ok(format!(
                    "[ACP Agent '{}' failed: {}]",
                    agent_name, err.message
                ))
            }
        }
    }

    /// Override the default `to_openai_json` to use the rich description
    /// with the agent catalog.
    fn to_openai_json(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name(),
                "description": self.build_description(),
                "parameters": self.input_schema(),
            }
        })
    }
}

// ════════════════════════════════════════════════════════════
// ACP Configuration
// ════════════════════════════════════════════════════════════

/// ACP configuration section in the YAML config file.
///
/// Defines which ACP agents are available to the main agent.
///
/// ```yaml
/// acp:
///   enabled: true
///   agents:
///     - name: "code-analyzer"
///       url: "https://analyzer.example.com"
///     - name: "doc-writer"
///       url: "https://docs.example.com"
/// ```
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct AcpConfig {
    /// Whether ACP agent integration is enabled.
    pub enabled: bool,
    /// List of remote ACP agents to connect to at startup.
    #[serde(default)]
    pub agents: Vec<AcpAgentEntry>,
}

/// A single ACP agent entry in the configuration.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AcpAgentEntry {
    /// Agent name (used for routing). If not specified, fetched from the card.
    #[serde(default)]
    pub name: Option<String>,
    /// Base URL of the remote ACP server.
    pub url: String,
}

impl AcpConfig {
    /// Return true if ACP is enabled and has agents configured.
    pub fn has_agents(&self) -> bool {
        self.enabled && !self.agents.is_empty()
    }
}

// ════════════════════════════════════════════════════════════
// Bootstrap Helper
// ════════════════════════════════════════════════════════════

/// Initialize the ACP client from configuration.
///
/// Connects to all configured remote agents and returns the client
/// with all agents registered. Returns `None` if ACP is disabled
/// or no agents are configured.
///
/// This function is called during bootstrap (after tracing is initialized)
/// and the resulting client is installed into the ToolRouter.
pub async fn init_acp_client(config: &AcpConfig) -> Option<(Arc<RwLock<AcpClient>>, Vec<AgentCard>)> {
    if !config.enabled {
        return None;
    }

    if config.agents.is_empty() {
        tracing::debug!("ACP enabled but no agents configured");
        return None;
    }

    let mut client = AcpClient::new("daedalus");
    let mut cards = Vec::new();

    for entry in &config.agents {
        let result = if let Some(ref name) = entry.name {
            // Connect to a specific named agent
            tracing::info!(
                agent = %name,
                url = %entry.url,
                "Connecting to ACP agent..."
            );
            client.connect_remote_agent(&entry.url, name).await
        } else {
            // Connect to the primary agent at the URL
            tracing::info!(
                url = %entry.url,
                "Connecting to ACP server..."
            );
            client.connect_remote(&entry.url).await
        };

        match result {
            Ok(card) => {
                tracing::info!(
                    agent = %card.name,
                    url = %entry.url,
                    "ACP agent connected successfully"
                );
                cards.push(card);
            }
            Err(err) => {
                tracing::warn!(
                    url = %entry.url,
                    error = %err,
                    "Failed to connect to ACP agent, skipping"
                );
            }
        }
    }

    if cards.is_empty() {
        tracing::warn!("No ACP agents connected successfully");
        return None;
    }

    tracing::info!(
        agents = cards.len(),
        "ACP client initialized"
    );

    Some((Arc::new(RwLock::new(client)), cards))
}

/// Build the `AcpTool` for registration in the ToolRouter.
///
/// Returns `None` if no ACP agents are available.
pub fn build_acp_tool(
    client: Arc<RwLock<AcpClient>>,
    cards: Vec<AgentCard>,
) -> Option<Box<dyn BuiltinTool>> {
    if cards.is_empty() {
        return None;
    }
    Some(Box::new(AcpTool::new(client, cards)))
}

// ════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::agent_card::AgentSkill;
    use super::super::types::AcpError;

    #[test]
    fn test_acp_config_default() {
        let config = AcpConfig::default();
        assert!(!config.enabled);
        assert!(config.agents.is_empty());
        assert!(!config.has_agents());
    }

    #[test]
    fn test_acp_config_has_agents() {
        let config = AcpConfig {
            enabled: true,
            agents: vec![AcpAgentEntry {
                name: Some("test".to_string()),
                url: "http://localhost:3000".to_string(),
            }],
        };
        assert!(config.has_agents());
    }

    #[test]
    fn test_acp_config_disabled_with_agents() {
        let config = AcpConfig {
            enabled: false,
            agents: vec![AcpAgentEntry {
                name: Some("test".to_string()),
                url: "http://localhost:3000".to_string(),
            }],
        };
        assert!(!config.has_agents());
    }

    #[test]
    fn test_acp_tool_schema() {
        let client = Arc::new(RwLock::new(AcpClient::new("test")));
        let cards = vec![
            AgentCard::new("agent-a", "Agent A description")
                .with_skill(AgentSkill::new("analyze", "Analyze code")),
            AgentCard::new("agent-b", "Agent B description")
                .with_url("https://remote.example.com"),
        ];
        let tool = AcpTool::new(client, cards);

        assert_eq!(tool.name(), "call_acp_agent");

        let schema = tool.input_schema();
        let props = schema.get("properties").unwrap();
        assert!(props.get("agent_name").is_some());
        assert!(props.get("task").is_some());
        assert!(props.get("context").is_some());

        // Check enum values
        let agent_enum = props
            .get("agent_name")
            .unwrap()
            .get("enum")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(agent_enum.len(), 2);
        assert_eq!(agent_enum[0], "agent-a");
        assert_eq!(agent_enum[1], "agent-b");
    }

    #[test]
    fn test_acp_tool_description() {
        let client = Arc::new(RwLock::new(AcpClient::new("test")));
        let cards = vec![
            AgentCard::new("local-agent", "A local agent"),
            AgentCard::new("remote-agent", "A remote agent")
                .with_url("https://example.com"),
        ];
        let tool = AcpTool::new(client, cards);

        let desc = tool.build_description();
        assert!(desc.contains("local-agent"));
        assert!(desc.contains("[local]"));
        assert!(desc.contains("remote-agent"));
        assert!(desc.contains("[remote]"));
    }

    #[tokio::test]
    async fn test_acp_tool_execute_agent_not_found() {
        let client = Arc::new(RwLock::new(AcpClient::new("test")));
        let cards = vec![AgentCard::new("existing", "An agent")];
        let tool = AcpTool::new(client, cards);

        let args = serde_json::json!({
            "agent_name": "nonexistent",
            "task": "Do something"
        });

        let result = tool.execute(args).await.unwrap();
        assert!(result.contains("failed"));
        assert!(result.contains("nonexistent"));
    }

    #[tokio::test]
    async fn test_acp_tool_execute_success() {
        use super::super::server::AcpServer;
        use super::super::types::{TaskRequest, TaskResponse};

        // Create a mock ACP server
        struct MockServer {
            card: AgentCard,
        }

        #[async_trait]
        impl AcpServer for MockServer {
            fn card(&self) -> &AgentCard {
                &self.card
            }
            async fn handle_task(&self, request: TaskRequest) -> Result<TaskResponse, AcpError> {
                Ok(TaskResponse::completed(&request, "mock-agent", "Task done!"))
            }
        }

        let client = Arc::new(RwLock::new(AcpClient::new("test")));
        {
            let mut c = client.write().await;
            c.register(Arc::new(MockServer {
                card: AgentCard::new("mock-agent", "A mock agent"),
            }));
        }

        let cards = vec![AgentCard::new("mock-agent", "A mock agent")];
        let tool = AcpTool::new(client, cards);

        let args = serde_json::json!({
            "agent_name": "mock-agent",
            "task": "Do something useful"
        });

        let result = tool.execute(args).await.unwrap();
        assert!(result.contains("ACP Agent 'mock-agent'"));
        assert!(result.contains("completed"));
        assert!(result.contains("Task done!"));
    }

    #[tokio::test]
    async fn test_acp_tool_execute_with_context() {
        use super::super::server::AcpServer;
        use super::super::types::{TaskRequest, TaskResponse};

        struct ContextCapturingServer {
            card: AgentCard,
        }

        #[async_trait]
        impl AcpServer for ContextCapturingServer {
            fn card(&self) -> &AgentCard {
                &self.card
            }
            async fn handle_task(&self, request: TaskRequest) -> Result<TaskResponse, AcpError> {
                let content = format!(
                    "Task: {}, Context: {}",
                    request.task,
                    request.context.clone().unwrap_or_else(|| "none".to_string())
                );
                Ok(TaskResponse::completed(&request, "ctx-agent", content))
            }
        }

        let client = Arc::new(RwLock::new(AcpClient::new("test")));
        {
            let mut c = client.write().await;
            c.register(Arc::new(ContextCapturingServer {
                card: AgentCard::new("ctx-agent", "Context agent"),
            }));
        }

        let cards = vec![AgentCard::new("ctx-agent", "Context agent")];
        let tool = AcpTool::new(client, cards);

        let args = serde_json::json!({
            "agent_name": "ctx-agent",
            "task": "Analyze code",
            "context": "Working on a Rust project"
        });

        let result = tool.execute(args).await.unwrap();
        assert!(result.contains("Analyze code"));
        assert!(result.contains("Working on a Rust project"));
    }

    #[test]
    fn test_acp_config_deserialization() {
        let yaml = r#"
enabled: true
agents:
  - name: "code-analyzer"
    url: "https://analyzer.example.com"
  - url: "https://docs.example.com"
"#;
        let config: AcpConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.enabled);
        assert_eq!(config.agents.len(), 2);
        assert_eq!(config.agents[0].name, Some("code-analyzer".to_string()));
        assert_eq!(config.agents[0].url, "https://analyzer.example.com");
        assert_eq!(config.agents[1].name, None);
        assert_eq!(config.agents[1].url, "https://docs.example.com");
    }

    #[tokio::test]
    async fn test_init_acp_client_disabled() {
        let config = AcpConfig::default();
        let result = init_acp_client(&config).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_init_acp_client_no_agents() {
        let config = AcpConfig {
            enabled: true,
            agents: vec![],
        };
        let result = init_acp_client(&config).await;
        assert!(result.is_none());
    }
}
