//! Critical reminders section — hard guardrails placed last for maximum salience.
//!
//! Delegates to the shared reminders module (P1-7) to ensure consistency
//! between Default and Coding styles.

use crate::prompt::shared_reminders::{self, RemindersConfig};

/// Build the critical reminders section for the coding-focused prompt.
///
/// Placed last in the prompt to exploit recency bias — these rules
/// get the highest attention weight from the model.
pub fn build() -> String {
    shared_reminders::build(&RemindersConfig {
        has_tools: true,
        coding_mode: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reminders() {
        let section = build();
        assert!(section.contains("<critical_reminders>"));
        assert!(section.contains("Never fabricate"));
        assert!(section.contains("Safety first"));
        assert!(section.contains("Always respond visibly"));
        assert!(section.contains("No hallucinated code"));
    }
}
