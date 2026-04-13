use super::history::HistoryEntry;
use super::long_term::LongTermMemory;

/// The output produced by the consolidation LLM (or manual consolidation).
///
/// Contains both the history summary (appended to history log) and the
/// updated long-term memory (replaces existing long-term memory).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ConsolidationResult {
    /// A 2-5 sentence event summary to append to the history log.
    pub history_entry: HistoryEntry,
    /// The complete updated long-term memory (replaces existing).
    pub memory_update: LongTermMemory,
}
