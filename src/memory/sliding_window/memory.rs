use crate::llm::{ChatMessage, LlmApi};
use crate::memory::dynamic_cheatsheet::DynamicCheatsheet;
use crate::memory::persistence::MemoryPersistence;
use crate::memory::estimate_tokens;

use super::compact_ops::{self, CompactInput};
use super::config::{SlidingWindowConfig, ContextPressureLevel};
use super::consolidation::ConsolidationResult;
use super::consolidation_ops;
use super::history::HistoryEntry;
use super::long_term::LongTermMemory;
use super::session_state;
use crate::memory::{Memory, PersistentState};

// ── Compact result ──

/// Statistics from a context compression (compact) operation.
#[derive(Debug, Clone)]
pub struct CompactResult {
    /// Number of messages before compact.
    pub messages_before: usize,
    /// Number of messages after compact.
    pub messages_after: usize,
    /// Estimated token count before compact.
    pub estimated_tokens_before: usize,
    /// Estimated token count after compact.
    pub estimated_tokens_after: usize,
}

// ── Constants ──

/// Number of recent messages whose tool context is preserved in full
/// during micro-compact. Older tool context messages are truncated to
/// `MICRO_COMPACT_MAX_CHARS` characters.
const MICRO_COMPACT_PRESERVE_RECENT: usize = 6;

/// Maximum character length for tool context messages outside the
/// micro-compact preservation window.
const MICRO_COMPACT_MAX_CHARS: usize = 200;

/// Marker substring that identifies a tool context message produced by
/// `summarize_tool_history()` in the memory middleware.
const TOOL_CONTEXT_MARKER: &str = "[Tool call round ";

/// Maximum consecutive auto-compact failures before the circuit breaker
/// trips and stops retrying until the next manual `/compact` or new session.
const MAX_COMPACT_FAILURES: usize = 3;

/// Aggregated persistent components that survive across sessions.
///
/// Grouping these together simplifies `take_persistent_state` /
/// `restore_persistent_state` / `persist` — they all operate on
/// a single struct instead of individual fields.
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
pub struct SlidingWindowMemory {
    /// The original system prompt (without long-term memory injection).
    base_system_prompt: String,
    /// All conversation messages (user + assistant), in chronological order.
    pub(crate) messages: Vec<ChatMessage>,
    /// Persistent components (long-term memory, history log, cheatsheet).
    persistent: PersistentComponents,
    /// Index of the first unconsolidated message in `messages`.
    consolidation_cursor: usize,
    /// Configuration parameters.
    config: SlidingWindowConfig,
    /// Consecutive auto-compact failure count for circuit breaker.
    compact_failure_count: usize,
}

#[allow(dead_code)]
impl SlidingWindowMemory {
    // ── Construction ──

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
            compact_failure_count: 0,
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

    // ── Consolidation tracking ──

    /// Return the number of unconsolidated messages.
    pub fn unconsolidated_count(&self) -> usize {
        self.messages.len().saturating_sub(self.consolidation_cursor)
    }

    /// Get the messages that should be consolidated.
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

    /// Mark a message at the given index as semantically preserved.
    #[allow(dead_code)]
    pub fn mark_preserved(&mut self, index: usize, preserved: bool) -> bool {
        if let Some(msg) = self.messages.get_mut(index) {
            msg.preserved = preserved;
            true
        } else {
            false
        }
    }

    /// Auto-detect and mark semantically important messages as preserved.
    ///
    /// ## Rules
    /// 1. **First user message**: Always preserved (task instruction).
    /// 2. **Error messages**: Assistant messages with error indicators.
    /// 3. **Decision messages**: User messages with decision language.
    /// 4. **Already-preserved**: Never un-marked.
    pub(crate) fn auto_mark_preserved(&mut self) {
        let mut found_first_user = false;

        for msg in self.messages.iter_mut() {
            if msg.preserved {
                continue;
            }

            match msg.role {
                crate::llm::ChatRole::User => {
                    if !found_first_user {
                        msg.preserved = true;
                        found_first_user = true;
                        continue;
                    }

                    let lower = msg.content.to_lowercase();
                    if lower.contains("decide") || lower.contains("choose")
                        || lower.contains("go with") || lower.contains("let's use")
                        || lower.contains("i want") || lower.contains("please implement")
                        || lower.contains("please fix") || lower.contains("please add")
                    {
                        msg.preserved = true;
                    }
                }
                crate::llm::ChatRole::Assistant => {
                    let lower = msg.content.to_lowercase();
                    // Preserve error reports (but not tool context that happens to contain "error").
                    if (lower.contains("error") || lower.contains("failed")
                        || lower.contains("panic") || lower.contains("compilation failed"))
                        && !msg.content.contains(TOOL_CONTEXT_MARKER)
                    {
                        msg.preserved = true;
                    }
                    // Preserve plan/architecture content — these provide long-term
                    // structural context that should survive compact compression.
                    else if !msg.content.contains(TOOL_CONTEXT_MARKER)
                        && (lower.contains("## implementation plan")
                            || lower.contains("## plan")
                            || lower.contains("## approach")
                            || lower.contains("## steps")
                            || lower.contains("## goal")
                            || (lower.contains("step 1") && lower.contains("step 2")
                                && lower.contains("step 3")))
                    {
                        msg.preserved = true;
                    }
                }
                _ => {}
            }
        }
    }

