//! Hooks configuration — YAML-driven hook definitions.

use serde::Deserialize;

/// Top-level hooks configuration section in `daedalus.yaml`.
///
/// ```yaml
/// hooks:
///   pre_tool_use:
///     - matcher: "bash"
///       command: "echo \"$DAEDALUS_TOOL_NAME: $DAEDALUS_TOOL_INPUT\" >> /tmp/audit.log"
///       timeout_secs: 10
///   post_tool_use:
///     - matcher: "edit_file|write_file"
///       command: "prettier --write $(echo $DAEDALUS_TOOL_INPUT | jq -r '.path') 2>/dev/null || true"
///   session_start:
///     - command: "echo 'Session started' >> /tmp/daedalus.log"
///   stop:
///     - command: "echo 'Agent finished' >> /tmp/daedalus.log"
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HooksConfig {
    /// Hooks to run before a tool call is executed.
    /// If any hook exits with non-zero status, the tool call is blocked.
    pub pre_tool_use: Vec<HookEntry>,

    /// Hooks to run after a tool call completes.
    pub post_tool_use: Vec<HookEntry>,

    /// Hooks to run when a new session starts.
    pub session_start: Vec<HookEntry>,

    /// Hooks to run when the agent finishes responding.
    pub stop: Vec<HookEntry>,
}

impl HooksConfig {
    /// Check if any hooks are configured.
    pub fn is_empty(&self) -> bool {
        self.pre_tool_use.is_empty()
            && self.post_tool_use.is_empty()
            && self.session_start.is_empty()
            && self.stop.is_empty()
    }

    /// Get hooks for a specific event, filtered by tool name matcher.
    pub fn matching_hooks(&self, event: HookEvent, tool_name: Option<&str>) -> Vec<&HookEntry> {
        let hooks = match event {
            HookEvent::PreToolUse => &self.pre_tool_use,
            HookEvent::PostToolUse => &self.post_tool_use,
            HookEvent::SessionStart => &self.session_start,
            HookEvent::Stop => &self.stop,
        };

        hooks.iter().filter(|h| {
            match (&h.matcher, tool_name) {
                (Some(pattern), Some(name)) => matches_tool(pattern, name),
                (Some(_), None) => false, // Hook has matcher but no tool name provided
                (None, _) => true,        // No matcher = matches everything
            }
        }).collect()
    }
}

/// A single hook entry.
#[derive(Debug, Clone, Deserialize)]
pub struct HookEntry {
    /// Optional glob pattern to match tool names.
    /// If not set, the hook matches all tools.
    /// Supports `|` for alternation (e.g., "edit_file|write_file"),
    /// `*` for wildcard (e.g., "git*", "*_file", "*edit*").
    #[serde(default)]
    pub matcher: Option<String>,

    /// The shell command to execute.
    pub command: String,

    /// Timeout in seconds (default: 30).
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

/// Hook event types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum HookEvent {
    /// Before a tool call is executed.
    PreToolUse,
    /// After a tool call completes.
    PostToolUse,
    /// When a new session starts.
    SessionStart,
    /// When the agent finishes responding.
    Stop,
}

fn default_timeout() -> u64 {
    30
}

/// Check if a tool name matches a pattern.
///
/// Supports:
/// - `|` for alternation (e.g., "edit_file|write_file|multi_edit")
/// - `*` as a standalone wildcard matching everything
/// - Suffix glob: `git*` matches "git_status", "git_diff"
/// - Prefix glob: `*_file` matches "edit_file", "read_file"
/// - Contains glob: `*edit*` matches "edit_file", "multi_edit"
fn matches_tool(pattern: &str, tool_name: &str) -> bool {
    pattern.split('|').any(|p| {
        let p = p.trim();
        if p == "*" {
            true
        } else if p.contains('*') {
            // Split on '*' and check if all parts appear in order
            let parts: Vec<&str> = p.split('*').collect();
            let mut remaining = tool_name;

            for (i, part) in parts.iter().enumerate() {
                if part.is_empty() {
                    continue;
                }
                if i == 0 {
                    // First segment must be a prefix (pattern doesn't start with *)
                    if !remaining.starts_with(part) {
                        return false;
                    }
                    remaining = &remaining[part.len()..];
                } else if i == parts.len() - 1 {
                    // Last segment must be a suffix (pattern doesn't end with *)
                    if !remaining.ends_with(part) {
                        return false;
                    }
                } else {
                    // Middle segments must appear somewhere in the remaining string
                    match remaining.find(part) {
                        Some(pos) => {
                            remaining = &remaining[pos + part.len()..];
                        }
                        None => return false,
                    }
                }
            }
            true
        } else {
            p == tool_name
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matches_tool_exact() {
        assert!(matches_tool("bash", "bash"));
        assert!(!matches_tool("bash", "edit_file"));
    }

    #[test]
    fn test_matches_tool_alternation() {
        assert!(matches_tool("edit_file|write_file|multi_edit", "edit_file"));
        assert!(matches_tool("edit_file|write_file|multi_edit", "write_file"));
        assert!(matches_tool("edit_file|write_file|multi_edit", "multi_edit"));
        assert!(!matches_tool("edit_file|write_file|multi_edit", "bash"));
    }

    #[test]
    fn test_matches_tool_wildcard() {
        assert!(matches_tool("*", "anything"));
        assert!(matches_tool("git*", "git_status"));
        assert!(matches_tool("git*", "git_diff"));
        assert!(!matches_tool("git*", "bash"));
    }

    #[test]
    fn test_matches_tool_prefix_wildcard() {
        assert!(matches_tool("*_file", "edit_file"));
        assert!(matches_tool("*_file", "read_file"));
        assert!(matches_tool("*_file", "write_file"));
        assert!(!matches_tool("*_file", "bash"));
        assert!(!matches_tool("*_file", "file_reader"));
    }

    #[test]
    fn test_matches_tool_contains_wildcard() {
        assert!(matches_tool("*edit*", "edit_file"));
        assert!(matches_tool("*edit*", "multi_edit"));
        assert!(matches_tool("*edit*", "multi_edit_tool"));
        assert!(!matches_tool("*edit*", "bash"));
    }

    #[test]
    fn test_hooks_config_matching() {
        let config = HooksConfig {
            pre_tool_use: vec![
                HookEntry {
                    matcher: Some("bash".to_string()),
                    command: "echo test".to_string(),
                    timeout_secs: 30,
                },
                HookEntry {
                    matcher: None,
                    command: "echo all".to_string(),
                    timeout_secs: 30,
                },
            ],
            ..Default::default()
        };

        // "bash" matches both the specific hook and the catch-all
        let hooks = config.matching_hooks(HookEvent::PreToolUse, Some("bash"));
        assert_eq!(hooks.len(), 2);

        // "read_file" only matches the catch-all
        let hooks = config.matching_hooks(HookEvent::PreToolUse, Some("read_file"));
        assert_eq!(hooks.len(), 1);
    }
}
