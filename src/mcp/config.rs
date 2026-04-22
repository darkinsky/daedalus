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

    /// Try loading MCP config from common paths (env var + local file).
    ///
    /// Returns `Some(config)` if found, `None` if neither source exists.
    /// This is the shared prefix of `load()` and `load_with_workspace()`.
    fn try_common_paths() -> Result<Option<Self>> {
        // 1. DAEDALUS_MCP_CONFIG env var
        if let Ok(path) = std::env::var("DAEDALUS_MCP_CONFIG")
            && std::path::Path::new(&path).exists() {
                tracing::info!("Loading MCP config from env: {}", path);
                return Ok(Some(Self::from_file(&path)?));
            }

        // 2. ./mcp.json in the current directory
        let local_path = "mcp.json";
        if std::path::Path::new(local_path).exists() {
            tracing::info!("Loading MCP config from: {}", local_path);
            return Ok(Some(Self::from_file(local_path)?));
        }

        Ok(None)
    }

    /// Try loading MCP config from the legacy home directory path.
    ///
    /// Returns `Some(config)` if `~/.config/daedalus/mcp.json` exists.
    fn try_legacy_home_path() -> Result<Option<Self>> {
        if let Some(home) = crate::workspace::home_dir() {
            let config_path = format!("{}/.config/daedalus/mcp.json", home.display());
            if std::path::Path::new(&config_path).exists() {
                tracing::info!("Loading MCP config from: {}", config_path);
                return Ok(Some(Self::from_file(&config_path)?));
            }
        }
        Ok(None)
    }

    /// Load MCP configuration with workspace support.
    ///
    /// Checks in order:
    /// 1. `DAEDALUS_MCP_CONFIG` environment variable
    /// 2. `./mcp.json` in the current directory
    /// 3. Workspace `config/mcp.json`
    /// 4. `~/.config/daedalus/mcp.json` (legacy fallback)
    pub fn load_with_workspace(workspace: &crate::workspace::Workspace) -> Result<Self> {
        if let Some(config) = Self::try_common_paths()? {
            return Ok(config);
        }
        // Workspace-specific: check workspace config directory
        if workspace.has_mcp_config() {
            let ws_path = workspace.mcp_config_path();
            tracing::info!("Loading MCP config from workspace: {}", ws_path.display());
            return Self::from_file(ws_path.to_str().unwrap_or_default());
        }
        if let Some(config) = Self::try_legacy_home_path()? {
            return Ok(config);
        }
        tracing::debug!("No MCP config found, running without MCP servers");
        Ok(Self::default())
    }
}
