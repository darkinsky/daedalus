use std::collections::HashMap;

use anyhow::{Context, Result};

/// Configuration for a single MCP server.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct McpServerConfig {
    /// The command to run (e.g., "npx", "python", "node").
    pub command: String,
    /// Arguments to pass to the command.
    #[serde(default)]
    pub args: Vec<String>,
    /// Optional environment variables.
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// Top-level MCP configuration: a map of server name → server config.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct McpConfig {
    /// Map of server name to server configuration.
    #[serde(rename = "mcpServers", default)]
    pub servers: HashMap<String, McpServerConfig>,
}

impl McpConfig {
    /// Load MCP configuration from a JSON file.
    ///
    /// The expected format is:
    /// ```json
    /// {
    ///   "mcpServers": {
    ///     "filesystem": {
    ///       "command": "npx",
    ///       "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
    ///       "env": {}
    ///     }
    ///   }
    /// }
    /// ```
    pub fn from_file(path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read MCP config file: {}", path))?;
        let config: McpConfig = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse MCP config file: {}", path))?;
        Ok(config)
    }

    /// Load MCP configuration from the default config path or env var.
    ///
    /// Checks in order:
    /// 1. `DAEDALUS_MCP_CONFIG` environment variable
    /// 2. `./mcp.json` in the current directory
    /// 3. `~/.config/daedalus/mcp.json`
    #[allow(dead_code)]
    pub fn load() -> Result<Self> {
        // Check env var first
        if let Ok(path) = std::env::var("DAEDALUS_MCP_CONFIG")
            && std::path::Path::new(&path).exists() {
                tracing::info!("Loading MCP config from env: {}", path);
                return Self::from_file(&path);
            }

        // Check current directory
        let local_path = "mcp.json";
        if std::path::Path::new(local_path).exists() {
            tracing::info!("Loading MCP config from: {}", local_path);
            return Self::from_file(local_path);
        }

        // Check ~/.config/daedalus/mcp.json
        if let Some(home) = home_dir() {
            let config_path = format!("{}/.config/daedalus/mcp.json", home);
            if std::path::Path::new(&config_path).exists() {
                tracing::info!("Loading MCP config from: {}", config_path);
                return Self::from_file(&config_path);
            }
        }

        // No config found — return empty (no MCP servers)
        tracing::debug!("No MCP config found, running without MCP servers");
        Ok(Self::default())
    }

    /// Load MCP configuration with workspace support.
    ///
    /// Checks in order:
    /// 1. `DAEDALUS_MCP_CONFIG` environment variable
    /// 2. `./mcp.json` in the current directory
    /// 3. Workspace `config/mcp.json`
    /// 4. `~/.config/daedalus/mcp.json` (legacy fallback)
    pub fn load_with_workspace(workspace: &crate::workspace::Workspace) -> Result<Self> {
        // Check env var first
        if let Ok(path) = std::env::var("DAEDALUS_MCP_CONFIG")
            && std::path::Path::new(&path).exists() {
                tracing::info!("Loading MCP config from env: {}", path);
                return Self::from_file(&path);
            }

        // Check current directory
        let local_path = "mcp.json";
        if std::path::Path::new(local_path).exists() {
            tracing::info!("Loading MCP config from: {}", local_path);
            return Self::from_file(local_path);
        }

        // Check workspace config
        if workspace.has_mcp_config() {
            let ws_path = workspace.mcp_config_path();
            tracing::info!("Loading MCP config from workspace: {}", ws_path.display());
            return Self::from_file(ws_path.to_str().unwrap_or_default());
        }

        // Legacy fallback: ~/.config/daedalus/mcp.json
        if let Some(home) = home_dir() {
            let config_path = format!("{}/.config/daedalus/mcp.json", home);
            if std::path::Path::new(&config_path).exists() {
                tracing::info!("Loading MCP config from: {}", config_path);
                return Self::from_file(&config_path);
            }
        }

        // No config found — return empty (no MCP servers)
        tracing::debug!("No MCP config found, running without MCP servers");
        Ok(Self::default())
    }
}

/// Get the user's home directory from environment variables.
fn home_dir() -> Option<String> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
}
