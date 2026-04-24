use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

/// A single entry in the dynamic cheatsheet.
///
/// Each entry captures a specific insight, strategy, error pattern,
/// code snippet, or worked example learned from past interactions.
/// Entries are accumulated over time and injected into the system prompt
/// to help the LLM avoid repeating mistakes and reuse proven strategies.
///
/// Content can be multi-line — including code blocks, worked solutions,
/// and detailed edge-case descriptions — matching the DC paper's design
/// of preserving actionable, reusable reference material.
///
/// Inspired by the Dynamic Cheatsheet paper (arxiv:2504.07952).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheatsheetEntry {
    /// Category of this entry (free-form, e.g., "strategy", "code_snippet",
    /// "edge_case", "meta_reasoning"). The LLM chooses categories naturally;
    /// they are not restricted to a fixed set.
    pub category: String,
    /// The insight content — can be multi-line, including code blocks,
    /// worked examples, and detailed explanations.
    pub content: String,
    /// How many times this entry has been reinforced (used/validated/updated).
    /// Tracks the paper's "Usage Counter" concept for prioritization.
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
    ///
    /// Called when the Curator confirms this entry was useful or when
    /// an existing entry is updated/refined.
    pub fn reinforce(&mut self) {
        self.reinforcement_count += 1;
        self.updated_at = Local::now();
    }

    /// Update the content of this entry, refresh the timestamp,
    /// and increment the reinforcement count (since an update implies
    /// the entry is actively useful).
    pub fn update_content(&mut self, new_content: String) {
        self.content = new_content;
        self.reinforcement_count += 1;
        self.updated_at = Local::now();
    }

    /// Format for display in the cheatsheet Markdown, including usage count.
    ///
    /// Matches the paper's format: each entry shows its usage/reinforcement
    /// count so the LLM can prioritize frequently-validated strategies.
    pub fn to_markdown_item(&self) -> String {
        if self.reinforcement_count > 1 {
            format!("- {} *(used {}×)*", self.content, self.reinforcement_count)
        } else {
            format!("- {}", self.content)
        }
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
    fn test_update_content_increments_reinforcement() {
        let mut entry = CheatsheetEntry::new(
            "strategy".to_string(),
            "Old content".to_string(),
        );
        assert_eq!(entry.reinforcement_count, 1);
        entry.update_content("New refined content".to_string());
        assert_eq!(entry.content, "New refined content");
        assert_eq!(entry.reinforcement_count, 2);
    }

    #[test]
    fn test_to_markdown_item_single_use() {
        let entry = CheatsheetEntry::new(
            "strategy".to_string(),
            "Use memoization".to_string(),
        );
        assert_eq!(entry.to_markdown_item(), "- Use memoization");
    }

    #[test]
    fn test_to_markdown_item_multi_use() {
        let mut entry = CheatsheetEntry::new(
            "strategy".to_string(),
            "Use memoization".to_string(),
        );
        entry.reinforce();
        entry.reinforce();
        assert_eq!(entry.to_markdown_item(), "- Use memoization *(used 3×)*");
    }
}
