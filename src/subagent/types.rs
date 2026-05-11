//! Core type definitions for the subagent subsystem.
//!
//! This module contains all data structures, enums, and their trait
//! implementations used across the subagent subsystem. Keeping types
//! in a dedicated module avoids bloating `mod.rs` and provides a
//! single source of truth for the data model.

/// Metadata about a subagent for display and LLM routing.
#[derive(Debug, Clone)]
pub struct SubagentInfo {
    /// Unique subagent name (lowercase + hyphens).
    pub name: String,
    /// Human-readable description (used by LLM for routing decisions).
    pub description: String,
    /// Source location of this subagent definition.
    #[allow(dead_code)]
    pub source: SubagentSource,
}

/// A fully loaded subagent definition.
#[derive(Debug, Clone)]
pub struct SubagentDefinition {
    /// Unique identifier name (lowercase + hyphens, e.g., "code-reviewer").
    pub name: String,
    /// Description — the most important field. Determines when the LLM
    /// automatically dispatches tasks to this subagent.
    pub description: String,
    /// System prompt for the subagent (Markdown body after frontmatter).
    /// The subagent receives ONLY this prompt, not the main agent's system prompt.
    pub system_prompt: String,
    /// Model to use. `None` means inherit from the parent agent.
    /// Accepted values: "haiku", "sonnet", "opus", or a full model ID.
    pub model: Option<String>,
    /// Tool whitelist. When set, the subagent can ONLY use these tools.
    /// MCP tools are excluded in whitelist mode.
    pub tools: Option<Vec<String>>,
    /// Tool blacklist. The subagent inherits all tools EXCEPT these.
    /// MCP tools are preserved in blacklist mode.
    pub disallowed_tools: Option<Vec<String>>,
    /// Permission mode controlling how the subagent handles authorization.
    #[allow(dead_code)]
    pub permission_mode: PermissionMode,
    /// Maximum number of tool-calling rounds (overrides the default).
    pub max_turns: Option<usize>,
    /// Where this definition was loaded from (affects priority).
    pub source: SubagentSource,
    /// Isolation mode for the subagent's execution environment.
    pub isolation: IsolationMode,
    /// Shell command to run before the subagent starts execution.
    /// The command receives the task description via stdin.
    pub on_start: Option<String>,
    /// Shell command to run after the subagent completes execution.
    /// The command receives the subagent result via stdin.
    pub on_complete: Option<String>,
    /// Optional shared context injected by the orchestrator.
    ///
    /// When set, this read-only context is included in the subagent's system
    /// prompt as a `<shared_context>` section. It typically contains:
    /// - Project architecture overview
    /// - Key shared types and traits
    /// - Cross-module dependency map
    ///
    /// This breaks the information silo between parallel subagents by giving
    /// each one a common understanding of the project. Not persisted to disk;
    /// only set at runtime by the orchestrator when spawning subagents.
    pub shared_context: Option<String>,
}

/// Permission mode for subagent tool execution.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PermissionMode {
    /// Normal permission prompts (default).
    #[default]
    Default,
    /// Automatically accept file edits without confirmation.
    AcceptEdits,
    /// Silently reject unauthorized operations (no interruption).
    DontAsk,
    /// Skip all permission checks (use with caution).
    BypassPermissions,
    /// Read-only planning mode — no write operations.
    Plan,
}

impl std::str::FromStr for PermissionMode {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "default" => Ok(Self::Default),
            "acceptedits" | "accept_edits" | "accept-edits" => Ok(Self::AcceptEdits),
            "dontask" | "dont_ask" | "dont-ask" => Ok(Self::DontAsk),
            "bypasspermissions" | "bypass_permissions" | "bypass-permissions" => {
                Ok(Self::BypassPermissions)
            }
            "plan" => Ok(Self::Plan),
            _ => Err(format!(
                "Invalid permission mode: '{}'. Expected: default, acceptEdits, dontAsk, bypassPermissions, plan",
                s
            )),
        }
    }
}

impl std::fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Default => write!(f, "default"),
            Self::AcceptEdits => write!(f, "acceptEdits"),
            Self::DontAsk => write!(f, "dontAsk"),
            Self::BypassPermissions => write!(f, "bypassPermissions"),
            Self::Plan => write!(f, "plan"),
        }
    }
}

/// Where a subagent definition was loaded from.
///
/// Variants are ordered by priority (lowest → highest).
/// Higher-priority sources override lower-priority ones when names conflict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentSource {
    /// Built-in: hardcoded in the binary (lowest priority).
    Builtin,
    /// Global: `~/.daedalus/agents/` (medium priority).
    Global,
    /// Project-level: `.daedalus/agents/` (highest priority).
    Project,
}

impl std::fmt::Display for SubagentSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Builtin => write!(f, "built-in"),
            Self::Project => write!(f, "project"),
            Self::Global => write!(f, "global"),
        }
    }
}

/// Isolation mode for subagent execution.
///
/// Controls how the subagent's file operations are isolated from
/// the main workspace.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum IsolationMode {
    /// No isolation — subagent operates directly on the workspace (default).
    #[default]
    None,
    /// Git worktree isolation — subagent operates on a temporary git worktree.
    /// Changes are isolated until explicitly merged back.
    Worktree,
}

impl std::str::FromStr for IsolationMode {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "none" => Ok(Self::None),
            "worktree" => Ok(Self::Worktree),
            _ => Err(format!(
                "Invalid isolation mode: '{}'. Expected: none, worktree",
                s
            )),
        }
    }
}

impl std::fmt::Display for IsolationMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "none"),
            Self::Worktree => write!(f, "worktree"),
        }
    }
}

/// Result returned after a subagent completes its task.
#[derive(Debug, Clone)]
pub struct SubagentResult {
    /// Name of the subagent that executed the task.
    pub agent_name: String,
    /// Final output content from the subagent.
    pub content: String,
    /// Token usage statistics (accumulated across all tool rounds).
    pub usage: Option<crate::llm::TokenUsage>,
    /// Number of tool-calling rounds executed.
    pub tool_rounds: usize,
}

/// A task assignment for a team member in a parallel team execution.
///
/// Only compiled in when the `team` feature is enabled.
#[cfg(feature = "team")]
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TeamTask {
    /// The subagent name to assign this task to.
    pub agent_name: String,
    /// The task description for this subagent.
    pub task: String,
}
