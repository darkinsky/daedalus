use anyhow::{Context, Result};

use super::client::McpClient;
use super::config::McpConfig;
use super::types::ToolDefinition;

use crate::tools::ToolInfo;

/// The MCP manager — owns all MCP client connections and provides
/// a unified interface for tool discovery and execution.
pub struct McpManager {
    /// All connected MCP clients.
    clients: Vec<McpClient>,
}

impl McpManager {
    /// Create a new MCP manager and connect to all configured servers.
    ///
    /// Servers are connected in parallel for faster startup.
    /// Servers that fail to connect are logged and skipped (non-fatal).
    pub async fn from_config(config: &McpConfig) -> Self {
        use tokio::task::JoinSet;

        let mut join_set = JoinSet::new();

        for (name, server_config) in &config.servers {
            let name = name.clone();
            let command = server_config.command.clone();
            let args: Vec<String> = server_config.args.clone();
            let env: Vec<(String, String)> = server_config.env.iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();

            // Clone into owned values because JoinSet::spawn requires 'static futures.
            // Inside the async block we convert back to borrowed slices for McpClient::new.
            join_set.spawn(async move {
                let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                let env_refs: Vec<(&str, &str)> = env.iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect();
                let env_ref = if env_refs.is_empty() { None } else { Some(env_refs.as_slice()) };

                let result = McpClient::new(&name, &command, &arg_refs, env_ref).await;
                (name, result)
            });
        }

        let mut clients = Vec::new();
        while let Some(result) = join_set.join_next().await {
            match result {
                Ok((name, Ok(client))) => {
                    tracing::info!(
                        server = %name,
                        tools = client.tools().len(),
                        "MCP server connected"
                    );
                    clients.push(client);
                }
                Ok((name, Err(e))) => {
                    tracing::error!(
                        server = %name,
                        error = %e,
                        "Failed to connect to MCP server (skipping)"
                    );
                }
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "MCP server connection task panicked"
                    );
                }
            }
        }

        Self { clients }
    }

    /// Create an empty manager (no MCP servers).
    ///
    /// Useful for testing or when MCP is explicitly disabled.
    #[allow(dead_code)]
    pub fn empty() -> Self {
        Self { clients: Vec::new() }
    }

    /// Return true if any MCP servers are connected.
    ///
    /// Prefer `has_tools()` for checking tool availability, as a connected
    /// server may not necessarily provide any tools.
    #[allow(dead_code)]
    pub fn has_servers(&self) -> bool {
        !self.clients.is_empty()
    }

    /// Return true if any tools are available across all connected servers.
    pub fn has_tools(&self) -> bool {
        self.tool_count() > 0
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

    /// Return tool metadata for CLI display.
    pub fn tool_infos(&self) -> Vec<ToolInfo> {
        self.all_tools()
            .iter()
            .map(|(server, tool)| ToolInfo {
                name: tool.name.clone(),
                description: tool.description.clone().unwrap_or_default(),
                source: server.to_string(),
                usage_hint: None,
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
    /// Returns the concatenated text content from the tool result.
    /// If the tool reported an error (`is_error: true`), the text is prefixed
    /// with `[Tool Error]` so the LLM can distinguish success from failure.
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
        let is_error = result.is_error.unwrap_or(false);

        // Concatenate all text content from the result
        let text = result.content.iter()
            .filter_map(|c| c.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n");

        if is_error {
            Ok(format!("[Tool Error] {}", text))
        } else {
            Ok(text)
        }
    }

    /// Shut down all MCP servers gracefully.
    ///
    /// Called during application shutdown to terminate child processes
    /// and prevent orphaned MCP server processes.
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
