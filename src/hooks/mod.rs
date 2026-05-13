//! Hooks system — user-configurable lifecycle hooks for tool calls.
//!
//! Hooks allow users to run custom shell commands at specific points in the
//! agent lifecycle. Inspired by Claude Code's hooks system.
//!
//! ## Supported Events
//!
//! - `PreToolUse`: Before a tool call is executed (can block execution)
//! - `PostToolUse`: After a tool call completes
//! - `SessionStart`: When a new session begins
//! - `Stop`: When the agent finishes responding
//!
//! ## Configuration
//!
//! Hooks are configured in `daedalus.yaml`:
//!
//! ```yaml
//! hooks:
//!   pre_tool_use:
//!     - matcher: "edit_file|write_file|multi_edit"
//!       command: "echo 'Editing: $TOOL_INPUT' >> ~/.daedalus/audit.log"
//!   post_tool_use:
//!     - matcher: "edit_file|write_file|multi_edit"
//!       command: "prettier --write $(echo $TOOL_INPUT | jq -r '.path') 2>/dev/null || true"
//! ```
//!
//! ## Environment Variables
//!
//! Hooks receive context via environment variables:
//! - `DAEDALUS_TOOL_NAME`: The tool being called
//! - `DAEDALUS_TOOL_INPUT`: JSON string of tool arguments
//! - `DAEDALUS_TOOL_OUTPUT`: Tool result (PostToolUse only)
//! - `DAEDALUS_TOOL_SUCCESS`: "true" or "false" (PostToolUse only)
//! - `DAEDALUS_SESSION_ID`: Current session ID

pub mod config;
pub mod executor;
pub mod middleware;
