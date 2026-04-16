use crate::llm::{ChatMessage, LlmApi};
use crate::memory::dynamic_cheatsheet::DynamicCheatsheet;
use crate::memory::persistence::MemoryPersistence;

use super::config::SlidingWindowConfig;
use super::consolidation::ConsolidationResult;
use super::history::HistoryEntry;
use super::long_term::LongTermMemory;
use crate::memory::{Memory, PersistentState};

/// Aggregated persistent components that survive across sessions.
///
/// Grouping these together simplifies `take_persistent_state` /
/// `restore_persistent_state` / `persist` — they all operate on
/// a single struct instead of individual fields. This also makes
/// adding new persistent components a one-line change.
pub(crate) struct PersistentComponents {
    pub(crate) long_term_memory: LongTermMemory,
    pub(crate) history_log: Vec<HistoryEntry>,
    pub(crate) cheatsheet: Option<DynamicCheatsheet>,
}

/// Sliding window memory with dual-layer consolidation.
///
/// This is the default memory strategy for Daedalus. It combines:
///
/// 1. **Hot data** (automatic): Recent conversation messages within the
///    sliding window, sent to the LLM on every request.
/// 2. **Long-term memory** (automatic): Key facts extracted from past
///    conversations, injected into the system prompt.
/// 3. **History log** (on-demand): Append-only event summaries that can
///    be searched by keyword when the agent needs to recall past events.
///
/// ## Consolidation
///
/// When the number of unconsolidated messages exceeds `consolidation_threshold`,
/// the system should trigger consolidation (driven externally by the agent or
/// a background task). Consolidation:
/// - Extracts key facts → updates long-term memory
/// - Generates a summary → appends to history log
/// - Advances the `consolidation_cursor`
///
/// The retention window ensures recent messages are never consolidated,
/// preserving immediate context.
pub struct SlidingWindowMemory {
    /// The original system prompt (without long-term memory injection).
    base_system_prompt: String,
    /// All conversation messages (user + assistant), in chronological order.
    messages: Vec<ChatMessage>,
    /// Persistent components (long-term memory, history log, cheatsheet).
    /// Grouped for clean migration and persistence.
    persistent: PersistentComponents,
    /// Index of the first unconsolidated message in `messages`.
    /// All messages before this index have already been consolidated.
    consolidation_cursor: usize,
    /// Configuration parameters.
    config: SlidingWindowConfig,
}

#[allow(dead_code)]
impl SlidingWindowMemory {
    /// Create a new sliding window memory with the given config.
    pub fn new(system_prompt: &str, config: SlidingWindowConfig) -> Self {
        Self {
            base_system_prompt: system_prompt.to_string(),
            messages: Vec::new(),
            persistent: PersistentComponents {
                long_term_memory: LongTermMemory::default(),
                history_log: Vec::new(),
                cheatsheet: None,
            },
            consolidation_cursor: 0,
            config,
        }
    }

    /// Create a new memory with unlimited window (full history, no consolidation).
    pub fn unlimited(system_prompt: &str) -> Self {
        Self::new(system_prompt, SlidingWindowConfig::unlimited())
    }

    /// Create a new memory with default consolidation settings.
    pub fn with_defaults(system_prompt: &str) -> Self {
        Self::new(system_prompt, SlidingWindowConfig::default())
    }

    /// Return the configured max messages (None = unlimited).
    pub fn max_messages(&self) -> Option<usize> {
        self.config.max_messages
    }

    // ── Long-term memory access ──

    /// Get a reference to the long-term memory.
    pub fn long_term_memory(&self) -> &LongTermMemory {
        &self.persistent.long_term_memory
    }

    /// Get a mutable reference to the long-term memory.
    pub fn long_term_memory_mut(&mut self) -> &mut LongTermMemory {
        &mut self.persistent.long_term_memory
    }

    // ── Dynamic Cheatsheet access ──

    /// Get a reference to the dynamic cheatsheet (if enabled).
    pub fn cheatsheet(&self) -> Option<&DynamicCheatsheet> {
        self.persistent.cheatsheet.as_ref()
    }

