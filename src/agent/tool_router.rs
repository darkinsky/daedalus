use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use crate::llm::{ToolCall, ToolResponse};
use crate::mcp::McpManager;
use crate::skill::SkillRegistry;
use crate::tools::{BuiltinToolRegistry, ToolInfo};

/// Unified tool router — dispatches tool calls to built-in tools or MCP servers.
///
/// This component owns all tool sources (built-in registry + optional MCP manager)
/// and provides a single interface for:
/// - Aggregating tool definitions from all sources
/// - Routing tool calls to the correct handler
/// - Generating tool descriptions for CLI display
///
/// Routing priority: built-in tools first (including skills), then MCP servers.
pub struct ToolRouter {
    /// Built-in tools (filesystem, skills, etc.) — always available.
    builtin: BuiltinToolRegistry,
    /// Optional MCP manager for external tool servers.
    mcp: Option<McpManager>,
    /// Skill registry (shared via Arc so SkillTool can reference it).
    skills: Arc<SkillRegistry>,
}

impl ToolRouter {
    /// Create a new tool router with built-in tools and no MCP servers.
    pub fn new() -> Self {
        Self {
            builtin: BuiltinToolRegistry::new(),
            mcp: None,
            skills: Arc::new(SkillRegistry::new()),
        }
    }

    /// Load skills from a directory and register them as a built-in tool.
    ///
    /// Skills are exposed to the LLM as a single `use_skill` tool.
    /// The LLM decides which skill to invoke based on the skill
    /// descriptions embedded in the tool definition.
    ///
    /// Internally, this creates a new `SkillRegistry`, wraps it in `Arc`,
    /// and registers a `SkillTool` adapter into the `BuiltinToolRegistry`.
    pub fn load_skills(&mut self, dir: &Path) -> Result<usize> {
        let mut registry = SkillRegistry::new();
        let count = registry.load_from_dir(dir)?;
        if count > 0 {
            let registry = Arc::new(registry);
            // Register the SkillTool as a regular built-in tool
            if let Some(skill_tool) = registry.build_skill_tool() {
                self.builtin.register_tool(skill_tool);
            }
            self.skills = registry;
            tracing::info!(
                skills = count,
                path = %dir.display(),
                "Skills loaded into ToolRouter as built-in tool"
            );
        }
        Ok(count)
    }

    /// Attach an MCP manager to enable external tool calling.
    pub fn attach_mcp(&mut self, mcp: McpManager) {
        tracing::info!(
            tools = mcp.tool_count(),
            "MCP manager attached to ToolRouter"
        );
        self.mcp = Some(mcp);
    }

    /// Return true if any tools (built-in or MCP) are available.
    pub fn has_tools(&self) -> bool {
        self.builtin.tool_count() > 0
            || self.mcp.as_ref().map(|m| m.has_tools()).unwrap_or(false)
    }

    /// Return the total number of tools across all sources.
    pub fn tool_count(&self) -> usize {
        self.builtin.tool_count()
            + self.mcp.as_ref().map(|m| m.tool_count()).unwrap_or(0)
    }

    /// Build tool definitions in OpenAI function-calling JSON format.
    ///
    /// Combines built-in (including skill) and MCP tool definitions into a single list.
    pub fn build_tool_definitions(&self) -> Vec<serde_json::Value> {
        let mut definitions = self.builtin.build_tool_definitions();
        if let Some(ref mcp) = self.mcp {
            definitions.extend(mcp.build_tool_definitions());
        }
        definitions
    }

    /// Check if a tool is a built-in tool (as opposed to an MCP tool).
    pub fn is_builtin(&self, name: &str) -> bool {
        self.builtin.has_tool(name)
    }

    /// Return tool metadata for CLI display and prompt building.
    pub fn tool_infos(&self) -> Vec<ToolInfo> {
        let mut infos = self.builtin.tool_infos();
        if let Some(ref mcp) = self.mcp {
            infos.extend(mcp.tool_infos());
        }
        infos
    }

    /// Return the skill registry reference (for CLI display).
    pub fn skill_registry(&self) -> &SkillRegistry {
        &self.skills
    }

    /// Execute a single tool call, routing to the correct handler.
    ///
    /// Routing priority: built-in tools first (including skills), then MCP servers.
    /// Always returns a `ToolResponse` (never fails at the routing level;
    /// errors are captured in the response content).
    pub async fn execute(&self, tool_call: &ToolCall) -> ToolResponse {
        tracing::info!(
            tool = %tool_call.function_name,
            arguments = %tool_call.arguments,
            "Executing tool call"
        );

        // Try built-in tools first (includes skill tool)
        if self.builtin.has_tool(&tool_call.function_name) {
            return self.execute_and_log(
                &tool_call.call_id,
                &tool_call.function_name,
                "built-in",
                self.builtin.call_tool(&tool_call.function_name, tool_call.arguments.clone()),
            ).await;
        }

        // Try MCP servers
        if let Some(ref mcp) = self.mcp {
            return self.execute_and_log(
                &tool_call.call_id,
                &tool_call.function_name,
                "mcp",
                mcp.call_tool(&tool_call.function_name, tool_call.arguments.clone()),
            ).await;
        }

        // No handler found
        ToolResponse::error(
            &tool_call.call_id,
            format!("Error: No handler found for tool '{}'", tool_call.function_name),
        )
    }

    /// Execute a tool call future and log the result.
    ///
    /// This helper eliminates the duplicated Ok/Err logging pattern
    /// that was previously repeated for each tool source.
    async fn execute_and_log(
        &self,
        call_id: &str,
        tool_name: &str,
        source: &str,
        fut: impl std::future::Future<Output = Result<String>>,
    ) -> ToolResponse {
        match fut.await {
            Ok(result) => {
                tracing::info!(
                    tool = %tool_name,
                    result_len = result.len(),
                    source = source,
                    "Tool call succeeded"
                );
                ToolResponse::new(call_id, result)
            }
            Err(e) => {
                tracing::error!(
                    tool = %tool_name,
                    error = %e,
                    source = source,
                    "Tool call failed"
                );
                ToolResponse::error(
                    call_id,
                    format!("Error calling tool '{}': {}", tool_name, e),
                )
            }
        }
    }

    /// Shut down all external tool servers (MCP) gracefully.
    ///
    /// Called during application shutdown to prevent orphaned child processes.
    pub async fn shutdown(&mut self) {
        if let Some(ref mut mcp) = self.mcp {
            tracing::info!("Shutting down MCP servers...");
            mcp.shutdown().await;
            tracing::info!("MCP servers shut down");
        }
    }
}
