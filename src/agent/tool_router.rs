use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use crate::llm::{LlmConfig, ToolCall, ToolResponse};
use crate::mcp::McpManager;
use crate::skill::SkillRegistry;
use crate::subagent::{SubagentRegistry, SubagentRunner};
use crate::subagent::tool::SharedEventCallback;
use crate::tools::{BuiltinToolRegistry, ToolInfo};

/// Tool filter for --allowed-tools / --disallowed-tools CLI flags.
///
/// When active, only tools matching the filter are exposed to the LLM
/// and allowed to execute.
#[derive(Debug, Clone)]
pub struct ToolFilter {
    /// If set, only these tools are allowed (allowlist mode).
    allowed: Option<Vec<String>>,
    /// If set, these tools are blocked (denylist mode).
    disallowed: Option<Vec<String>>,
}

impl ToolFilter {
    /// Create a new tool filter from optional allow/deny lists.
    ///
    /// Returns `None` if both lists are empty (no filtering).
    pub fn new(
        allowed: Option<Vec<String>>,
        disallowed: Option<Vec<String>>,
    ) -> Option<Self> {
        if allowed.is_none() && disallowed.is_none() {
            return None;
        }
        Some(Self { allowed, disallowed })
    }

    /// Check if a tool name is allowed by this filter.
    ///
    /// Logic: allowlist takes precedence. If allowlist is set, the tool
    /// must be in it. If only denylist is set, the tool must NOT be in it.
    pub fn is_allowed(&self, name: &str) -> bool {
        if let Some(ref allowed) = self.allowed {
            return allowed.iter().any(|a| a == name);
        }
        if let Some(ref disallowed) = self.disallowed {
            return !disallowed.iter().any(|d| d == name);
        }
        true
    }
}

/// Unified tool router — dispatches tool calls to built-in tools or MCP servers.
///
/// This component owns all tool sources (built-in registry + optional MCP manager)
/// and provides a single interface for:
/// - Aggregating tool definitions from all sources
/// - Routing tool calls to the correct handler
/// - Generating tool descriptions for CLI display
///
/// Routing priority: built-in tools first (including skills/subagents), then MCP servers.
pub struct ToolRouter {
    /// Built-in tools (filesystem, skills, subagents, etc.) — always available.
    builtin: BuiltinToolRegistry,
    /// Optional MCP manager for external tool servers.
    mcp: Option<McpManager>,
    /// Skill registry (shared via Arc so SkillTool can reference it).
    skills: Arc<SkillRegistry>,
    /// Subagent registry (shared via Arc so SubagentTool can reference it).
    subagents: Arc<SubagentRegistry>,
    /// Shared event callback for subagent tool execution progress.
    /// The REPL sets this before each chat call so subagent tool events
    /// are rendered in real-time.
    subagent_event_callback: SharedEventCallback,
    /// Optional tool filter (from --allowed-tools / --disallowed-tools).
    tool_filter: Option<ToolFilter>,
}

