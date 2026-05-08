//! Built-in tool for web search — fetches real-time information from the internet.
//!
//! ## Module structure
//!
//! ```text
//! web_search/
//! ├── mod.rs         — Config types, WebSearchTool, BuiltinTool impl
//! ├── duckduckgo.rs  — DuckDuckGo backend (free, no API key)
//! ├── tavily.rs      — Tavily backend (API key required)
//! ├── exa.rs         — Exa backend (neural/keyword/auto search)
//! └── infoquest.rs   — InfoQuest backend (time range filtering)
//! ```
//!
//! The backend is selected via YAML configuration (`web_search.provider`) or
//! defaults to DuckDuckGo if not configured.

mod duckduckgo;
mod exa;
mod infoquest;
mod tavily;

use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;

use super::BuiltinTool;

// ── Configuration ──

/// Web search provider selection.
#[derive(Debug, Clone, Default, serde::Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum WebSearchProvider {
    /// DuckDuckGo — free, no API key required (default)
    #[default]
    DuckDuckGo,
    /// Tavily — requires TAVILY_API_KEY
    Tavily,
    /// Exa — requires EXA_API_KEY
    Exa,
    /// InfoQuest — requires INFOQUEST_API_KEY
    InfoQuest,
}

/// Web search configuration from YAML.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct WebSearchConfig {
    /// Which search provider to use.
    pub provider: WebSearchProvider,
    /// API key (optional, can also use env vars).
    pub api_key: Option<String>,
    /// API base URL override (optional, for self-hosted instances).
    pub api_base: Option<String>,
    /// Maximum number of results to return (default: 5).
    pub max_results: Option<usize>,
}

// ── Tool implementation ──

/// Web search tool that delegates to the configured backend.
pub struct WebSearchTool {
    pub(crate) config: WebSearchConfig,
    pub(crate) client: Client,
}

impl WebSearchTool {
    /// Create a new WebSearchTool with the given configuration.
    pub fn new(config: WebSearchConfig) -> Self {
        Self {
            config,
            client: Client::new(),
        }
    }

    /// Resolve API key from config or environment variable.
    pub(crate) fn resolve_api_key(&self, env_var: &str) -> Result<String> {
        self.config
            .api_key
            .clone()
            .or_else(|| std::env::var(env_var).ok())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "API key required for {:?} provider. Set `web_search.api_key` in config or {} env var.",
                    self.config.provider,
                    env_var
                )
            })
    }
}

#[async_trait]
impl BuiltinTool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web for real-time information. Use this tool when you need up-to-date \
         information that might not be available in your training data, or when you need to \
         verify current facts. Returns relevant snippets and URLs from web pages."
    }

    fn input_schema(&self) -> serde_json::Value {
        let mut properties = serde_json::json!({
            "query": {
                "type": "string",
                "description": "The search query. Be specific and include relevant keywords for better results."
            },
            "max_results": {
                "type": "integer",
                "description": "Maximum number of results to return (default: 5, max: 10)."
            }
        });

        // Add provider-specific parameters
        match self.config.provider {
            WebSearchProvider::Exa => {
                properties.as_object_mut().unwrap().insert(
                    "search_type".to_string(),
                    serde_json::json!({
                        "type": "string",
                        "description": "Search type: 'neural' (semantic), 'keyword' (traditional), or 'auto' (let Exa decide). Default: 'auto'.",
                        "enum": ["neural", "keyword", "auto"]
                    }),
                );
            }
            WebSearchProvider::InfoQuest => {
                properties.as_object_mut().unwrap().insert(
                    "time_range".to_string(),
                    serde_json::json!({
                        "type": "string",
                        "description": "Time range filter: 'day', 'week', 'month', 'year', or 'all'. Default: 'all'.",
                        "enum": ["day", "week", "month", "year", "all"]
                    }),
                );
            }
            _ => {}
        }

        serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": ["query"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String> {
        let query = arguments
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: query"))?;

        let max_results = arguments
            .get("max_results")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(self.config.max_results.unwrap_or(5))
            .min(10);

        match self.config.provider {
            WebSearchProvider::DuckDuckGo => {
                duckduckgo::search(self, query, max_results).await
            }
            WebSearchProvider::Tavily => {
                tavily::search(self, query, max_results).await
            }
            WebSearchProvider::Exa => {
                let search_type = arguments
                    .get("search_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("auto");
                exa::search(self, query, max_results, search_type).await
            }
            WebSearchProvider::InfoQuest => {
                let time_range = arguments
                    .get("time_range")
                    .and_then(|v| v.as_str())
                    .unwrap_or("all");
                infoquest::search(self, query, max_results, time_range).await
            }
        }
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = WebSearchConfig::default();
        assert_eq!(config.provider, WebSearchProvider::DuckDuckGo);
        assert!(config.api_key.is_none());
    }

    #[tokio::test]
    async fn test_tool_schema() {
        let config = WebSearchConfig::default();
        let tool = WebSearchTool::new(config);
        assert_eq!(tool.name(), "web_search");
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["query"].is_object());
    }

    #[tokio::test]
    async fn test_exa_schema_has_search_type() {
        let config = WebSearchConfig {
            provider: WebSearchProvider::Exa,
            ..Default::default()
        };
        let tool = WebSearchTool::new(config);
        let schema = tool.input_schema();
        assert!(schema["properties"]["search_type"].is_object());
    }

    #[tokio::test]
    async fn test_infoquest_schema_has_time_range() {
        let config = WebSearchConfig {
            provider: WebSearchProvider::InfoQuest,
            ..Default::default()
        };
        let tool = WebSearchTool::new(config);
        let schema = tool.input_schema();
        assert!(schema["properties"]["time_range"].is_object());
    }
}
