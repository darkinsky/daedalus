//! Subagent subsystem — isolated task delegation for LLM agents.
//!
//! ## Module structure
//!
//! - `types`     — Core data structures (SubagentDefinition, enums, etc.)
//! - `builtins`  — Built-in agent definitions hardcoded in the binary
//! - `loader`    — Filesystem loader for `.md` agent definitions
//! - `registry`  — Registration, lookup, and deduplication
//! - `runner`    — Core execution engine (LLM + tool loop)
//! - `isolation` — Git worktree isolation and lifecycle hooks
//! - `tool`      — BuiltinTool adapters (spawn_subagent, spawn_team)

pub(crate) mod builtins;
pub(crate) mod isolation;
pub(crate) mod loader;
pub mod registry;
pub(crate) mod runner;
pub(crate) mod tool;
pub(crate) mod types;

// Re-export core types for convenient access as `crate::subagent::*`
pub use registry::SubagentRegistry;
pub use runner::SubagentRunner;
pub use types::{
    IsolationMode, PermissionMode, SubagentDefinition, SubagentInfo, SubagentResult,
    SubagentSource, TeamTask,
};
