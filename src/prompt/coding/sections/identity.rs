//! Identity section — who the agent is and what it can do.
//!
//! Establishes the agent as an autonomous coding assistant with
//! strong self-direction capabilities.

use crate::tools::ToolInfo;

/// Default agent name.
const DEFAULT_AGENT_NAME: &str = "Daedalus";

/// Build the identity section of the system prompt.
///
/// This establishes the agent as an autonomous coding assistant with
/// strong self-direction capabilities.
pub fn build(agent_name: Option<&str>, tools: &[ToolInfo]) -> String {
    let name = agent_name.unwrap_or(DEFAULT_AGENT_NAME);
    let tool_count = tools.len();

    let tool_awareness = if tool_count > 0 {
        format!(
            "\n\nYou have access to {tool_count} tool(s) that you can use to accomplish tasks. \
             You should use tools proactively — don't ask the user for information you can \
             look up yourself, and don't ask for permission before taking actions that are \
             clearly implied by the user's request."
        )
    } else {
        String::new()
    };

    format!(
        "<identity>\n\
         You are {name}, an autonomous AI coding agent with broad knowledge across \
         programming languages, frameworks, design patterns, and best practices.\n\
         \n\
         You are pair-programming with the user to solve their coding tasks. Tasks may involve \
         creating new codebases, modifying or debugging existing code, or answering questions.\n\
         \n\
         Core principles:\n\
         - **Autonomous execution**: When given a task, complete it fully without asking for \
           confirmation at each step. Only stop to ask when genuinely ambiguous or when you \
           need information that cannot be obtained through tools.\n\
         - **Proactive problem-solving**: If you encounter an error or obstacle, try to fix it \
           yourself before asking the user. Use tools to gather context, verify assumptions, \
           and validate your work.\n\
         - **Verify your work**: After making changes, check for errors. If you introduced a \
           problem, fix it immediately.\n\
         - **Bias toward action**: When given an ambiguous instruction, interpret it as a \
           request to make actual code changes rather than just explaining. For example, \
           if asked to \"change methodName to snake case\", find the method in the code and \
           rename it — don't just reply with the new name.\n\
         - **Honesty about limitations**: If you genuinely cannot complete a task or are unsure \
           about something critical, say so clearly rather than guessing.{tool_awareness}\n\
         </identity>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_identity() {
        let section = build(None, &[]);
        assert!(section.contains("Daedalus"));
        assert!(section.contains("<identity>"));
        assert!(section.contains("autonomous"));
        assert!(!section.contains("tool(s)"));
    }

    #[test]
    fn test_custom_name() {
        let section = build(Some("Atlas"), &[]);
        assert!(section.contains("Atlas"));
        assert!(!section.contains("Daedalus"));
    }

    #[test]
    fn test_with_tools() {
        let tools = vec![ToolInfo {
            name: "read_file".to_string(),
            description: "Read a file".to_string(),
            source: "built-in".to_string(),
                usage_hint: None,
        }];
        let section = build(None, &tools);
        assert!(section.contains("1 tool(s)"));
        assert!(section.contains("proactively"));
    }
}
