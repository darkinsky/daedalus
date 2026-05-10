//! Consolidation algorithm — extracts long-term memory and history from messages.
//!
//! This module contains the pure logic for parsing consolidation LLM responses.
//! The actual LLM call orchestration remains in `memory.rs` as a thin wrapper,
//! since it needs access to multiple `SlidingWindowMemory` fields.

use super::consolidation::ConsolidationResult;
use super::history::HistoryEntry;
use super::long_term::LongTermMemory;

/// Parse the LLM's consolidation response into a `ConsolidationResult`.
///
/// Expected format:
/// ```text
/// SUMMARY: <summary text>
/// KEYWORDS: <kw1, kw2, kw3>
///
/// MEMORY:
/// ### Section Name
/// - fact 1
/// - fact 2
/// ```
pub(super) fn parse_consolidation_response(response: &str) -> Option<ConsolidationResult> {
    let response = response.trim();

    // Extract SUMMARY
    let summary = response
        .lines()
        .find(|line| line.starts_with("SUMMARY:"))
        .map(|line| line.trim_start_matches("SUMMARY:").trim().to_string())?;

    // Extract KEYWORDS
    let keywords: Vec<String> = response
        .lines()
        .find(|line| line.starts_with("KEYWORDS:"))
        .map(|line| {
            line.trim_start_matches("KEYWORDS:")
                .split(',')
                .map(|kw| kw.trim().to_string())
                .filter(|kw| !kw.is_empty())
                .collect()
        })
        .unwrap_or_default();

    // Extract MEMORY sections
    let mut new_ltm = LongTermMemory::default();
    if let Some(memory_start) = response.find("MEMORY:") {
        let memory_text = &response[memory_start + "MEMORY:".len()..];
        let mut current_section: Option<String> = None;

        for line in memory_text.lines() {
            let line = line.trim();
            if line.starts_with("### ") {
                current_section = Some(line.trim_start_matches("### ").trim().to_string());
            } else if line.starts_with("- ") {
                if let Some(ref section) = current_section {
                    let item = line.trim_start_matches("- ").trim().to_string();
                    if !item.is_empty() {
                        new_ltm.section_mut(section).push(item);
                    }
                }
            }
        }
    }

    let history_entry = HistoryEntry::new(summary, keywords);
    Some(ConsolidationResult {
        history_entry,
        memory_update: new_ltm,
    })
}
