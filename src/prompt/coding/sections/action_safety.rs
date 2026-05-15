//! Action safety section — guidance on reversibility and blast radius.
//!
//! Only included when the agent has tools available (can execute actions).
//! Provides a comprehensive risk framework covering file operations,
//! shell commands, and git operations.

use crate::tools::ToolInfo;

/// Build the action safety section.
///
/// Returns `None` if no tools are available.
pub fn build(tools: &[ToolInfo]) -> Option<String> {
    if tools.is_empty() {
        return None;
    }

    Some(
        "<action_safety>\n\
         ## Executing Actions with Care\n\
         \n\
         Consider the **reversibility** and **blast radius** of every action:\n\
         \n\
         ### Low risk (proceed without hesitation):\n\
         - Reading files, searching code, listing directories\n\
         - Running read-only commands (git status, git log, ls)\n\
         - Creating new files that don't overwrite existing ones\n\
         \n\
         ### Medium risk (proceed but verify):\n\
         - Editing existing files (use surgical edits, not full rewrites)\n\
         - Running build/test commands\n\
         - Creating git commits\n\
         - Installing project-local dependencies\n\
         \n\
         ### High risk (explain what will happen, prefer safer alternatives):\n\
         - Deleting files or directories\n\
         - Running destructive shell commands (rm -rf, DROP TABLE, git reset --hard)\n\
         - Force-pushing to shared branches\n\
         - Overwriting files with full content (prefer edit over write)\n\
         - Running commands that modify global state (npm install -g, pip install)\n\
         - Modifying git history on shared branches\n\
         \n\
         When in doubt, prefer the reversible option. If a destructive action is truly needed, \
         explain what it will do and why before executing.\n\
         </action_safety>"
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_tools_returns_none() {
        assert!(build(&[]).is_none());
    }

    #[test]
    fn test_with_tools() {
        let tools = vec![crate::tools::ToolInfo {
            name: "bash".to_string(),
            description: "Run shell commands".to_string(),
            source: "built-in".to_string(),
                usage_hint: None,
        }];
        let section = build(&tools).unwrap();
        assert!(section.contains("<action_safety>"));
        assert!(section.contains("reversibility"));
        assert!(section.contains("blast radius"));
        assert!(section.contains("Low risk"));
        assert!(section.contains("Medium risk"));
        assert!(section.contains("High risk"));
        assert!(section.contains("Force-pushing"));
    }
}
