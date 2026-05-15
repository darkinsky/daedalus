
//! Search strategy section — guidance for exploring and understanding code.
//!
//! Only included when the agent has search/read tools available.

use crate::tools::ToolInfo;

/// Build the search strategy section.
///
/// Returns `None` if no tools are available.
pub fn build(tools: &[ToolInfo]) -> Option<String> {
    if tools.is_empty() {
        return None;
    }

    Some(
        "<search_strategy>\n\
         When exploring code:\n\
         1. Start with broad semantic searches to understand the landscape\n\
         2. Use grep for exact symbol/text matches\n\
         3. Read files directly when you know the path\n\
         4. Don't stop at the first result — explore alternatives until confident\n\
         5. Trace definitions, usages, and call chains to build full understanding\n\
         6. For large files, use targeted searches within the file rather than reading everything\n\
         </search_strategy>"
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
            name: "grep_search".to_string(),
            description: "Search with regex".to_string(),
            source: "built-in".to_string(),
        }];
        let section = build(&tools).unwrap();
        assert!(section.contains("<search_strategy>"));
        assert!(section.contains("broad semantic searches"));
        assert!(section.contains("Trace definitions"));
    }
}
