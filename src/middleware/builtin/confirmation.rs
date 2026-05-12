
//! Confirmation middleware — interactive user approval for sensitive tool calls.
//!
//! This middleware classifies tool calls by risk level and pauses execution
//! to request user confirmation for operations that modify state or could be
//! dangerous. It integrates with the CLI layer via async channels.
//!
//! ## Risk Levels
//!
//! - `ReadOnly`: Safe operations that never require confirmation (read_file, grep, etc.)
//! - `Sensitive`: State-modifying operations that require confirmation unless pre-approved
//!   (edit_file, write_file, multi_edit)
//! - `Dangerous`: Potentially destructive operations that always require confirmation
//!   (bash, MCP tools)
//!
//! ## Rule Engine (Phase 2)
//!
//! Before prompting the user, the middleware checks persisted permission rules:
//! - Global rules: `~/.daedalus/permissions.json`
//! - Project rules: `.daedalus/permissions.json`
//! - Session rules: in-memory (from "Allow for session" choices)
//!
//! Rules support glob patterns (e.g. `bash` + `git *` allows all git commands).
//!
//! ## Bypass
//!
//! The `--dangerously-skip-permissions` CLI flag bypasses all confirmations.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::llm::ToolResponse;

use super::super::{ToolMiddleware, ToolNext, ToolRequest};
use super::permission_rules::{
    PermissionRule, PermissionRuleSet, PermissionsConfig, PermissionMode,
    RuleDecision, RuleScope,
    extract_match_value, suggest_pattern,
};

// ── Risk Classification ──

/// Risk level for a tool call, determining whether user confirmation is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRiskLevel {
    /// Safe read-only operations — never require confirmation.
    ReadOnly,
    /// State-modifying operations — require confirmation unless pre-approved.
    Sensitive,
    /// Potentially destructive operations — always require confirmation
    /// unless bypassed globally.
    Dangerous,
}

/// A request sent to the CLI layer asking for user confirmation.
#[derive(Debug)]
pub struct ConfirmationRequest {
    /// The tool name being called.
    pub tool_name: String,
    /// The source of the tool ("built-in" or MCP server name).
    #[allow(dead_code)]
    pub source: String,
    /// Human-readable description of what the tool will do.
    pub description: String,
    /// The risk level classification.
    pub risk_level: ToolRiskLevel,
    /// Suggested pattern for "Always Allow" (e.g. "git *" for bash).
    /// `None` if no meaningful pattern can be suggested.
    pub suggested_pattern: Option<String>,
    /// Channel to send the user's decision back.
    pub response_tx: oneshot::Sender<UserDecision>,
}

/// The user's decision after seeing a confirmation prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserDecision {
    /// Allow this single invocation.
    AllowOnce,
    /// Allow this tool for the rest of the session.
    AllowSession,
    /// Always allow this tool+pattern (persisted to rules file).
    AlwaysAllow {
        /// The scope to persist the rule to.
        scope: RuleScope,
        /// Optional pattern to match (e.g. "git *" for bash).
        /// If None, allows all invocations of the tool.
        pattern: Option<String>,
    },
    /// Deny this invocation.
    Deny,
}

/// Channel type for sending confirmation requests to the CLI layer.
pub type ConfirmationSender = mpsc::UnboundedSender<ConfirmationRequest>;
/// Channel type for receiving confirmation requests in the CLI layer.
pub type ConfirmationReceiver = mpsc::UnboundedReceiver<ConfirmationRequest>;

/// Create a new confirmation channel pair.
pub fn confirmation_channel() -> (ConfirmationSender, ConfirmationReceiver) {
    mpsc::unbounded_channel()
}

// ── Tool Classifier ──

/// Tools that are always considered read-only (safe, no confirmation needed).
const READ_ONLY_TOOLS: &[&str] = &[
    "read_file",
    "list_directory",
    "search_files",
    "grep_search",
    "get_file_info",
    "recall_history",
    "codebase_search",
    "view_code_item",
    "take_note",
];

