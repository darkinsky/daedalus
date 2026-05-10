/// Build the thinking style section of the system prompt.
///
/// This section guides the LLM's reasoning approach — how to analyze problems,
/// plan actions, and structure its thought process before responding.
///
/// # Arguments
/// * `has_tools` - Whether the agent has MCP tools available.
pub fn build_thinking_section(has_tools: bool) -> String {
    let tool_thinking = if has_tools {
        "\n- Before calling a tool, verify: Is this the right tool? Are the arguments correct? \
         Could I answer without a tool call?\n\
         - After receiving tool results, evaluate quality before presenting to the user. \
         If results are insufficient, consider retrying with different parameters."
    } else {
        ""
    };

    format!(
        "<thinking_style>\n\
         - Think step-by-step about the user's request BEFORE taking action\n\
         - Break down the task: What is being asked? What information do I have? What is missing?\n\
         - If the request is ambiguous or missing critical details, ask for clarification \
         BEFORE proceeding — do not guess\n\
         \n\
         ## Adaptive Planning\n\
         \n\
         Gauge task complexity BEFORE acting and scale your planning accordingly:\n\
         \n\
         - **Simple tasks** (single-file edit, factual question, quick fix): Act immediately. \
         No visible plan needed.\n\
         - **Medium tasks** (multi-file changes, debugging across modules, feature implementation): \
         State your approach in 2-3 sentences at the start of your response, then execute.\n\
         - **Complex tasks** (architecture changes, large refactors, multi-step investigations \
         spanning 3+ modules): Produce a brief structured plan before executing:\n\
           1. Goal — one sentence summary\n\
           2. Steps — numbered list of concrete actions\n\
           3. Risks — anything that might go wrong\n\
           Then execute the plan step by step.\n\
         \n\
         Signs a task is complex: involves 5+ files, crosses module boundaries, requires \
         understanding multiple subsystems, or the user explicitly asks to \"plan\" or \"design\".\n\
         \n\
         - Never dump your entire reasoning into the response — keep thinking internal, \
         deliver results externally{tool_thinking}\n\
         - After thinking, you MUST provide a visible response. Thinking is for planning; \
         the response is for delivery.\n\
         </thinking_style>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_thinking_without_tools() {
        let section = build_thinking_section(false);
        assert!(section.contains("<thinking_style>"));
        assert!(section.contains("</thinking_style>"));
        assert!(section.contains("Adaptive Planning"));
        assert!(section.contains("Simple tasks"));
        assert!(section.contains("Medium tasks"));
        assert!(section.contains("Complex tasks"));
        assert!(!section.contains("tool"));
    }

    #[test]
    fn test_thinking_with_tools() {
        let section = build_thinking_section(true);
        assert!(section.contains("Before calling a tool"));
        assert!(section.contains("tool results"));
        assert!(section.contains("Adaptive Planning"));
    }
}
