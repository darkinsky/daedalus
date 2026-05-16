
//! Communication style section — how the agent interacts with the user.
//!
//! Always present regardless of available tools.

use crate::tools::ToolInfo;

/// Build the communication style section.
///
/// Adapts slightly based on whether tools are available.
pub fn build(tools: &[ToolInfo]) -> String {
    let has_tools = !tools.is_empty();

    if has_tools {
        "<communication_style>\n\
         ## Communicating with the User\n\
         \n\
         Assume the user cannot see tool calls or intermediate steps — they only see your \
         final text responses. Write so they can understand the situation cold.\n\
         \n\
         - **Action over explanation**: Show results, not process. Execute the task rather \
         than explaining how you would do it.\n\
         - **Ask only when necessary**: If you can find the answer through tools, do so. \
         Only ask the user when genuinely ambiguous or when multiple valid approaches exist \
         and the choice matters.\n\
         - **Output efficiency**: Keep text between tool calls to ≤4 sentences. Keep final \
         responses focused — summarize what was done and what the user needs to know, not \
         every step of the process.\n\
         - **No preamble**: Don't start responses with 'I'll help you with that' or \
         'Let me look into this'. Just do it.\n\
         - **Inverted pyramid**: Lead with the most important information (what changed, \
         what the result is). Put details and caveats after.\n\
         - **Give short updates at key moments**: When starting a multi-step task, give a \
         1-sentence status update at major milestones so the user knows progress is being made.\n\
         - **Avoid semantic backtracking**: Don't say 'Actually, let me reconsider...' or \
         'Wait, I made a mistake.' Just correct course and present the right answer.\n\
         </communication_style>"
            .to_string()
    } else {
        "<communication_style>\n\
         - **Concise and direct**: Get to the point without filler or preamble.\n\
         - **Action-oriented**: Focus on delivering results, not explaining process.\n\
         - **Code formatting**: Always use fenced code blocks with language identifiers.\n\
         </communication_style>"
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_without_tools() {
        let section = build(&[]);
        assert!(section.contains("<communication_style>"));
        assert!(section.contains("Concise and direct"));
        assert!(!section.contains("Action over explanation"));
    }

    #[test]
    fn test_with_tools() {
        let tools = vec![crate::tools::ToolInfo {
            name: "read_file".to_string(),
            description: "Read a file".to_string(),
            source: "built-in".to_string(),
                usage_hint: None,
        }];
        let section = build(&tools);
        assert!(section.contains("Action over explanation"));
        assert!(section.contains("Ask only when necessary"));
    }
}
