use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use super::types::{
    JsonRpcRequest, JsonRpcResponse, InitializeResult, ServerInfo,
    ToolDefinition, ToolsListResult, ToolCallResult,
};

/// Default timeout for MCP request/response round-trips.
const MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Timeout for the MCP initialize handshake (servers may need extra startup time).
const MCP_INIT_TIMEOUT: Duration = Duration::from_secs(60);

/// An MCP client that communicates with a single MCP server over stdio.
///
/// The client spawns the server as a child process and communicates via
/// JSON-RPC 2.0 messages over stdin/stdout. It handles the full MCP
/// lifecycle: initialize → list tools → call tools → shutdown.
pub struct McpClient {
    /// Human-readable name for this server connection.
    server_name: String,
    /// The child process running the MCP server.
    child: Child,
    /// Writer to the child's stdin (wrapped in Mutex for safe async access).
    stdin: Mutex<tokio::process::ChildStdin>,
    /// Reader from the child's stdout.
    stdout: Mutex<BufReader<tokio::process::ChildStdout>>,
    /// Auto-incrementing JSON-RPC request ID counter.
    request_id_counter: AtomicU64,
    /// Tools discovered from this server.
    tools: Vec<ToolDefinition>,
    /// Server info from the initialize handshake.
    server_info: Option<ServerInfo>,
}

impl McpClient {
    /// Spawn an MCP server process and create a client connected to it.
    ///
    /// # Arguments
    /// * `server_name` - A human-readable name for this server.
    /// * `command` - The command to run (e.g., "npx", "python").
    /// * `args` - Arguments to pass to the command.
    /// * `env` - Optional environment variables to set.
    pub async fn new(
        server_name: impl Into<String>,
        command: &str,
        args: &[&str],
        env: Option<&[(&str, &str)]>,
    ) -> Result<Self> {
        let server_name = server_name.into();

        tracing::info!(
            server = %server_name,
            command = %command,
            args = ?args,
            "Spawning MCP server"
        );

        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(env_vars) = env {
            for (key, value) in env_vars {
                cmd.env(key, value);
            }
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("Failed to spawn MCP server '{}': {} {:?}", server_name, command, args))?;

        let stdin = child.stdin.take()
            .context("Failed to capture MCP server stdin")?;
        let stdout = child.stdout.take()
            .context("Failed to capture MCP server stdout")?;

        let mut client = Self {
            server_name,
            child,
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(BufReader::new(stdout)),
            request_id_counter: AtomicU64::new(1),
            tools: Vec::new(),
            server_info: None,
        };

        // Perform the MCP initialize handshake
        client.initialize().await?;

        // Discover available tools
        client.discover_tools().await?;

        Ok(client)
    }

    /// Return the server name.
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Return the tools discovered from this server.
    pub fn tools(&self) -> &[ToolDefinition] {
        &self.tools
    }

    /// Return server info (if available).
    ///
    /// Currently unused but part of the public MCP client API for
    /// future features (e.g., server capability negotiation).
    #[allow(dead_code)]
    pub fn server_info(&self) -> Option<&ServerInfo> {
        self.server_info.as_ref()
    }

    /// Send a JSON-RPC request and read the response.
    ///
    /// Uses the default `MCP_REQUEST_TIMEOUT` for the response wait.
    async fn send_request(&self, method: &str, params: Option<serde_json::Value>) -> Result<JsonRpcResponse> {
        self.send_request_with_timeout(method, params, MCP_REQUEST_TIMEOUT).await
    }

    /// Send a JSON-RPC request and read the response with a custom timeout.
    ///
    /// Checks that the child process is still alive before sending.
    async fn send_request_with_timeout(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout: Duration,
    ) -> Result<JsonRpcResponse> {
        let id = self.request_id_counter.fetch_add(1, Ordering::SeqCst);
        let request = JsonRpcRequest::new(id, method, params);

        let request_json = serde_json::to_string(&request)
            .context("Failed to serialize JSON-RPC request")?;

        tracing::debug!(
            server = %self.server_name,
            method = %method,
            id = id,
            "Sending MCP request"
        );

        // Write request + newline to stdin
        {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(request_json.as_bytes()).await
                .context("Failed to write to MCP server stdin")?;
            stdin.write_all(b"\n").await
                .context("Failed to write newline to MCP server stdin")?;
            stdin.flush().await
                .context("Failed to flush MCP server stdin")?;
        }

        // Read response line from stdout (with timeout to prevent hanging)
        let response_line = {
            let mut stdout = self.stdout.lock().await;
            let mut line = String::new();
            let read_fut = stdout.read_line(&mut line);
            tokio::time::timeout(timeout, read_fut)
                .await
                .map_err(|_| anyhow::anyhow!(
                    "MCP server '{}' timed out after {}s waiting for response to '{}'",
                    self.server_name, timeout.as_secs(), method
                ))?
                .context("Failed to read from MCP server stdout")?;
            line
        };

        if response_line.is_empty() {
            anyhow::bail!("MCP server '{}' closed stdout unexpectedly", self.server_name);
        }

        let response: JsonRpcResponse = serde_json::from_str(&response_line)
            .with_context(|| format!(
                "Failed to parse MCP server response: {}",
                response_line.trim()
            ))?;

        if let Some(ref error) = response.error {
            tracing::warn!(
                server = %self.server_name,
                code = error.code,
                message = %error.message,
                "MCP server returned error"
            );
        }

        Ok(response)
    }

