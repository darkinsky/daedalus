use crate::llm::ToolInfo;

/// Build the tool guidance section of the system prompt.
///
/// This section teaches the LLM how to use available MCP tools effectively.
/// It includes the tool inventory, usage guidelines, and error handling advice.
///
/// Returns an empty string if no tools are available.
///
/// # Arguments
/// * `tools` - Available tool descriptions.
pub fn build_tool_guidance_section(tools: &[ToolInfo]) -> String {
    if tools.is_empty() {
        return String::new();
    }

    // Build tool inventory
    let tool_list: Vec<String> = tools
        .iter()
        .map(|t| {
            format!(
                "  - **{name}** (server: {server}): {desc}",
                name = t.name,
                server = t.server,
                desc = if t.description.is_empty() {
                    "No description available"
                } else {
                    &t.description
                }
            )
        })
        .collect();

    let inventory = tool_list.join("\n");

    format!(
        "<tool_system>\n\
         You have access to the following tools via MCP:\n\
         \n\
         {inventory}\n\
         \n\
         **Tool Usage Guidelines:**\n\
         1. **Right tool for the job**: Choose the most appropriate tool for each task. \
         Read the tool description carefully before calling it.\n\
         2. **Validate arguments**: Ensure all required parameters are provided and correctly \
         formatted before making a tool call. Do not fabricate parameter values.\n\
         3. **Interpret results**: After receiving tool results, analyze them critically. \
         Summarize key findings for the user rather than dumping raw output.\n\
         4. **Handle errors gracefully**: If a tool call fails, explain the error to the user \
         and suggest alternatives. Do not retry the same call with identical arguments.\n\
         5. **Minimize unnecessary calls**: If you can answer confidently from your training \
         knowledge, do so without calling a tool. Use tools for real-time data, external \
         actions, or tasks that require specific capabilities.\n\
         6. **Parallel execution**: When multiple independent tool calls are needed, describe \
         all of them together rather than sequentially.\n\
         \n\
         **IMPORTANT**: Never mention internal tool names or MCP protocol details to the user. \
         Present tool results naturally as part of your response.\n\
         </tool_system>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_tools_returns_empty() {
        let section = build_tool_guidance_section(&[]);
        assert!(section.is_empty());
    }

    #[test]
    fn test_single_tool() {
        let tools = vec![ToolInfo {
            name: "web_search".to_string(),
            description: "Search the web for information".to_string(),
            server: "brave-search".to_string(),
        }];
        let section = build_tool_guidance_section(&tools);
        assert!(section.contains("<tool_system>"));
        assert!(section.contains("web_search"));
        assert!(section.contains("brave-search"));
        assert!(section.contains("Tool Usage Guidelines"));
    }

    #[test]
    fn test_multiple_tools() {
        let tools = vec![
            ToolInfo {
                name: "search".to_string(),
                description: "Search".to_string(),
                server: "server-a".to_string(),
            },
            ToolInfo {
                name: "read_file".to_string(),
                description: "Read a file".to_string(),
                server: "server-b".to_string(),
            },
        ];
        let section = build_tool_guidance_section(&tools);
        assert!(section.contains("search"));
        assert!(section.contains("read_file"));
    }

    #[test]
    fn test_tool_without_description() {
        let tools = vec![ToolInfo {
            name: "mystery_tool".to_string(),
            description: String::new(),
            server: "server".to_string(),
        }];
        let section = build_tool_guidance_section(&tools);
        assert!(section.contains("No description available"));
    }
}
