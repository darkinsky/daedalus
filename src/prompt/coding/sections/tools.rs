//! Tool definitions section — detailed per-tool guidance.
//!
//! Each tool gets specific when-to-use / when-NOT-to-use guidance,
//! plus parallel execution strategy.

use crate::tools::ToolInfo;

/// Build the tools section with per-tool strategies and parallel execution guidance.
///
/// Returns empty string if no tools are available.
pub fn build(tools: &[ToolInfo]) -> String {
    if tools.is_empty() {
        return String::new();
    }

    // Build tool inventory with descriptions
    let tool_list: Vec<String> = tools
        .iter()
        .map(|t| {
            format!(
                "- **{name}** [{source}]: {desc}",
                name = t.name,
                source = t.source,
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
        "<tools>\n\
         ## Available Tools\n\n\
         {inventory}\n\
         \n\
         ## Tool Usage Strategy\n\
         \n\
         ### Execution Principles\n\
         \n\
         1. **Maximize parallel execution**: When multiple independent tool calls are needed, \
         execute ALL of them simultaneously. Never make sequential calls when parallel is possible.\n\
            - Reading multiple files → parallel\n\
            - Searching different patterns → parallel\n\
            - Independent grep + codebase search → parallel\n\
            - Only serialize when output of tool A is required input for tool B\n\
         \n\
         2. **Gather before acting**: Before making code changes, first understand the full \
         context. Read relevant files, search for usages, check imports — all in parallel.\n\
         \n\
         3. **Right tool for the job**:\n\
            - Exact text/symbol lookup → grep or regex search tools\n\
            - Semantic/meaning-based search → semantic search tools (if available)\n\
            - Known file path → read the file directly\n\
            - File/directory discovery → search or list tools\n\
            - Multiple edits to one file → batch/multi-edit tools\n\
            - System commands → shell/bash execution\n\
         \n\
         4. **Validate after editing**: After making changes, check for errors. If a tool \
         call fails, analyze why and try a different approach — don't retry with identical args.\n\
         \n\
         5. **Minimize unnecessary calls**: If you can answer confidently from context already \
         gathered, do so without additional tool calls.\n\
         \n\
         ### Error Handling\n\
         \n\
         - If a tool fails 3 times, switch to an alternative approach\n\
         - Never expose raw tool errors to the user — explain in natural language\n\
         - Tool results are intermediate data, not final answers — always interpret and \
           contextualize before presenting to the user\n\
         \n\
         ### Important Constraints\n\
         \n\
         - Never mention tool names or MCP protocol details to the user\n\
         - Present tool results naturally as part of your response\n\
         - Do not fabricate tool arguments — if you don't have the required info, ask or search\n\
         </tools>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_tools_returns_empty() {
        assert!(build(&[]).is_empty());
    }

    #[test]
    fn test_single_tool() {
        let tools = vec![ToolInfo {
            name: "read_file".to_string(),
            description: "Read file contents".to_string(),
            source: "built-in".to_string(),
        }];
        let section = build(&tools);
        assert!(section.contains("<tools>"));
        assert!(section.contains("read_file"));
        assert!(section.contains("Maximize parallel execution"));
    }

    #[test]
    fn test_multiple_tools() {
        let tools = vec![
            ToolInfo {
                name: "read_file".to_string(),
                description: "Read a file".to_string(),
                source: "built-in".to_string(),
            },
            ToolInfo {
                name: "grep_search".to_string(),
                description: "Search with regex".to_string(),
                source: "built-in".to_string(),
            },
        ];
        let section = build(&tools);
        assert!(section.contains("read_file"));
        assert!(section.contains("grep_search"));
    }
}