    // ── Token estimation ──

    /// Estimate the total token count for the messages that would be sent to the LLM.
    pub fn estimate_token_count(&self) -> usize {
        let (system_tokens, message_tokens) = self.estimate_token_breakdown();
        system_tokens + message_tokens
    }

    /// Estimate token counts broken down by system prompt vs conversation messages.
    fn estimate_token_breakdown(&self) -> (usize, usize) {
        let system_prompt = self.effective_system_prompt();
        let window = self.windowed_messages();

        let system_tokens = estimate_tokens(&system_prompt);
        let message_tokens: usize = window.iter().map(|m| estimate_tokens(&m.content)).sum();

        (system_tokens, message_tokens)
    }

    /// Compute the cache-adjusted token count used for threshold comparisons.
    pub(crate) fn cache_adjusted_tokens(&self) -> usize {
        let (system_tokens, message_tokens) = self.estimate_token_breakdown();
        system_tokens / 4 + message_tokens
    }

    /// Return the current context pressure level based on multi-level thresholds.
    pub fn context_pressure_level(&self) -> ContextPressureLevel {
        let cache_adjusted = self.cache_adjusted_tokens();
        let budget = self.config.context_budget as f64;

        let hard_limit = (budget * self.config.compact_hard_limit_ratio) as usize;
        let threshold = (budget * self.config.compact_threshold_ratio) as usize;
        let warning = (budget * self.config.compact_warning_ratio) as usize;

        if cache_adjusted > hard_limit {
            ContextPressureLevel::Critical
        } else if cache_adjusted > threshold {
            ContextPressureLevel::High
        } else if cache_adjusted > warning {
            ContextPressureLevel::Warning
        } else {
            ContextPressureLevel::Normal
        }
    }

    /// Check whether auto-compact should be triggered based on token budget.
    pub fn should_compact(&self) -> bool {
        self.context_pressure_level() >= ContextPressureLevel::High
    }

    // ── Context Compression (Compact) — thin delegates to compact_ops ──

    /// Run context compression (compact) on the full conversation.
    pub async fn compact(
        &mut self,
        llm: &dyn LlmApi,
        custom_instruction: Option<&str>,
    ) -> anyhow::Result<CompactResult> {
        self.compact_with_range(llm, custom_instruction, None).await
    }

    /// Run partial compact on a specific range of messages.
    pub async fn compact_range(
        &mut self,
        llm: &dyn LlmApi,
        custom_instruction: Option<&str>,
        range: (usize, usize),
    ) -> anyhow::Result<CompactResult> {
        self.compact_with_range(llm, custom_instruction, Some(range)).await
    }

    /// Internal: delegates to `compact_ops::run_compact` and applies the output.
    async fn compact_with_range(
        &mut self,
        llm: &dyn LlmApi,
        custom_instruction: Option<&str>,
        range: Option<(usize, usize)>,
    ) -> anyhow::Result<CompactResult> {
        // Auto-detect semantically important messages before compressing.
        self.auto_mark_preserved();

        let estimated_tokens_before = self.estimate_token_count();

        let input = CompactInput {
            messages: &self.messages,
            config: &self.config,
        };

        // Token estimation callback — captures `self` state needed for estimation.
        // We use a simple per-message estimate here since the full system prompt
        // estimation requires `&self` which is already borrowed.
        let base_system_prompt = self.effective_system_prompt();
        let estimate_fn = |msgs: &[ChatMessage]| -> usize {
            let system_tokens = estimate_tokens(&base_system_prompt);
            let msg_tokens: usize = msgs.iter().map(|m| estimate_tokens(&m.content)).sum();
            system_tokens + msg_tokens
        };

        let output = compact_ops::run_compact(
            input,
            llm,
            custom_instruction,
            range,
            estimated_tokens_before,
            estimate_fn,
        ).await?;

        // Apply output to self — the only place that mutates fields.
        self.messages = output.new_messages;
        self.consolidation_cursor = output.new_consolidation_cursor;
        self.compact_failure_count = 0;

        Ok(output.result)
    }