/// Tools that modify state but are predictable (confirmation needed, can be pre-approved).
const SENSITIVE_TOOLS: &[&str] = &[
    "edit_file",
    "multi_edit",
    "write_file",
];

/// Tools that are inherently dangerous (arbitrary execution, always confirm).
const DANGEROUS_TOOLS: &[&str] = &[
    "bash",
];

/// Classify a tool call's risk level.
fn classify_tool(tool_name: &str, source: &str) -> ToolRiskLevel {
    // All MCP (external) tools are considered dangerous by default
    if source != "built-in" {
        return ToolRiskLevel::Dangerous;
    }

    if READ_ONLY_TOOLS.contains(&tool_name) {
        return ToolRiskLevel::ReadOnly;
    }

    if DANGEROUS_TOOLS.contains(&tool_name) {
        return ToolRiskLevel::Dangerous;
    }

    if SENSITIVE_TOOLS.contains(&tool_name) {
        return ToolRiskLevel::Sensitive;
    }

    // Unknown built-in tools (e.g. dynamically registered): treat as Sensitive
    ToolRiskLevel::Sensitive
}

/// Generate a human-readable description of what the tool call will do.
fn describe_tool_call(tool_name: &str, arguments: &serde_json::Value) -> String {
    match tool_name {
        "bash" => {
            let cmd = arguments
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("<unknown command>");
            format!("$ {}", cmd)
        }
        "edit_file" | "multi_edit" => {
            let path = arguments
                .get("path")
                .or_else(|| arguments.get("file_path"))
                .and_then(|v| v.as_str())
                .unwrap_or("<unknown file>");
            format!("modify {}", path)
        }
        "write_file" => {
            let path = arguments
                .get("path")
                .or_else(|| arguments.get("file_path"))
                .and_then(|v| v.as_str())
                .unwrap_or("<unknown file>");
            format!("write to {}", path)
        }
        _ => {
            // For MCP or unknown tools, show the tool name and first argument
            let preview = arguments
                .as_object()
                .and_then(|obj| obj.iter().next())
                .map(|(k, v)| {
                    let val_str = match v {
                        serde_json::Value::String(s) => {
                            if s.len() > 60 {
                                format!("{}...", &s[..57])
                            } else {
                                s.clone()
                            }
                        }
                        other => {
                            let s = other.to_string();
                            if s.len() > 60 {
                                format!("{}...", &s[..57])
                            } else {
                                s
                            }
                        }
                    };
                    format!("{}: {}", k, val_str)
                })
                .unwrap_or_default();
            if preview.is_empty() {
                format!("call {}", tool_name)
            } else {
                format!("call {} ({})", tool_name, preview)
            }
        }
    }
}

// ── Confirmation Middleware ──

/// Tool-level confirmation middleware.
///
/// Intercepts tool calls that require user approval, sends a confirmation
/// request to the CLI layer, and waits for the user's decision before
/// proceeding or rejecting the call.
pub struct ConfirmationToolMiddleware {
    /// Channel to send confirmation requests to the CLI.
    confirm_tx: ConfirmationSender,
    /// Whether to bypass all confirmations (--dangerously-skip-permissions).
    bypass_all: bool,
    /// Session-level approved tools (tool names approved via "Allow for session").
    session_approved: Arc<Mutex<HashSet<String>>>,
    /// Permission rules engine (glob-based, persisted).
    rules: Arc<Mutex<PermissionRuleSet>>,
    /// Permission mode from YAML config.
    permission_mode: PermissionMode,
}

impl ConfirmationToolMiddleware {
    /// Create a new confirmation middleware (without pre-loaded rules).
    ///
    /// - `confirm_tx`: Channel to send confirmation requests to the CLI layer.
    /// - `bypass_all`: If true, all confirmations are skipped (unsafe mode).
    #[allow(dead_code)]
    pub fn new(confirm_tx: ConfirmationSender, bypass_all: bool) -> Self {
        Self {
            confirm_tx,
            bypass_all,
            session_approved: Arc::new(Mutex::new(HashSet::new())),
            rules: Arc::new(Mutex::new(PermissionRuleSet::new())),
            permission_mode: PermissionMode::Default,
        }
    }

