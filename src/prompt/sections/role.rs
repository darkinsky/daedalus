use crate::tools::ToolInfo;

/// Default agent name used when no custom name is configured.
const DEFAULT_AGENT_NAME: &str = "Daedalus";

/// Build the role definition section of the system prompt.
///
/// This section establishes the agent's identity, capabilities, and behavioral
/// boundaries. It adapts based on whether tools are available.
///
/// # Arguments
/// * `agent_name` - Custom agent name, or `None` for the default.
/// * `tools` - Available tool descriptions (empty if no tools).
pub fn build_role_section(agent_name: Option<&str>, tools: &[ToolInfo]) -> String {
    let name = agent_name.unwrap_or(DEFAULT_AGENT_NAME);
    let tool_awareness = if tools.is_empty() {
        String::new()
    } else {
        format!(
            "\nYou have access to {count} external tool(s) via MCP (Model Context Protocol). \
             Use them when the user's request requires real-time data, external actions, \
             or capabilities beyond text generation.",
            count = tools.len()
        )
    };

    format!(
        "<role>\n\
         You are {name}, an intelligent AI assistant powered by large language models.\n\
         \n\
         Core capabilities:\n\
         - Answering questions with accuracy and depth\n\
         - Analyzing problems and providing structured solutions\n\
         - Writing, reviewing, and debugging code in multiple languages\n\
         - Explaining complex concepts clearly{tool_awareness}\n\
         \n\
         You are honest about your limitations. If you don't know something, say so \
         rather than guessing. If a task is beyond your capabilities, explain why and \
         suggest alternatives.\n\
         </role>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_role_no_tools() {
        let section = build_role_section(None, &[]);
        assert!(section.contains("Daedalus"));
        assert!(section.contains("<role>"));
        assert!(section.contains("</role>"));
        assert!(!section.contains("MCP"));
    }

    #[test]
    fn test_custom_name() {
        let section = build_role_section(Some("Atlas"), &[]);
        assert!(section.contains("Atlas"));
        assert!(!section.contains("Daedalus"));
    }

    #[test]
    fn test_with_tools() {
        let tools = vec![
            ToolInfo {
                name: "web_search".to_string(),
                description: "Search the web".to_string(),
                source: "search-server".to_string(),
                usage_hint: None,
            },
        ];
        let section = build_role_section(None, &tools);
        assert!(section.contains("1 external tool(s)"));
        assert!(section.contains("MCP"));
    }
}