    /// Get a mutable reference to the dynamic cheatsheet (if enabled).
    pub fn cheatsheet_mut(&mut self) -> Option<&mut DynamicCheatsheet> {
        self.persistent.cheatsheet.as_mut()
    }

    /// Enable the dynamic cheatsheet with the given instance.
    pub fn set_cheatsheet(&mut self, cheatsheet: DynamicCheatsheet) {
        self.persistent.cheatsheet = Some(cheatsheet);
    }

    // ── History log access ──

    /// Get all history entries.
    pub fn history_log(&self) -> &[HistoryEntry] {
        &self.persistent.history_log
    }

    /// Append a history entry to the log.
    pub fn append_history_entry(&mut self, entry: HistoryEntry) {
        self.persistent.history_log.push(entry);
    }

    /// Search history entries by keyword (case-insensitive).
    ///
    /// Returns entries whose summary or keywords contain the query string.
    /// When `limit` is `Some(n)`, at most `n` entries are returned;
    /// when `None`, all matching entries are returned.
    pub fn search_history(&self, query: &str, limit: Option<usize>) -> Vec<&HistoryEntry> {
        let query_lower = query.to_lowercase();
        let iter = self.persistent.history_log
            .iter()
            .filter(|entry| {
                entry.summary.to_lowercase().contains(&query_lower)
                    || entry.keywords.iter().any(|kw| kw.to_lowercase().contains(&query_lower))
            });
        match limit {
            Some(n) => iter.take(n).collect(),
            None => iter.collect(),
        }
    }

    // ── Consolidation ──

    /// Return the number of unconsolidated messages.
    pub fn unconsolidated_count(&self) -> usize {
        self.messages.len().saturating_sub(self.consolidation_cursor)
    }

    /// Get the messages that should be consolidated.
    ///
    /// Returns messages from `consolidation_cursor` up to (but not including)
    /// the retention window. Returns empty if there's nothing to consolidate.
    pub fn messages_to_consolidate(&self) -> &[ChatMessage] {
        let total = self.messages.len();
        let retain_start = total.saturating_sub(self.config.retention_window);
        if self.consolidation_cursor >= retain_start {
            return &[];
        }
        &self.messages[self.consolidation_cursor..retain_start]
    }

    /// Get all unconsolidated messages (for full archive, e.g., `/new` command).
    pub fn all_unconsolidated_messages(&self) -> &[ChatMessage] {
        if self.consolidation_cursor >= self.messages.len() {
            return &[];
        }
        &self.messages[self.consolidation_cursor..]
    }

    /// Apply a consolidation result: update long-term memory, append history
    /// entry, and advance the consolidation cursor.
    pub fn apply_consolidation(&mut self, result: ConsolidationResult, consolidated_up_to: usize) {
        self.persistent.long_term_memory.replace_with(result.memory_update);
        self.persistent.history_log.push(result.history_entry);
        self.consolidation_cursor = consolidated_up_to;
    }

    /// Apply a full archive consolidation (e.g., `/new` command).
    pub fn apply_full_archive(&mut self, result: ConsolidationResult) {
        self.persistent.long_term_memory.replace_with(result.memory_update);
        self.persistent.history_log.push(result.history_entry);
        self.messages.clear();
        self.consolidation_cursor = 0;
    }

    // ── Workspace persistence ──

    /// Save all persistent memory state to the workspace.
    ///
    /// Saves:
    /// - LongTermMemory → `memory/long_term.json`
    /// - HistoryLog → `memory/history.jsonl`
    pub fn save_to_workspace(
        &self,
        ltm_path: &std::path::Path,
        history_path: &std::path::Path,
    ) -> anyhow::Result<()> {
        self.persistent.long_term_memory.save(ltm_path)?;
        HistoryEntry::save_all(&self.persistent.history_log, history_path)?;
        tracing::info!("SlidingWindowMemory persisted to workspace");
        Ok(())
    }

