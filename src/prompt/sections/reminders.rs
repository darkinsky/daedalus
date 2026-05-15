//! Critical reminders section for the Default prompt style.
//!
//! Delegates to the shared reminders module (P1-7) to ensure consistency
//! between Default and Coding styles.

use crate::prompt::shared_reminders::{self, RemindersConfig};

/// Build the critical reminders section of the system prompt.
///
/// This section contains high-priority behavioral rules that the LLM
/// should always follow. It acts as a "guardrail" to prevent common
/// failure modes.
///
/// # Arguments
/// * `has_tools` - Whether the agent has MCP tools available.
pub fn build_reminders_section(has_tools: bool) -> String {
    shared_reminders::build(&RemindersConfig {
        has_tools,
        coding_mode: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reminders_without_tools() {
        let section = build_reminders_section(false);
        assert!(section.contains("<critical_reminders>"));
        assert!(section.contains("Never fabricate"));
        assert!(!section.contains("Tool results"));
    }

    #[test]
    fn test_reminders_with_tools() {
        let section = build_reminders_section(true);
        assert!(section.contains("Tool results are not final answers"));
        assert!(section.contains("Never expose raw tool errors"));
    }
}