    /// Create a new confirmation middleware with pre-loaded rules.
    ///
    /// - `confirm_tx`: Channel to send confirmation requests to the CLI layer.
    /// - `bypass_all`: If true, all confirmations are skipped (unsafe mode).
    /// - `workspace_root`: The workspace root for loading project-level rules.
    #[allow(dead_code)]
    pub fn with_rules(
        confirm_tx: ConfirmationSender,
        bypass_all: bool,
        workspace_root: Option<&Path>,
    ) -> Self {
        let rules = PermissionRuleSet::load(workspace_root);
        Self {
            confirm_tx,
            bypass_all,
            session_approved: Arc::new(Mutex::new(HashSet::new())),
            rules: Arc::new(Mutex::new(rules)),
            permission_mode: PermissionMode::Default,
        }
    }

    /// Create a new confirmation middleware with full YAML configuration.
    ///
    /// Loads rules from both persisted files and YAML config, and applies
    /// the configured permission mode.
    #[allow(dead_code)]
    pub fn with_config(
        confirm_tx: ConfirmationSender,
        bypass_all: bool,
        workspace_root: Option<&Path>,
        config: &PermissionsConfig,
    ) -> Self {
        let rules = PermissionRuleSet::load_with_config(workspace_root, config);
        let permission_mode = if bypass_all {
            PermissionMode::BypassPermissions
        } else {
            config.mode.clone()
        };
        Self {
            confirm_tx,
            bypass_all: bypass_all || permission_mode == PermissionMode::BypassPermissions,
            session_approved: Arc::new(Mutex::new(HashSet::new())),
            rules: Arc::new(Mutex::new(rules)),
            permission_mode,
        }
    }

    /// Create a new confirmation middleware with externally-owned shared state.
    ///
    /// This constructor accepts pre-existing `session_approved` and `rules`
    /// `Arc`s so that the same state is shared across multiple pipeline
    /// rebuilds (each `chat()` call creates a new `CoreTurnHandler` and
    /// therefore a new `ConfirmationToolMiddleware`). Without sharing,
    /// session-level approvals and dynamically-added rules would be lost
    /// after every turn.
    pub fn with_shared_state(
        confirm_tx: ConfirmationSender,
        bypass_all: bool,
        session_approved: Arc<Mutex<HashSet<String>>>,
        rules: Arc<Mutex<PermissionRuleSet>>,
        permission_mode: PermissionMode,
    ) -> Self {
        Self {
            confirm_tx,
            bypass_all: bypass_all || permission_mode == PermissionMode::BypassPermissions,
            session_approved,
            rules,
            permission_mode,
        }
    }

    /// Return a reference to the rules engine (for /permissions display).
    #[allow(dead_code)]
    pub fn rules(&self) -> &Arc<Mutex<PermissionRuleSet>> {
        &self.rules
    }

    /// Check if a tool has been session-approved.
    async fn is_session_approved(&self, tool_name: &str) -> bool {
        self.session_approved.lock().await.contains(tool_name)
    }

    /// Mark a tool as session-approved.
    async fn approve_for_session(&self, tool_name: &str) {
        self.session_approved.lock().await.insert(tool_name.to_string());
    }
}