    /// Load persistent memory state from the workspace.
    ///
    /// Loads:
    /// - LongTermMemory from `memory/long_term.json`
    /// - HistoryLog from `memory/history.jsonl`
    pub fn load_from_workspace(
        &mut self,
        ltm_path: &std::path::Path,
        history_path: &std::path::Path,
    ) -> anyhow::Result<()> {
        self.persistent.long_term_memory = LongTermMemory::load(ltm_path)?;
        self.persistent.history_log = HistoryEntry::load_all(history_path)?;
        tracing::info!(
            ltm_sections = self.persistent.long_term_memory.section_count(),
            history_entries = self.persistent.history_log.len(),
            "SlidingWindowMemory loaded from workspace"
        );
        Ok(())
    }

    // ── Internal helpers ──

    /// Build the effective system prompt by injecting long-term memory
    /// and dynamic cheatsheet.
    fn effective_system_prompt(&self) -> String {
        let mut prompt = self.base_system_prompt.clone();

        if let Some(memory_md) = self.persistent.long_term_memory.to_markdown() {
            prompt = format!("{}\n\n{}", prompt, memory_md);
        }

        if let Some(ref cs) = self.persistent.cheatsheet {
            if let Some(cs_md) = cs.to_markdown() {
                prompt = format!("{}\n\n{}", prompt, cs_md);
            }
        }

        prompt
    }

    /// Get the windowed slice of messages to send to the LLM.
    fn windowed_messages(&self) -> &[ChatMessage] {
        match self.config.max_messages {
            None => &self.messages[..],
            Some(max) => {
                if self.messages.len() <= max {
                    &self.messages[..]
                } else {
                    &self.messages[self.messages.len() - max..]
                }
            }
        }
    }
}

impl Memory for SlidingWindowMemory {
    fn add_user_message(&mut self, content: &str) {
        self.messages.push(ChatMessage::user(content));
    }

    fn add_assistant_message(&mut self, content: &str) {
        self.messages.push(ChatMessage::assistant(content));
    }

    fn build_messages(&self) -> Vec<ChatMessage> {
        let system_prompt = self.effective_system_prompt();
        let window = self.windowed_messages();

        let mut messages = Vec::with_capacity(1 + window.len());
        messages.push(ChatMessage::system(system_prompt));
        messages.extend(window.iter().cloned());
        messages
    }

    fn clear(&mut self) {
        self.messages.clear();
        self.consolidation_cursor = 0;
    }

    fn should_consolidate(&self) -> bool {
        self.unconsolidated_count() >= self.config.consolidation_threshold
    }

    fn turn_count(&self) -> usize {
        self.messages.len() / 2
    }

    fn strategy_name(&self) -> &str {
        "sliding_window"
    }

    fn take_persistent_state(&mut self) -> Option<PersistentState> {
        let components = PersistentComponents {
            long_term_memory: std::mem::take(&mut self.persistent.long_term_memory),
            history_log: std::mem::take(&mut self.persistent.history_log),
            cheatsheet: self.persistent.cheatsheet.take(),
        };
        Some(PersistentState::new(components))
    }

    fn restore_persistent_state(&mut self, state: PersistentState) {
        match state.downcast::<PersistentComponents>() {
            Ok(components) => {
                self.persistent = components;
            }
            Err(_) => {
                tracing::warn!(
                    "Persistent state type mismatch, state discarded"
                );
            }
        }
    }

    fn persist(&self, workspace: &crate::workspace::Workspace) -> anyhow::Result<()> {
        self.save_to_workspace(
            &workspace.long_term_memory_path(),
            &workspace.history_log_path(),
        )?;

        // Persist dynamic cheatsheet if enabled.
        if let Some(ref cs) = self.persistent.cheatsheet {
            cs.save(&workspace.cheatsheet_path())?;
        }

        Ok(())
    }

    fn reflect_on_turn<'a>(
        &'a mut self,
        user_input: &'a str,
        assistant_response: &'a str,
        llm: &'a dyn LlmApi,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            if let Some(ref mut cheatsheet) = self.persistent.cheatsheet {
                cheatsheet.reflect(user_input, assistant_response, llm).await;
            }
        })
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