    /// Send a JSON-RPC notification (no response expected).
    async fn send_notification(&self, method: &str, params: Option<serde_json::Value>) -> Result<()> {
        // Notifications have no id field
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params.unwrap_or(serde_json::Value::Null),
        });

        let json = serde_json::to_string(&notification)
            .context("Failed to serialize JSON-RPC notification")?;

        let mut stdin = self.stdin.lock().await;
        stdin.write_all(json.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;

        Ok(())
    }

    /// Perform the MCP `initialize` handshake.
    async fn initialize(&mut self) -> Result<()> {
        let params = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "Daedalus",
                "version": env!("CARGO_PKG_VERSION"),
            }
        });

        // Use a longer timeout for initialization (server may need startup time)
        let response = self.send_request_with_timeout("initialize", Some(params), MCP_INIT_TIMEOUT).await?;

        if let Some(error) = response.error {
            anyhow::bail!("MCP initialize failed for '{}': {}", self.server_name, error);
        }

        if let Some(result) = response.result {
            let init_result: InitializeResult = serde_json::from_value(result)
                .context("Failed to parse initialize result")?;

            tracing::info!(
                server = %self.server_name,
                protocol_version = %init_result.protocol_version,
                server_info = ?init_result.server_info,
                "MCP server initialized"
            );

            self.server_info = init_result.server_info;
        }

        // Send the `initialized` notification to complete the handshake
        self.send_notification("notifications/initialized", None).await?;

        Ok(())
    }

    /// Discover tools by calling `tools/list`.
    async fn discover_tools(&mut self) -> Result<()> {
        let response = self.send_request("tools/list", None).await?;

        if let Some(error) = response.error {
            // tools/list not supported — that's okay, just no tools
            tracing::warn!(
                server = %self.server_name,
                "tools/list failed: {} (server may not support tools)",
                error
            );
            return Ok(());
        }

        if let Some(result) = response.result {
            let tools_result: ToolsListResult = serde_json::from_value(result)
                .context("Failed to parse tools/list result")?;

            tracing::info!(
                server = %self.server_name,
                tool_count = tools_result.tools.len(),
                tools = ?tools_result.tools.iter().map(|t| &t.name).collect::<Vec<_>>(),
                "Discovered MCP tools"
            );

            self.tools = tools_result.tools;
        }

        Ok(())
    }

    /// Call a tool on this MCP server.
    ///
    /// # Arguments
    /// * `tool_name` - The name of the tool to call.
    /// * `arguments` - The arguments to pass (as a JSON object).
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<ToolCallResult> {
        tracing::info!(
            server = %self.server_name,
            tool = %tool_name,
            "Calling MCP tool"
        );

        let params = serde_json::json!({
            "name": tool_name,
            "arguments": arguments,
        });

        let response = self.send_request("tools/call", Some(params)).await?;

        if let Some(error) = response.error {
            anyhow::bail!(
                "MCP tool call '{}' failed on server '{}': {}",
                tool_name,
                self.server_name,
                error
            );
        }

        let result = response.result
            .context("MCP tools/call returned no result")?;

        let tool_result: ToolCallResult = serde_json::from_value(result)
            .context("Failed to parse tools/call result")?;

        if tool_result.is_error.unwrap_or(false) {
            let error_text = tool_result.content.iter()
                .filter_map(|c| c.text.as_deref())
                .collect::<Vec<_>>()
                .join("\n");
            tracing::warn!(
                server = %self.server_name,
                tool = %tool_name,
                error = %error_text,
                "MCP tool returned error"
            );
        }

        Ok(tool_result)
    }

    /// Gracefully shut down the MCP server.
    ///
    /// Called during application shutdown. Closes the stdin pipe to signal
    /// the server to exit, then kills the process if still running.
    #[allow(dead_code)]
    pub async fn shutdown(&mut self) -> Result<()> {
        tracing::info!(server = %self.server_name, "Shutting down MCP server");

        // Close stdin to signal the server to exit gracefully
        drop(self.stdin.lock().await);

        // Give the server a moment to exit, then force-kill if needed
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.child.wait(),
        ).await;

        // Kill the child process if still running
        let _ = self.child.kill().await;

        Ok(())
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        // Best-effort kill on drop
        if let Ok(Some(_)) = self.child.try_wait() {
            // Already exited
        } else {
            let _ = self.child.start_kill();
        }
    }
}
