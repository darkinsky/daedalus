/// Build the critical reminders section of the system prompt.
///
/// This section contains high-priority behavioral rules that the LLM
/// should always follow. It acts as a "guardrail" to prevent common
/// failure modes.
///
/// # Arguments
/// * `has_tools` - Whether the agent has MCP tools available.
pub fn build_reminders_section(has_tools: bool) -> String {
    let tool_reminders = if has_tools {
        "\n- **Tool results are not final answers**: Always interpret and contextualize \
         tool output before presenting it to the user.\n\
         - **Never expose raw tool errors**: If a tool fails, explain the situation in \
         user-friendly language."
    } else {
        ""
    };

    format!(
        "<critical_reminders>\n\
         - **Honesty over confidence**: If you are unsure, say so. Never fabricate facts, \
         URLs, citations, or data.\n\
         - **Safety first**: Refuse requests that could cause harm, violate privacy, or \
         break laws. Explain why you cannot help.\n\
         - **Stay on topic**: Address the user's actual question. Don't add unrequested \
         information or unsolicited advice.\n\
         - **Acknowledge limitations**: You have a knowledge cutoff date. For recent events \
         or real-time data, use available tools or tell the user your information may be outdated.\n\
         - **No hallucinated references**: Never invent book titles, paper names, URLs, or \
         API endpoints. Only cite sources you are confident exist.{tool_reminders}\n\
         - **Always respond**: Your thinking is internal. You MUST always provide a visible \
         response to the user.\n\
         </critical_reminders>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reminders_without_tools() {
        let section = build_reminders_section(false);
        assert!(section.contains("<critical_reminders>"));
        assert!(section.contains("Honesty over confidence"));
        assert!(!section.contains("Tool results"));
    }

    #[test]
    fn test_reminders_with_tools() {
        let section = build_reminders_section(true);
        assert!(section.contains("Tool results are not final answers"));
        assert!(section.contains("Never expose raw tool errors"));
    }
}
