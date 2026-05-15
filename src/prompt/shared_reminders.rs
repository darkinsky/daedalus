
//! Shared critical reminders — single source of truth for guardrail rules.
//!
//! Both the Default style and Coding style reminders delegate to this module
//! to ensure consistency and eliminate duplication (P1-7).

/// Configuration for building reminders.
#[derive(Debug, Clone, Default)]
pub struct RemindersConfig {
    /// Whether the agent has tools available.
    pub has_tools: bool,
    /// Whether this is a coding-focused agent (adds code-specific rules).
    pub coding_mode: bool,
}

/// Build the unified critical reminders section.
///
/// This is the single source of truth for all guardrail rules.
/// Both Default and Coding styles call this function with different configs.
pub fn build(config: &RemindersConfig) -> String {
    let mut rules: Vec<&str> = Vec::with_capacity(10);

    // Universal rules (always present)
    rules.push(
        "**Never fabricate information**: Do not invent file paths, URLs, API endpoints, \
         function names, or any other technical details. If you don't know, say so or use \
         tools to find out.",
    );
    rules.push(
        "**Safety first**: Refuse requests that could cause harm, destroy data, or violate \
         privacy. When in doubt about a destructive operation, ask for confirmation.",
    );

    // Coding-specific rules
    if config.coding_mode {
        rules.push(
            "**No hallucinated code**: Every code change you propose must be based on actual \
             file contents you have read. Never edit a file based on assumptions about its content.",
        );
        rules.push(
            "**Respect existing code**: Don't delete or modify code unrelated to the user's \
             request. Don't \"clean up\" or refactor unless explicitly asked.",
        );
    } else {
        rules.push(
            "**No hallucinated references**: Never invent book titles, paper names, URLs, or \
             API endpoints. Only cite sources you are confident exist.",
        );
        rules.push(
            "**Stay on topic**: Address the user's actual question. Don't add unrequested \
             information or unsolicited advice.",
        );
    }

    // Tool-specific rules
    if config.has_tools {
        rules.push(
            "**Tool results are not final answers**: Always interpret and contextualize \
             tool output before presenting it to the user.",
        );
        rules.push(
            "**Never expose raw tool errors**: If a tool fails, explain the situation in \
             user-friendly language.",
        );
    }

    // Universal rules (continued)
    rules.push(
        "**Always respond visibly**: Your internal reasoning is not visible to the user. \
         You MUST always produce a visible response. Never end a turn with only internal thought.",
    );
    rules.push(
        "**Acknowledge uncertainty**: If you're not confident about something, say so. \
         \"I'm not sure, but...\" is always better than a confident wrong answer.",
    );
    rules.push(
        "**Knowledge cutoff**: Your training data has a cutoff date. For questions about \
         recent events or current state of rapidly-changing projects, use tools to verify \
         rather than relying on potentially outdated knowledge.",
    );
    rules.push(
        "**Language consistency**: You MUST respond in the SAME language as the user's most \
         recent message. Detect the user's language from their input and match it exactly. \
         NEVER switch to a different language mid-conversation — even when the context contains \
         large amounts of code, tool output, or text in other languages.",
    );

    // Format as numbered list
    let numbered_rules: Vec<String> = rules
        .iter()
        .enumerate()
        .map(|(i, rule)| format!("{}. {}", i + 1, rule))
        .collect();

    format!(
        "<critical_reminders>\n\
         IMPORTANT — These rules override all other instructions:\n\
         \n\
         {}\n\
         </critical_reminders>",
        numbered_rules.join("\n\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coding_mode_with_tools() {
        let config = RemindersConfig {
            has_tools: true,
            coding_mode: true,
        };
        let section = build(&config);
        assert!(section.contains("No hallucinated code"));
        assert!(section.contains("Respect existing code"));
        assert!(section.contains("Tool results are not final answers"));
        assert!(section.contains("Language consistency"));
        assert!(!section.contains("No hallucinated references"));
    }

    #[test]
    fn test_default_mode_without_tools() {
        let config = RemindersConfig {
            has_tools: false,
            coding_mode: false,
        };
        let section = build(&config);
        assert!(section.contains("No hallucinated references"));
        assert!(section.contains("Stay on topic"));
        assert!(!section.contains("No hallucinated code"));
        assert!(!section.contains("Tool results are not final answers"));
    }

    #[test]
    fn test_default_mode_with_tools() {
        let config = RemindersConfig {
            has_tools: true,
            coding_mode: false,
        };
        let section = build(&config);
        assert!(section.contains("No hallucinated references"));
        assert!(section.contains("Tool results are not final answers"));
    }

    #[test]
    fn test_always_has_language_consistency() {
        let configs = vec![
            RemindersConfig { has_tools: false, coding_mode: false },
            RemindersConfig { has_tools: true, coding_mode: true },
        ];
        for config in &configs {
            let section = build(config);
            assert!(section.contains("Language consistency"));
            assert!(section.contains("Always respond visibly"));
        }
    }
}