    /// Run auto-compact if the context window is approaching the budget.
    ///
    /// Includes a circuit breaker: after `MAX_COMPACT_FAILURES` consecutive
    /// failures, auto-compact stops retrying until a manual `/compact` succeeds
    /// or a new session starts.
    pub async fn maybe_compact(&mut self, llm: &dyn LlmApi) {
        if self.compact_failure_count >= MAX_COMPACT_FAILURES {
            tracing::debug!(
                failures = self.compact_failure_count,
                "Auto-compact circuit breaker active, skipping"
            );
            return;
        }

        if !self.should_compact() {
            return;
        }

        let pressure = self.context_pressure_level();
        tracing::info!(
            estimated_tokens = self.estimate_token_count(),
            cache_adjusted_tokens = self.cache_adjusted_tokens(),
            budget = self.config.context_budget,
            ?pressure,
            "Auto-compact triggered: context approaching budget"
        );

        match self.compact(llm, None).await {
            Ok(result) => {
                tracing::info!(
                    messages_before = result.messages_before,
                    messages_after = result.messages_after,
                    tokens_before = result.estimated_tokens_before,
                    tokens_after = result.estimated_tokens_after,
                    "Auto-compact succeeded"
                );
            }
            Err(e) => {
                self.compact_failure_count += 1;
                tracing::warn!(
                    error = %e,
                    failures = self.compact_failure_count,
                    max_failures = MAX_COMPACT_FAILURES,
                    "Auto-compact failed, continuing without compression"
                );
            }
        }
    }

    // ── Consolidation — delegates parsing to consolidation_ops ──

