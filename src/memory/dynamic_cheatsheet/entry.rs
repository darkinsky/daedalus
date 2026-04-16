use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

/// A single entry in the dynamic cheatsheet.
///
/// Each entry captures a specific insight, strategy, error pattern,
/// or code snippet learned from past interactions. Entries are
/// accumulated over time and injected into the system prompt to
/// help the LLM avoid repeating mistakes and reuse proven strategies.
///
/// Inspired by the Dynamic Cheatsheet paper (arxiv:2504.07952).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheatsheetEntry {
    /// Category of this entry (e.g., "strategy", "error_pattern", "code_snippet").
    pub category: String,
    /// The insight content — a concise, actionable description.
    pub content: String,
    /// How many times this entry has been reinforced (used/validated).
    pub reinforcement_count: u32,
    /// When this entry was first created.
    pub created_at: DateTime<Local>,
    /// When this entry was last updated or reinforced.
    pub updated_at: DateTime<Local>,
}

impl CheatsheetEntry {
    /// Create a new cheatsheet entry.
    pub fn new(category: String, content: String) -> Self {
        let now = Local::now();
        Self {
            category,
            content,
            reinforcement_count: 1,
            created_at: now,
            updated_at: now,
        }
    }

    /// Reinforce this entry (increment count and update timestamp).
    pub fn reinforce(&mut self) {
        self.reinforcement_count += 1;
        self.updated_at = Local::now();
    }

    /// Update the content of this entry and refresh the timestamp.
    pub fn update_content(&mut self, new_content: String) {
        self.content = new_content;
        self.updated_at = Local::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_entry() {
        let entry = CheatsheetEntry::new(
            "strategy".to_string(),
            "Use binary search for sorted arrays".to_string(),
        );
        assert_eq!(entry.category, "strategy");
        assert_eq!(entry.content, "Use binary search for sorted arrays");
        assert_eq!(entry.reinforcement_count, 1);
    }

    #[test]
    fn test_reinforce() {
        let mut entry = CheatsheetEntry::new(
            "error_pattern".to_string(),
            "Off-by-one in loop bounds".to_string(),
        );
        let original_count = entry.reinforcement_count;
        entry.reinforce();
        assert_eq!(entry.reinforcement_count, original_count + 1);
        assert!(entry.updated_at >= entry.created_at);
    }

    #[test]
    fn test_update_content() {
        let mut entry = CheatsheetEntry::new(
            "strategy".to_string(),
            "Old content".to_string(),
        );
        entry.update_content("New refined content".to_string());
        assert_eq!(entry.content, "New refined content");
    }
}
