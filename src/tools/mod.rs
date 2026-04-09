pub mod fs;

use anyhow::Result;
use async_trait::async_trait;

/// A tool description for CLI display, prompt building, and tool routing.
///
/// This is the canonical definition of tool metadata, shared across all
/// modules that need to describe tools (agent, prompt, CLI, MCP).
///
/// Previously lived in `llm::types` but was moved here because it describes
/// tools, not LLM concepts. It is re-exported from `crate::llm::ToolInfo`
/// for backward compatibility.
#[derive(Debug, Clone)]
pub struct ToolInfo {
    /// The tool name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Which source provides this tool (e.g., "built-in", MCP server name).
    pub server: String,
}

/// A built-in tool that can be called directly without an external MCP server.
///
/// Built-in tools follow the same OpenAI function-calling JSON format as MCP tools,
/// making them seamlessly interchangeable from the LLM's perspective.
#[async_trait]
pub trait BuiltinTool: Send + Sync {
    /// Return the tool name (unique identifier).
    fn name(&self) -> &str;

    /// Return a human-readable description of the tool.
    fn description(&self) -> &str;

    /// Return the JSON Schema for the tool's input parameters.
    fn input_schema(&self) -> serde_json::Value;

    /// Execute the tool with the given arguments and return the result as text.
    async fn execute(&self, arguments: serde_json::Value) -> Result<String>;

    /// Convert to OpenAI function-calling JSON format.
    fn to_openai_json(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name(),
                "description": self.description(),
                "parameters": self.input_schema(),
            }
        })
    }
}

/// Registry that manages all built-in tools.
///
/// Provides tool discovery, definition generation, and execution routing —
/// the same interface pattern as `McpManager` but for local tools.
pub struct BuiltinToolRegistry {
    tools: Vec<Box<dyn BuiltinTool>>,
}

impl BuiltinToolRegistry {
    /// Create a new registry with the default set of built-in tools.
    pub fn new() -> Self {
        let tools: Vec<Box<dyn BuiltinTool>> = vec![
            Box::new(fs::ReadFileTool),
            Box::new(fs::WriteFileTool),
            Box::new(fs::ListDirectoryTool),
            Box::new(fs::SearchFilesTool),
            Box::new(fs::GetFileInfoTool),
        ];

        tracing::info!(
            tool_count = tools.len(),
            "Built-in tool registry initialized"
        );

        Self { tools }
    }

    /// Return the total number of built-in tools.
    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }

    /// Build tool definitions in OpenAI function-calling JSON format.
    pub fn build_tool_definitions(&self) -> Vec<serde_json::Value> {
        self.tools.iter().map(|t| t.to_openai_json()).collect()
    }

    /// Return tool descriptions for CLI display.
    pub fn tool_descriptions(&self) -> Vec<ToolInfo> {
        self.tools
            .iter()
            .map(|t| ToolInfo {
                name: t.name().to_string(),
                description: t.description().to_string(),
                server: "built-in".to_string(),
            })
            .collect()
    }

    /// Check if a tool with the given name exists in this registry.
    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.iter().any(|t| t.name() == name)
    }

    /// Execute a built-in tool by name.
    ///
    /// Returns the tool output as a string, or an error if the tool is not found
    /// or execution fails.
    pub async fn call_tool(&self, name: &str, arguments: serde_json::Value) -> Result<String> {
        let tool = self
            .tools
            .iter()
            .find(|t| t.name() == name)
            .ok_or_else(|| anyhow::anyhow!("Built-in tool '{}' not found", name))?;

        tool.execute(arguments).await
    }
}