    /// Run automatic consolidation if the threshold is reached.
    pub async fn maybe_consolidate(&mut self, llm: &dyn LlmApi) {
        if !self.should_consolidate() {
            return;
        }

        let to_consolidate = self.messages_to_consolidate();
        if to_consolidate.is_empty() {
            return;
        }

        let consolidated_up_to = self.messages.len().saturating_sub(self.config.retention_window);

        let messages_text = to_consolidate
            .iter()
            .map(|msg| format!("[{}]: {}", msg.role, msg.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        let current_memory = self.persistent.long_term_memory
            .to_markdown()
            .unwrap_or_default();

        let user_prompt = super::prompts::consolidation_user_prompt(
            &messages_text,
            &current_memory,
        );

        let messages = vec![
            ChatMessage::system(super::prompts::CONSOLIDATION_SYSTEM_PROMPT),
            ChatMessage::user(user_prompt),
        ];

        match llm.chat(&messages, None).await {
            Ok(response) => {
                match consolidation_ops::parse_consolidation_response(&response.content) {
                    Some(result) => {
                        tracing::info!(
                            consolidated_messages = consolidated_up_to - self.consolidation_cursor,
                            history_entries = self.persistent.history_log.len() + 1,
                            "Consolidation complete"
                        );
                        self.apply_consolidation(result, consolidated_up_to);
                    }
                    None => {
                        tracing::warn!(
                            "Failed to parse consolidation response, skipping this cycle"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Consolidation LLM call failed, continuing without consolidation"
                );
            }
        }
    }

    /// Parse the LLM's consolidation response (public for tests).
    pub(crate) fn parse_consolidation_response(response: &str) -> Option<ConsolidationResult> {
        consolidation_ops::parse_consolidation_response(response)
    }

    // ── Workspace persistence — delegates to session_state ──

    /// Save all persistent memory state to the workspace.
    pub fn save_to_workspace(
        &self,
        ltm_path: &std::path::Path,
        history_path: &std::path::Path,
        session_state_path: &std::path::Path,
    ) -> anyhow::Result<()> {
        self.persistent.long_term_memory.save(ltm_path)?;
        HistoryEntry::save_all(&self.persistent.history_log, history_path)?;
        session_state::save_session_state(
            &self.messages,
            self.consolidation_cursor,
            session_state_path,
        )?;
        tracing::info!(
            messages = self.messages.len(),
            consolidation_cursor = self.consolidation_cursor,
            "SlidingWindowMemory persisted to workspace"
        );
        Ok(())
    }

    /// Load persistent memory state from the workspace.
    pub fn load_from_workspace(
        &mut self,
        ltm_path: &std::path::Path,
        history_path: &std::path::Path,
        session_state_path: &std::path::Path,
    ) -> anyhow::Result<()> {
        self.persistent.long_term_memory = LongTermMemory::load(ltm_path)?;
        self.persistent.history_log = HistoryEntry::load_all(history_path)?;
        let loaded = session_state::load_session_state(session_state_path)?;
        self.messages = loaded.messages;
        self.consolidation_cursor = loaded.consolidation_cursor;
        tracing::info!(
            ltm_sections = self.persistent.long_term_memory.section_count(),
            history_entries = self.persistent.history_log.len(),
            session_messages = self.messages.len(),
            consolidation_cursor = self.consolidation_cursor,
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

    // ── Micro-compact (zero-LLM-cost context reduction) ──

    /// Truncate old tool context messages in-place to reduce context size.
    fn micro_compact(messages: &mut [ChatMessage]) {
        use crate::llm::ChatRole;

        let total = messages.len();
        if total <= 1 + MICRO_COMPACT_PRESERVE_RECENT {
            return;
        }

        let cutoff = total - MICRO_COMPACT_PRESERVE_RECENT;

        for msg in &mut messages[1..cutoff] {
            if msg.role != ChatRole::Assistant {
                continue;
            }
            if !msg.content.contains(TOOL_CONTEXT_MARKER) {
                continue;
            }
            if msg.content.len() <= MICRO_COMPACT_MAX_CHARS {
                continue;
            }

            let truncated_lines: Vec<String> = msg.content
                .lines()
                .map(|line| {
                    if line.starts_with(TOOL_CONTEXT_MARKER) && line.len() > MICRO_COMPACT_MAX_CHARS {
                        let preview: String = line.chars().take(MICRO_COMPACT_MAX_CHARS).collect();
                        format!("{}...(truncated)]", preview)
                    } else {
                        line.to_string()
                    }
                })
                .collect();

            msg.content = truncated_lines.join("\n");
        }
    }
}

// ── Memory trait implementation ──

impl Memory for SlidingWindowMemory {
    fn add_user_message(&mut self, content: &str) {
        self.messages.push(ChatMessage::user(content));
    }

    fn add_assistant_message(&mut self, content: &str) {
        self.messages.push(ChatMessage::assistant(content));
    }

    fn build_messages(&self) -> Vec<ChatMessage> {
        use crate::llm::CacheControl;
        let system_prompt = self.effective_system_prompt();
        let window = self.windowed_messages();

        let mut messages = Vec::with_capacity(1 + window.len());
        messages.push(
            ChatMessage::system(system_prompt)
                .with_cache_control(CacheControl::Ephemeral)
        );
        messages.extend(window.iter().cloned());

        Self::micro_compact(&mut messages);

        // Filter out non-system messages with empty content.
        messages.retain(|msg| {
            msg.role == crate::llm::ChatRole::System || !msg.content.is_empty()
        });

        messages
    }

    fn clear(&mut self) {
        self.messages.clear();
        self.consolidation_cursor = 0;
    }

    fn should_consolidate(&self) -> bool {
        self.unconsolidated_count() >= self.config.consolidation_threshold
    }

    fn search_history(&self, query: &str, limit: Option<usize>) -> Vec<String> {
        self.search_history(query, limit)
            .into_iter()
            .map(|entry| entry.to_log_line())
            .collect()
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
            &workspace.session_messages_path(),
        )?;

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

    fn maybe_consolidate<'a>(
        &'a mut self,
        llm: &'a dyn LlmApi,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            self.maybe_consolidate(llm).await;
        })
    }

    fn should_compact(&self) -> bool {
        self.should_compact()
    }

    fn context_pressure_level(&self) -> ContextPressureLevel {
        self.context_pressure_level()
    }

    fn maybe_compact<'a>(
        &'a mut self,
        llm: &'a dyn LlmApi,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            self.maybe_compact(llm).await;
        })
    }

    fn compact<'a>(
        &'a mut self,
        llm: &'a dyn LlmApi,
        instruction: Option<&'a str>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<String>> + Send + 'a>> {
        Box::pin(async move {
            let result = self.compact(llm, instruction).await?;
            Ok(format!(
                "Compact complete: {} → {} messages, ~{} → ~{} tokens",
                result.messages_before,
                result.messages_after,
                result.estimated_tokens_before,
                result.estimated_tokens_after,
            ))
        })
    }

    fn compact_range<'a>(
        &'a mut self,
        llm: &'a dyn LlmApi,
        instruction: Option<&'a str>,
        range: (usize, usize),
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<String>> + Send + 'a>> {
        Box::pin(async move {
            let result = self.compact_range(llm, instruction, range).await?;
            Ok(format!(
                "Partial compact complete: {} → {} messages, ~{} → ~{} tokens (range {}..{})",
                result.messages_before,
                result.messages_after,
                result.estimated_tokens_before,
                result.estimated_tokens_after,
                range.0,
                range.1.min(result.messages_before),
            ))
        })
    }
}
