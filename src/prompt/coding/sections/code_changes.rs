
//! Code changes section — guidance for making edits safely and correctly.
//!
//! Only included when the agent has editing tools available.
//! Includes file operation safety boundaries (P1-6).

use crate::tools::ToolInfo;

/// Build the code changes section.
///
/// Returns `None` if no tools are available (no editing capability).
pub fn build(tools: &[ToolInfo]) -> Option<String> {
    if tools.is_empty() {
        return None;
    }

    Some(
        "<code_changes>\n\
         ## Making Code Changes\n\
         \n\
         1. **Read before writing**: Always examine the current file content before attempting \
         any edit. Never guess at file contents based on memory alone.\n\
         2. **Minimal changes**: Make the smallest edit that accomplishes the goal. Don't \
         refactor surrounding code unless explicitly asked.\n\
         3. **Preserve style**: Match the existing code style (indentation, naming conventions, \
         patterns) of the file you're editing.\n\
         4. **Complete implementations**: Generated code must be immediately runnable. Include \
         all necessary imports, handle edge cases, and don't leave TODO placeholders unless \
         the user explicitly asks for a skeleton.\n\
         5. **Verify after editing**: After making changes, check for lint errors or obvious \
         issues. Fix problems you introduced — don't leave broken code.\n\
         6. **One concern at a time**: If a task involves multiple files, handle them \
         systematically. Don't jump between unrelated changes.\n\
         \n\
         ## File Operation Safety\n\
         \n\
         - **Auto-execute** (no confirmation needed): reading files, searching, listing directories\n\
         - **Execute with caution**: creating new files, editing existing files\n\
         - **Require extra care**: deleting files, overwriting entire files, running destructive \
         shell commands (rm -rf, DROP TABLE, etc.)\n\
         - Prefer surgical edits over full file overwrites — smaller changes are safer and \
         easier to review\n\
         - Before deleting or overwriting, verify the file path is correct\n\
         </code_changes>"
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
            name: "edit_file".to_string(),
            description: "Edit a file".to_string(),
            source: "built-in".to_string(),
        }];
        let section = build(&tools).unwrap();
        assert!(section.contains("<code_changes>"));
        assert!(section.contains("Read before writing"));
        assert!(section.contains("File Operation Safety"));
        assert!(section.contains("Require extra care"));
    }
}
