
//! Subagent delegation section — guidance for spawning and managing subagents.
//!
//! Only included when the `spawn_subagent` tool is available.
//! This saves ~40 lines / ~300 tokens when subagents are not configured.

use crate::tools::ToolInfo;

/// Check if the spawn_subagent tool is available.
fn has_subagent_tool(tools: &[ToolInfo]) -> bool {
    tools.iter().any(|t| t.name == "spawn_subagent")
}

/// Build the subagent delegation section.
///
/// Returns `None` if the `spawn_subagent` tool is not available.
pub fn build(tools: &[ToolInfo]) -> Option<String> {
    if !has_subagent_tool(tools) {
        return None;
    }

    Some(
        "<delegation>\n\
         ## Subagent Delegation\n\
         \n\
         ### Basic Principles\n\
         - Spawn the most specialized subagent directly. Do not chain multiple subagents \
         sequentially — they have isolated contexts and cannot share data.\n\
         - If a subagent returns partial results, use them as-is. Never redo its work yourself.\n\
         - On failure, retry with a narrower scope instead of a full retry.\n\
         - **Delegate early**: If a task clearly needs subagents (e.g., full-project review, \
         multi-module exploration), delegate in round 1-2. Do NOT spend rounds exploring \
         the codebase yourself only to pass the same information to a subagent.\n\
         \n\
         ### Duplicate Detection\n\
         \n\
         Before dispatching subagents, check if the conversation history already contains \
         a completed result for the same or similar scope (e.g., a previous review report). \
         If found, summarize what exists and ask the user whether to update, change scope, \
         or redo from scratch. Do NOT silently re-execute expensive parallel work.\n\
         \n\
         ### Complex Task Decomposition (Plan → Parallel Execution)\n\
         \n\
         For large tasks that would overwhelm a single subagent's context window:\n\
         \n\
         1. **Assess scope quickly** (1 round max): Use `list_directory` or `bash find` \
         to estimate file count. If >30 files or >5,000 lines → decompose.\n\
         \n\
         2. **Plan first**: Spawn `plan` to analyze the project structure and propose \
         independent sub-tasks scoped by module/directory.\n\
         \n\
         3. **Execute in parallel**: Spawn multiple `spawn_subagent` calls simultaneously \
         (one per sub-task). The same agent type can handle multiple sub-tasks \
         (e.g., multiple `code-reviewer` instances reviewing different modules).\n\
         \n\
         4. **Synthesize**: After all sub-tasks complete, merge their results into a \
         coherent final response. Deduplicate, resolve conflicts, and add cross-cutting \
         observations.\n\
         \n\
         ### Partition Guidelines\n\
         \n\
         - **Self-contained tasks**: Each sub-task description must include the specific \
         directories/files and any relevant context — subagents cannot see each other's results.\n\
         - **Balanced scope**: Keep sub-tasks roughly equal (~20-35 files each). Give large \
         modules (20+ files) their own subagent.\n\
         - **Cross-module hints**: When modules have known interactions (e.g., shared types, \
         common utilities), mention the dependency in BOTH partitions' task descriptions \
         so each subagent can check the interface boundary. If possible, extract key \
         cross-module interfaces (via import/dependency grep) during the planning phase \
         and inject them into each subagent's task as `<cross_module_context>`.\n\
         </delegation>"
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_subagent_tool_returns_none() {
        let tools = vec![crate::tools::ToolInfo {
            name: "read_file".to_string(),
            description: "Read a file".to_string(),
            source: "built-in".to_string(),
                usage_hint: None,
        }];
        assert!(build(&tools).is_none());
    }

    #[test]
    fn test_empty_tools_returns_none() {
        assert!(build(&[]).is_none());
    }

    #[test]
    fn test_with_subagent_tool() {
        let tools = vec![crate::tools::ToolInfo {
            name: "spawn_subagent".to_string(),
            description: "Spawn a subagent".to_string(),
            source: "built-in".to_string(),
                usage_hint: None,
        }];
        let section = build(&tools).unwrap();
        assert!(section.contains("<delegation>"));
        assert!(section.contains("Delegate early"));
        assert!(section.contains("Duplicate Detection"));
        assert!(section.contains("Partition Guidelines"));
        assert!(section.contains("Cross-module hints"));
        assert!(section.contains("cross_module_context"));
    }
}