#[async_trait]
impl ToolMiddleware for ConfirmationToolMiddleware {
    async fn handle(
        &self,
        request: ToolRequest,
        next: &dyn ToolNext,
    ) -> ToolResponse {
        // Bypass mode — skip all confirmations
        if self.bypass_all {
            return next.run(request).await;
        }

        let tool_name = &request.call.function_name;
        let source = &request.source;

        // Classify the risk level
        let risk = classify_tool(tool_name, source);

        // ReadOnly tools never need confirmation
        if risk == ToolRiskLevel::ReadOnly {
            return next.run(request).await;
        }

        // Apply permission mode policy
        match (&self.permission_mode, risk) {
            // AcceptEdits: Sensitive tools auto-approved
            (PermissionMode::AcceptEdits, ToolRiskLevel::Sensitive) => {
                return next.run(request).await;
            }
            // Plan mode: deny all non-ReadOnly tools
            (PermissionMode::Plan, _) => {
                return ToolResponse::error(
                    &request.call.call_id,
                    format!(
                        "Permission denied: tool '{}' is not allowed in plan (read-only) mode.",
                        tool_name
                    ),
                );
            }
            // Default and other modes: continue to rules + confirmation
            _ => {}
        }

        // Check session-level approval (fast path, no lock contention with rules)
        if self.is_session_approved(tool_name).await {
            return next.run(request).await;
        }

        // Extract the match value for rule evaluation
        let match_value = extract_match_value(tool_name, &request.call.arguments);

        // Check permission rules (session → project → global)
        {
            let rules = self.rules.lock().await;
            match rules.evaluate(tool_name, match_value.as_deref()) {
                Some(RuleDecision::Allow) => {
                    tracing::debug!(
                        tool = %tool_name,
                        "Tool call allowed by permission rule"
                    );
                    return next.run(request).await;
                }
                Some(RuleDecision::Deny) => {
                    tracing::info!(
                        tool = %tool_name,
                        "Tool call denied by permission rule"
                    );
                    return ToolResponse::error(
                        &request.call.call_id,
                        format!(
                            "Permission denied: tool '{}' is blocked by a permission rule.",
                            tool_name
                        ),
                    );
                }
                None => {
                    // No matching rule — fall through to user confirmation
                }
            }
        }

        // Generate a suggested pattern for "Always Allow"
        let suggested_pattern = suggest_pattern(tool_name, &request.call.arguments);

        // Build a description of what the tool will do
        let description = describe_tool_call(tool_name, &request.call.arguments);

        // Create a oneshot channel for the response
        let (response_tx, response_rx) = oneshot::channel();

        let confirm_request = ConfirmationRequest {
            tool_name: tool_name.clone(),
            source: source.clone(),
            description,
            risk_level: risk,
            suggested_pattern,
            response_tx,
        };

        // Send the confirmation request to the CLI layer
        if self.confirm_tx.send(confirm_request).is_err() {
            // If the receiver is dropped (e.g., non-interactive mode), deny by default
            tracing::warn!(
                tool = %tool_name,
                "Confirmation channel closed — denying tool call (non-interactive mode)"
            );
            return ToolResponse::error(
                &request.call.call_id,
                format!(
                    "Permission denied: tool '{}' requires user confirmation but no interactive \
                     session is available. Use --dangerously-skip-permissions to bypass.",
                    tool_name
                ),
            );
        }

        // Wait for the user's decision
        match response_rx.await {
            Ok(UserDecision::AllowOnce) => {
                tracing::info!(tool = %tool_name, "User approved tool call (once)");
                next.run(request).await
            }
            Ok(UserDecision::AllowSession) => {
                tracing::info!(tool = %tool_name, "User approved tool call (session)");
                self.approve_for_session(tool_name).await;
                next.run(request).await
            }
            Ok(UserDecision::AlwaysAllow { scope, pattern }) => {
                tracing::info!(
                    tool = %tool_name,
                    ?scope,
                    ?pattern,
                    "User approved tool call (always allow — persisting rule)"
                );
                // Add the rule to the rules engine
                let rule = PermissionRule {
                    tool: tool_name.clone(),
                    pattern,
                    decision: RuleDecision::Allow,
                };
                self.rules.lock().await.add_rule(rule, scope);
                next.run(request).await
            }
            Ok(UserDecision::Deny) => {
                tracing::info!(tool = %tool_name, "User denied tool call");
                ToolResponse::error(
                    &request.call.call_id,
                    format!(
                        "Permission denied: user rejected the '{}' tool call. \
                         The operation was not executed.",
                        tool_name
                    ),
                )
            }
            Err(_) => {
                // Sender was dropped (shouldn't happen in normal flow)
                tracing::warn!(tool = %tool_name, "Confirmation response channel dropped");
                ToolResponse::error(
                    &request.call.call_id,
                    format!(
                        "Permission denied: confirmation for '{}' was interrupted.",
                        tool_name
                    ),
                )
            }
        }
    }

