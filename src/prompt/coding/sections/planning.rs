//! Planning guidance — instructs the LLM on when and how to use create_plan/update_plan.
//!
//! Inspired by Claude Code's TodoWrite system: forces the model to externalize
//! its plan into a trackable state, preventing goal drift in long tasks.
//! Only included when the `create_plan` tool is available.

use crate::tools::ToolInfo;

/// Build the planning guidance section.
///
/// Returns `None` if `create_plan` is not in the available tools.
pub fn build(tools: &[ToolInfo]) -> Option<String> {
    let has_plan_tool = tools.iter().any(|t| t.name == "create_plan");
    if !has_plan_tool {
        return None;
    }

    Some(
        "<planning>\n\
         **Task Planning with `create_plan` / `update_plan`**:\n\
         \n\
         When working on a task with 3 or more distinct steps, you MUST use the \
         `create_plan` tool to create a structured plan BEFORE starting execution. \
         This externalizes your plan into a tracked state that persists across rounds \
         and prevents goal drift during long-running tasks.\n\
         \n\
         **When to create a plan:**\n\
         - Multi-file changes (feature implementation, refactoring)\n\
         - Code reviews spanning multiple modules\n\
         - Architecture analysis or design tasks\n\
         - Any task where you anticipate 3+ distinct steps\n\
         \n\
         **Rules:**\n\
         - Create a plan at the START of complex tasks, before any tool calls\n\
         - Each step should be concrete and actionable (not vague like \"do stuff\")\n\
         - Call `update_plan` immediately after completing or starting each step\n\
         - Only ONE step should be `in_progress` at a time — finish or fail it before \
         moving to the next\n\
         - If your approach changes mid-task, create a NEW plan (the old one is archived)\n\
         \n\
         **Do NOT:**\n\
         - Plan only in your reasoning/text — the plan MUST be externalized via the tool\n\
         - Batch-update multiple steps at once — update after EACH step\n\
         - Ignore the plan once created — it is your contract with yourself\n\
         - Skip creating a plan for complex tasks just because you \"know what to do\"\n\
         </planning>"
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_planning_with_tool() {
        let tools = vec![ToolInfo {
            name: "create_plan".to_string(),
            description: "Create a plan".to_string(),
            source: "built-in".to_string(),
            usage_hint: None,
        }];
        let section = build(&tools);
        assert!(section.is_some());
        let text = section.unwrap();
        assert!(text.contains("<planning>"));
        assert!(text.contains("</planning>"));
        assert!(text.contains("MUST use"));
        assert!(text.contains("update_plan"));
    }

    #[test]
    fn test_planning_without_tool() {
        let tools = vec![ToolInfo {
            name: "read_file".to_string(),
            description: "Read a file".to_string(),
            source: "built-in".to_string(),
            usage_hint: None,
        }];
        let section = build(&tools);
        assert!(section.is_none());
    }
}