impl ToolRouter {
    /// Create a new tool router with built-in tools and no MCP servers.
    pub fn new() -> Self {
        Self {
            builtin: BuiltinToolRegistry::new(),
            mcp: None,
            skills: Arc::new(SkillRegistry::new()),
            subagents: Arc::new(SubagentRegistry::new()),
            subagent_event_callback: Arc::new(std::sync::RwLock::new(None)),
            tool_filter: None,
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

    /// Load subagent definitions from directories and register as a built-in tool.
    ///
    /// Subagents are exposed to the LLM as a single `spawn_subagent` tool.
    /// The LLM decides which subagent to invoke based on the subagent
    /// descriptions embedded in the tool definition.
    ///
    /// Loading order determines priority: project-level agents override
    /// global agents with the same name.
    pub fn load_subagents(
        &mut self,
        dirs: &[&Path],
        sources: &[crate::subagent::SubagentSource],
        parent_llm_config: LlmConfig,
    ) -> Result<usize> {
        let mut registry = SubagentRegistry::new();

        // Register built-in subagents first (lowest priority).
        // User-defined agents loaded below will override builtins with the same name.
        registry.register_builtins();

        for (dir, source) in dirs.iter().zip(sources.iter()) {
            registry.load_from_dir(dir, source.clone())?;
        }

        // Use agent_count() for the actual number of unique agents after
        // deduplication (builtins may be overridden by user-defined agents).
        let actual_count = registry.agent_count();

        if actual_count > 0 {
            let registry = Arc::new(registry);
            let runner = Arc::new(SubagentRunner::new(parent_llm_config));

            // Register the SubagentTool as a regular built-in tool
            if let Some(subagent_tool) = crate::subagent::tool::build_subagent_tool(
                &registry,
                Arc::clone(&runner),
                Arc::clone(&self.subagent_event_callback),
            ) {
                self.builtin.register_tool(subagent_tool);
            }

            // Register the TeamTool for parallel multi-agent execution
            // NOTE: `spawn_team` is temporarily disabled. Keep the code here
            // so it can be re-enabled by uncommenting this block.
            // if let Some(team_tool) = crate::subagent::tool::build_team_tool(
            //     &registry,
            //     Arc::clone(&runner),
            //     Arc::clone(&self.subagent_event_callback),
            // ) {
            //     self.builtin.register_tool(team_tool);
            // }
            self.subagents = registry;
            tracing::info!(
                subagents = actual_count,
                "Subagents loaded into ToolRouter as built-in tool"
            );
        }

        Ok(actual_count)
    }

    /// Return the subagent registry reference (for CLI display).
    pub fn subagent_registry(&self) -> &SubagentRegistry {
        &self.subagents
    }

    /// Set the subagent event callback for real-time progress display.
    ///
    /// Called by the REPL before each chat call to bind the callback
    /// to the current spinner. The callback is cleared after the call.
    pub fn set_subagent_event_callback(&self, callback: Option<crate::tools::ToolEventCallback>) {
        if let Ok(mut guard) = self.subagent_event_callback.write() {
            *guard = callback;
        }
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
    /// If a tool filter is active, only matching tools are included.
    pub fn build_tool_definitions(&self) -> Vec<serde_json::Value> {
        let mut definitions = self.builtin.build_tool_definitions();
        if let Some(ref mcp) = self.mcp {
            definitions.extend(mcp.build_tool_definitions());
        }

        // Apply tool filter if active
        if let Some(ref filter) = self.tool_filter {
            definitions.retain(|def| {
                let name = def
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("");
                filter.is_allowed(name)
            });
        }

        definitions
    }

    /// Check if a tool is a built-in tool (as opposed to an MCP tool).
    pub fn is_builtin(&self, name: &str) -> bool {
        self.builtin.has_tool(name)
    }

    /// Set a tool filter for --allowed-tools / --disallowed-tools.
    ///
    /// When set, only tools matching the filter are exposed to the LLM
    /// and allowed to execute.
    pub fn set_tool_filter(&mut self, filter: Option<ToolFilter>) {
        if let Some(ref f) = filter {
            tracing::info!(?f, "Tool filter activated");
        }
        self.tool_filter = filter;
    }

    /// Return tool metadata for CLI display and prompt building.
    pub fn tool_infos(&self) -> Vec<ToolInfo> {
        let mut infos = self.builtin.tool_infos();
        if let Some(ref mcp) = self.mcp {
            infos.extend(mcp.tool_infos());
        }
        // Apply tool filter if active
        if let Some(ref filter) = self.tool_filter {
            infos.retain(|info| filter.is_allowed(&info.name));
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

        // Check tool filter before execution
        if let Some(ref filter) = self.tool_filter {
            if !filter.is_allowed(&tool_call.function_name) {
                tracing::warn!(
                    tool = %tool_call.function_name,
                    "Tool call blocked by filter"
                );
                return ToolResponse::error(
                    &tool_call.call_id,
                    format!("Error: Tool '{}' is not allowed by the current tool filter", tool_call.function_name),
                );
            }
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_filter_none_when_both_empty() {
        let filter = ToolFilter::new(None, None);
        assert!(filter.is_none());
    }

    #[test]
    fn test_tool_filter_allowlist() {
        let filter = ToolFilter::new(
            Some(vec!["read_file".to_string(), "bash".to_string()]),
            None,
        ).unwrap();
        assert!(filter.is_allowed("read_file"));
        assert!(filter.is_allowed("bash"));
        assert!(!filter.is_allowed("write_file"));
        assert!(!filter.is_allowed("unknown"));
    }

    #[test]
    fn test_tool_filter_denylist() {
        let filter = ToolFilter::new(
            None,
            Some(vec!["bash".to_string(), "write_file".to_string()]),
        ).unwrap();
        assert!(filter.is_allowed("read_file"));
        assert!(!filter.is_allowed("bash"));
        assert!(!filter.is_allowed("write_file"));
        assert!(filter.is_allowed("list_directory"));
    }

    #[test]
    fn test_tool_filter_allowlist_takes_precedence() {
        // When both are set, allowlist takes precedence
        let filter = ToolFilter::new(
            Some(vec!["read_file".to_string()]),
            Some(vec!["read_file".to_string()]),
        ).unwrap();
        // read_file is in allowlist, so it's allowed (allowlist wins)
        assert!(filter.is_allowed("read_file"));
        // bash is not in allowlist, so it's blocked
        assert!(!filter.is_allowed("bash"));
    }
}