    fn name(&self) -> &str {
        "confirmation"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_read_only_tools() {
        assert_eq!(classify_tool("read_file", "built-in"), ToolRiskLevel::ReadOnly);
        assert_eq!(classify_tool("list_directory", "built-in"), ToolRiskLevel::ReadOnly);
        assert_eq!(classify_tool("grep_search", "built-in"), ToolRiskLevel::ReadOnly);
        assert_eq!(classify_tool("search_files", "built-in"), ToolRiskLevel::ReadOnly);
        assert_eq!(classify_tool("get_file_info", "built-in"), ToolRiskLevel::ReadOnly);
        assert_eq!(classify_tool("take_note", "built-in"), ToolRiskLevel::ReadOnly);
    }

    #[test]
    fn test_classify_sensitive_tools() {
        assert_eq!(classify_tool("edit_file", "built-in"), ToolRiskLevel::Sensitive);
        assert_eq!(classify_tool("multi_edit", "built-in"), ToolRiskLevel::Sensitive);
        assert_eq!(classify_tool("write_file", "built-in"), ToolRiskLevel::Sensitive);
    }

    #[test]
    fn test_classify_dangerous_tools() {
        assert_eq!(classify_tool("bash", "built-in"), ToolRiskLevel::Dangerous);
    }

    #[test]
    fn test_classify_mcp_tools_always_dangerous() {
        assert_eq!(classify_tool("any_tool", "my-mcp-server"), ToolRiskLevel::Dangerous);
        assert_eq!(classify_tool("read_file", "external-server"), ToolRiskLevel::Dangerous);
    }

    #[test]
    fn test_classify_unknown_builtin_as_sensitive() {
        assert_eq!(classify_tool("some_new_tool", "built-in"), ToolRiskLevel::Sensitive);
    }

    #[test]
    fn test_describe_bash() {
        let args = serde_json::json!({"command": "git status"});
        assert_eq!(describe_tool_call("bash", &args), "$ git status");
    }

    #[test]
    fn test_describe_edit_file() {
        let args = serde_json::json!({"path": "/src/main.rs"});
        assert_eq!(describe_tool_call("edit_file", &args), "modify /src/main.rs");
    }

    #[test]
    fn test_describe_write_file() {
        let args = serde_json::json!({"path": "/tmp/output.txt"});
        assert_eq!(describe_tool_call("write_file", &args), "write to /tmp/output.txt");
    }

    #[test]
    fn test_describe_unknown_tool() {
        let args = serde_json::json!({"query": "hello world"});
        let desc = describe_tool_call("custom_tool", &args);
        assert!(desc.contains("custom_tool"));
        assert!(desc.contains("query"));
    }

    #[tokio::test]
    async fn test_bypass_mode_skips_confirmation() {
        let (tx, _rx) = confirmation_channel();
        let mw = ConfirmationToolMiddleware::new(tx, true);
        // bypass_all = true means no confirmation needed
        assert!(mw.bypass_all);
    }

    #[tokio::test]
    async fn test_session_approval() {
        let (tx, _rx) = confirmation_channel();
        let mw = ConfirmationToolMiddleware::new(tx, false);

        assert!(!mw.is_session_approved("bash").await);
        mw.approve_for_session("bash").await;
        assert!(mw.is_session_approved("bash").await);
    }
}
