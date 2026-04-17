//! Agent diary for MemPalace.
//!
//! Personal journal entries for the AI agent, separated from
//! the core Palace structure for single-responsibility.

use super::palace::{DiaryEntry, Palace};

impl Palace {
    /// Write a diary entry for an agent.
    pub fn diary_write(&mut self, agent_name: &str, entry: &str, topic: &str) -> DiaryEntry {
        let diary_entry = DiaryEntry::new(
            agent_name.to_string(),
            entry.to_string(),
            topic.to_string(),
        );
        self.diary.push(diary_entry.clone());
        diary_entry
    }

    /// Read recent diary entries for an agent.
    #[allow(dead_code)]
    pub fn diary_read(&self, agent_name: &str, last_n: usize) -> Vec<&DiaryEntry> {
        let mut entries: Vec<&DiaryEntry> = self
            .diary
            .iter()
            .filter(|d| d.agent_name.eq_ignore_ascii_case(agent_name))
            .collect();
        // Sort by timestamp descending
        entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        entries.truncate(last_n);
        entries
    }
}
