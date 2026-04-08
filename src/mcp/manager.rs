use anyhow::{Context, Result};

use super::client::McpClient;
use super::config::McpConfig;
use super::types::ToolDefinition;

use crate::llm::ToolInfo;

/// The MCP manager — owns all MCP client connections and provides
/// a unified interface for tool discovery and execution.
pub struct McpManager {
    /// All connected MCP clients.
    clients: Vec<McpClient>,
}

impl McpManager {
    /// Create a new MCP manager and connect to all configured servers.
    ///
    /// Servers that fail to connect are logged and skipped (non-fatal).
    pub async fn from_config(config: &McpConfig) -> Self {
        let mut clients = Vec::new();

        for (name, server_config) in &config.servers {
            let args: Vec<&str> = server_config.args.iter().map(|s| s.as_str()).collect();
            let env: Vec<(&str, &str)> = server_config.env.iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();

            let env_ref = if env.is_empty() { None } else { Some(env.as_slice()) };

            match McpClient::new(name, &server_config.command, &args, env_ref).await {
                Ok(client) => {
                    tracing::info!(
                        server = %name,
                        tools = client.tools().len(),
                        "MCP server connected"
                    );
                    clients.push(client);
                }
                Err(e) => {
                    tracing::error!(
                        server = %name,
                        error = %e,
                        "Failed to connect to MCP server (skipping)"
                    );
                }
            }
        }

        Self { clients }
    }

    /// Create an empty manager (no MCP servers).
    #[allow(dead_code)]
    pub fn empty() -> Self {
        Self { clients: Vec::new() }
    }

    /// Return true if any MCP servers are connected.
    pub fn has_servers(&self) -> bool {
        !self.clients.is_empty()
    }

    /// Return the total number of tools available across all servers.
    pub fn tool_count(&self) -> usize {
        self.clients.iter().map(|c| c.tools().len()).sum()
    }

    /// Get all available tools with their server names.
    ///
    /// Returns `(server_name, tool_definition)` pairs.
    pub fn all_tools(&self) -> Vec<(&str, &ToolDefinition)> {
        self.clients
            .iter()
            .flat_map(|client| {
                client.tools().iter().map(move |tool| {
                    (client.server_name(), tool)
                })
            })
            .collect()
    }

    /// Build tool definitions in OpenAI function-calling JSON format.
    ///
    /// This is the provider-agnostic format; the LLM provider layer
    /// converts it to whatever wire format it needs.
    pub fn build_tool_definitions(&self) -> Vec<serde_json::Value> {
        self.all_tools()
            .iter()
            .map(|(_, tool)| tool.to_openai_json())
            .collect()
    }

    /// Return tool descriptions for CLI display.
    pub fn tool_descriptions(&self) -> Vec<ToolInfo> {
        self.all_tools()
            .iter()
            .map(|(server, tool)| ToolInfo {
                name: tool.name.clone(),
                description: tool.description.clone().unwrap_or_default(),
                server: server.to_string(),
            })
            .collect()
    }

    /// Find which server owns a given tool name.
    fn find_server_for_tool(&self, tool_name: &str) -> Option<&McpClient> {
        self.clients.iter().find(|client| {
            client.tools().iter().any(|t| t.name == tool_name)
        })
    }

    /// Call a tool by name, automatically routing to the correct server.
    ///
    /// # Arguments
    /// * `tool_name` - The tool name (must match a discovered tool).
    /// * `arguments` - The arguments as a JSON object.
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<String> {
        let client = self.find_server_for_tool(tool_name)
            .with_context(|| format!("No MCP server provides tool '{}'", tool_name))?;

        let result = client.call_tool(tool_name, arguments).await?;

        // Concatenate all text content from the result
        let text = result.content.iter()
            .filter_map(|c| c.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n");

        Ok(text)
    }

    /// Shut down all MCP servers gracefully.
    #[allow(dead_code)]
    pub async fn shutdown(&mut self) {
        for client in &mut self.clients {
            if let Err(e) = client.shutdown().await {
                tracing::warn!(
                    server = %client.server_name(),
                    error = %e,
                    "Error shutting down MCP server"
                );
            }
        }
    }
}
