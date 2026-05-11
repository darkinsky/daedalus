use std::sync::Arc;

use anyhow::Result;

use crate::llm::{LlmConfig, ToolCall, ToolResponse};
use crate::mcp::McpManager;
use crate::skill::SkillRegistry;
use crate::subagent::{SubagentRegistry, SubagentRunner};
use crate::subagent::tool::SubagentEventSink;
use crate::agent_tracing::SharedTracingHook;
use crate::tools::{BuiltinToolRegistry, ToolInfo};
use crate::tools::bash::BashConfig;

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
    /// Shared event sink for subagent tool execution progress.
    /// The REPL sets this before each chat call so subagent tool events
    /// are rendered in real-time.
    subagent_event_sink: SubagentEventSink,
    /// Shared tracing hook for subagent span creation.
    /// Set by ChatAgent before each chat call so subagent tool calls
    /// create spans nested under the main trace.
    shared_tracing_hook: SharedTracingHook,
    /// Optional tool filter (from --allowed-tools / --disallowed-tools).
    tool_filter: Option<ToolFilter>,
}

impl ToolRouter {
    /// Create a new tool router with built-in tools and no MCP servers.
    pub fn new() -> Self {
        Self::with_bash_config(BashConfig::default())
    }

    /// Create a new tool router with custom bash tool configuration.
    pub fn with_bash_config(bash_config: BashConfig) -> Self {
        Self {
            builtin: BuiltinToolRegistry::new_with_config(bash_config),
            mcp: None,
            skills: Arc::new(SkillRegistry::new()),
            subagents: Arc::new(SubagentRegistry::new()),
            subagent_event_sink: SubagentEventSink::new(),
            shared_tracing_hook: SharedTracingHook::new(),
            tool_filter: None,
        }
    }

    /// Replace the bash tool with one using the given configuration.
    ///
    /// Called during bootstrap when the user has configured custom bash
    /// settings (timeout, output limits) in the `tools.bash` YAML section.
    pub fn replace_bash_config(&mut self, config: BashConfig) {
        self.builtin.replace_tool(Box::new(crate::tools::bash::BashTool::new(config)));
    }

    /// Install a ready-to-use skill registry into the router.
    ///
    /// The registry must already contain any skills the caller wants
    /// exposed — this method only handles the last mile of wiring a
    /// `use_skill` `BuiltinTool` into the underlying `BuiltinToolRegistry`.
    ///
    /// This deliberately knows nothing about the filesystem: loading is
    /// the registry's job (`SkillRegistry::load_from_dir`). Splitting
    /// the concerns keeps the router focused on routing.
    ///
    /// No-op if the registry is empty.
    pub fn install_skills(&mut self, skills: Arc<SkillRegistry>) {
        if let Some(skill_tool) = skills.build_skill_tool() {
            self.builtin.register_tool(skill_tool);
            tracing::info!(
                skills = skills.skill_count(),
                "Skills installed into ToolRouter as built-in tool"
            );
        }
        self.skills = skills;
    }

    /// Install a ready-to-use subagent registry into the router.
    ///
    /// The registry must already contain every agent the caller wants
    /// exposed (builtins, project, global — loaded in whichever order
    /// the caller considers correct). This method only handles the
    /// last mile: building the `spawn_subagent` (and optionally
    /// `spawn_team`) `BuiltinTool`s and registering them.
    ///
    /// `parent_llm_config` seeds each subagent's LLM provider when it
    /// doesn't override the model itself.
    ///
    /// No-op if the registry is empty.
    pub fn install_subagents(
        &mut self,
        subagents: Arc<SubagentRegistry>,
        parent_llm_config: LlmConfig,
    ) {
        let count = subagents.agent_count();
        if count == 0 {
            self.subagents = subagents;
            return;
        }

        let runner = Arc::new(SubagentRunner::new(parent_llm_config));

        if let Some(subagent_tool) = crate::subagent::tool::build_subagent_tool(
            &subagents,
            Arc::clone(&runner),
            self.subagent_event_sink.clone(),
            self.shared_tracing_hook.clone(),
        ) {
            self.builtin.register_tool(subagent_tool);
        }

        // Register the TeamTool for parallel multi-agent execution.
        // Gated behind the `team` cargo feature (off by default).
        #[cfg(feature = "team")]
        if let Some(team_tool) = crate::subagent::tool::build_team_tool(
            &subagents,
            Arc::clone(&runner),
            self.subagent_event_sink.clone(),
            self.shared_tracing_hook.clone(),
        ) {
            self.builtin.register_tool(team_tool);
        }

        self.subagents = subagents;
        tracing::info!(
            subagents = count,
            "Subagents installed into ToolRouter as built-in tool"
        );
    }

    /// Register a single built-in tool dynamically.
    ///
    /// Used by the ACP integration to install the `call_acp_agent` tool
    /// at runtime after ACP agents are discovered.
    pub fn register_builtin_tool(&mut self, tool: Box<dyn crate::tools::BuiltinTool>) {
        tracing::info!(
            tool = tool.name(),
            "Registered dynamic tool into ToolRouter"
        );
        self.builtin.register_tool(tool);
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
        self.subagent_event_sink.set(callback);
    }

    /// Set the shared tracing hook for subagent span creation.
    ///
    /// Called by ChatAgent before each chat call to bind the trace context.
    /// The hook is cleared after the call completes.
    pub fn set_shared_tracing_hook(&self, ctx: Option<std::sync::Arc<crate::agent_tracing::TraceContext>>) {
        self.shared_tracing_hook.set(ctx);
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

    /// Return true if a tool name is allowed by the current filter.
    ///
    /// If no filter is active (the default), every name is allowed.
    /// This is the single source of truth consulted by every place that
    /// needs to make an allow/deny decision — `build_tool_definitions`,
    /// `tool_infos`, and `execute` all call it.
    fn is_tool_allowed(&self, name: &str) -> bool {
        self.tool_filter
            .as_ref()
            .map(|f| f.is_allowed(name))
            .unwrap_or(true)
    }

    /// Build tool definitions in OpenAI function-calling JSON format.
    ///
    /// Combines built-in (including skill) and MCP tool definitions into a single list.
    /// If a tool filter is active, only matching tools are included.
    ///
    /// **Cache stability**: The definitions are sorted alphabetically by tool name
    /// so that the tool section of the system prompt (which includes tool schemas)
    /// produces a deterministic token sequence across sessions. Without sorting,
    /// MCP/skill tools loaded in non-deterministic order would break prompt cache
    /// prefix stability. This mirrors Claude Code's `assembleToolPool()` design.
    pub fn build_tool_definitions(&self) -> Vec<serde_json::Value> {
        let mut definitions = self.builtin.build_tool_definitions();
        if let Some(ref mcp) = self.mcp {
            definitions.extend(mcp.build_tool_definitions());
        }

        definitions.retain(|def| {
            let name = def
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            self.is_tool_allowed(name)
        });

        // Sort by tool name for prompt cache stability.
        // Claude Code does this in assembleToolPool() to prevent MCP tool
        // registration order changes from invalidating the cached prefix.
        definitions.sort_by(|a, b| {
            let name_a = a.get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            let name_b = b.get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            name_a.cmp(name_b)
        });

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
        infos.retain(|info| self.is_tool_allowed(&info.name));
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
        if !self.is_tool_allowed(&tool_call.function_name) {
            tracing::warn!(
                tool = %tool_call.function_name,
                "Tool call blocked by filter"
            );
            return ToolResponse::error(
                &tool_call.call_id,
                format!("Error: Tool '{}' is not allowed by the current tool filter", tool_call.function_name),
            );
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
    /// Includes timing and result preview for observability.
    async fn execute_and_log(
        &self,
        call_id: &str,
        tool_name: &str,
        source: &str,
        fut: impl std::future::Future<Output = Result<String>>,
    ) -> ToolResponse {
        let start = std::time::Instant::now();
        let outcome = fut.await;
        let elapsed_ms = start.elapsed().as_millis() as u64;

        match outcome {
            Ok(result) => {
                // Truncate result preview for logging (avoid flooding logs)
                let preview = if result.len() > 500 {
                    format!("{}...(truncated, total {} bytes)", crate::tools::truncate_at_char_boundary(&result, 500), result.len())
                } else {
                    result.clone()
                };
                tracing::info!(
                    tool = %tool_name,
                    source = source,
                    result_len = result.len(),
                    elapsed_ms = elapsed_ms,
                    "Tool call succeeded"
                );
                tracing::debug!(
                    tool = %tool_name,
                    result_preview = %preview,
                    "Tool call result"
                );
                ToolResponse::new(call_id, result)
            }
            Err(e) => {
                tracing::error!(
                    tool = %tool_name,
                    error = %e,
                    source = source,
                    elapsed_ms = elapsed_ms,
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
