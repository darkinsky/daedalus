use crate::llm::{ChatMessage, LlmApi};
use crate::memory::dynamic_cheatsheet::DynamicCheatsheet;
use crate::memory::persistence::{MemoryPersistence, atomic_write};
use crate::memory::estimate_tokens;

use super::config::{SlidingWindowConfig, ContextPressureLevel};
use super::consolidation::ConsolidationResult;
use super::history::HistoryEntry;
use super::long_term::LongTermMemory;
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

// ── Session state serialization ──

/// Serializable representation of a ChatMessage for disk persistence.
#[derive(serde::Serialize, serde::Deserialize)]
struct SerializableMessage {
    role: String,
    content: String,
}

impl From<&ChatMessage> for SerializableMessage {
    fn from(msg: &ChatMessage) -> Self {
        Self {
            role: msg.role.to_string(),
            content: msg.content.clone(),
        }
    }
}

impl SerializableMessage {
    fn to_chat_message(&self) -> ChatMessage {
        match self.role.as_str() {
            "system" => ChatMessage::system(&self.content),
            "user" => ChatMessage::user(&self.content),
            "assistant" => ChatMessage::assistant(&self.content),
            "tool" => ChatMessage::tool(&self.content),
            _ => ChatMessage::user(&self.content), // fallback
        }
    }
}

/// Serializable session state: messages + consolidation cursor.
///
/// The `consolidation_cursor` must be persisted alongside messages so that
/// after a restart we know which messages have already been consolidated.
/// Without it, all messages would appear unconsolidated, causing duplicate
/// consolidation.
#[derive(serde::Serialize, serde::Deserialize)]
struct SessionState {
    /// Index of the first unconsolidated message.
    consolidation_cursor: usize,
    /// All conversation messages in chronological order.
    messages: Vec<SerializableMessage>,
}

/// Save session state (messages + consolidation cursor) to a JSON file atomically.
fn save_session_state(
    messages: &[ChatMessage],
    consolidation_cursor: usize,
    path: &std::path::Path,
) -> anyhow::Result<()> {
    let state = SessionState {
        consolidation_cursor,
        messages: messages.iter().map(SerializableMessage::from).collect(),
    };
    let json = serde_json::to_string(&state)
        .map_err(|e| anyhow::anyhow!("Failed to serialize session state: {}", e))?;
    atomic_write(path, json.as_bytes())?;
    Ok(())
}

/// Loaded session state from disk.
struct LoadedSessionState {
    messages: Vec<ChatMessage>,
    consolidation_cursor: usize,
}

/// Load session state from a JSON file.
/// Returns empty state if the file doesn't exist.
/// Handles backward compatibility with the old format (plain message array).
fn load_session_state(path: &std::path::Path) -> anyhow::Result<LoadedSessionState> {
    if !path.exists() {
        return Ok(LoadedSessionState {
            messages: Vec::new(),
            consolidation_cursor: 0,
        });
    }
    let data = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Failed to read session state: {}", e))?;

    // Try new format first (object with consolidation_cursor + messages)
    if let Ok(state) = serde_json::from_str::<SessionState>(&data) {
        return Ok(LoadedSessionState {
            messages: state.messages.iter().map(|m| m.to_chat_message()).collect(),
            consolidation_cursor: state.consolidation_cursor,
        });
    }

    // Fallback: old format (plain array of messages, no cursor)
    let serializable: Vec<SerializableMessage> = serde_json::from_str(&data)
        .map_err(|e| anyhow::anyhow!("Failed to deserialize session state: {}", e))?;
    Ok(LoadedSessionState {
        messages: serializable.iter().map(|m| m.to_chat_message()).collect(),
        consolidation_cursor: 0,
    })
}

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

/// Content prefix that identifies a compact boundary message.
///
/// When compact runs, it produces a summary message with this prefix.
/// On subsequent compacts, we look for this marker to find the boundary
/// and only compress messages *after* it (incremental compression).
const COMPACT_BOUNDARY_PREFIX: &str = "[Previous conversation context \u{2014}";

pub struct SlidingWindowMemory {
    /// The original system prompt (without long-term memory injection).
    base_system_prompt: String,
    /// All conversation messages (user + assistant), in chronological order.
    pub(crate) messages: Vec<ChatMessage>,
    /// Persistent components (long-term memory, history log, cheatsheet).
    /// Grouped for clean migration and persistence.
    persistent: PersistentComponents,
    /// Index of the first unconsolidated message in `messages`.
    /// All messages before this index have already been consolidated.
    consolidation_cursor: usize,
    /// Configuration parameters.
    config: SlidingWindowConfig,
    /// Consecutive auto-compact failure count for circuit breaker.
    /// Reset to 0 on success or manual `/compact`.
    compact_failure_count: usize,
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

    /// Mark a message at the given index as semantically preserved.
    ///
    /// Preserved messages are never compressed by the compact algorithm,
    /// even if they fall outside the `compact_preserve_recent` window.
    /// This is useful for marking the user's initial task instruction,
    /// critical error messages, or important decision points.
    ///
    /// Returns `false` if the index is out of bounds.
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
    /// This applies heuristic rules to identify messages that should survive
    /// compact compression. Called automatically before compact runs.
    ///
    /// ## Rules
    ///
    /// 1. **First user message**: The initial task instruction is always preserved
    ///    (it defines the user's goal for the session).
    /// 2. **Error messages**: Assistant messages containing error indicators
    ///    ("error", "failed", "panic") are preserved (they provide debugging context).
    /// 3. **Decision messages**: User messages containing decision language
    ///    ("decide", "choose", "go with", "let's use") are preserved.
    /// 4. **Already-preserved messages**: Messages explicitly marked by the user
    ///    or agent are never un-marked.
    pub(crate) fn auto_mark_preserved(&mut self) {
        let mut found_first_user = false;

        for msg in self.messages.iter_mut() {
            // Never un-mark explicitly preserved messages.
            if msg.preserved {
                continue;
            }

            match msg.role {
                crate::llm::ChatRole::User => {
                    // Rule 1: First user message is the task instruction.
                    if !found_first_user {
                        msg.preserved = true;
                        found_first_user = true;
                        continue;
                    }

                    // Rule 3: Decision language.
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
                    // Rule 2: Error indicators.
                    let lower = msg.content.to_lowercase();
                    if (lower.contains("error") || lower.contains("failed")
                        || lower.contains("panic") || lower.contains("compilation failed"))
                        && !msg.content.contains(TOOL_CONTEXT_MARKER)
                    {
                        msg.preserved = true;
                    }
                }
                _ => {}
            }
        }
    }

    // ── Context Compression (Compact) ──

    /// Estimate the total token count for the messages that would be sent to the LLM.
    ///
    /// Uses the CJK-aware `estimate_tokens()` heuristic that handles mixed
    /// Chinese/English content accurately. This is intentionally approximate —
    /// the goal is to detect when we're approaching the context budget, not to be exact.
    ///
    /// The system prompt is counted separately from conversation messages
    /// so that callers (like `should_compact`) can reason about which part
    /// of the context is growing.
    pub fn estimate_token_count(&self) -> usize {
        let (system_tokens, message_tokens) = self.estimate_token_breakdown();
        system_tokens + message_tokens
    }

    /// Estimate token counts broken down by system prompt vs conversation messages.
    ///
    /// Returns `(system_prompt_tokens, conversation_message_tokens)`.
    /// The system prompt portion is largely cacheable (especially the static prefix),
    /// so callers can use this to make cache-aware decisions about when to compact.
    ///
    /// Uses the CJK-aware `estimate_tokens()` function for accurate estimation
    /// of mixed Chinese/English content.
    fn estimate_token_breakdown(&self) -> (usize, usize) {
        let system_prompt = self.effective_system_prompt();
        let window = self.windowed_messages();

        let system_tokens = estimate_tokens(&system_prompt);
        let message_tokens: usize = window.iter().map(|m| estimate_tokens(&m.content)).sum();

        (system_tokens, message_tokens)
    }

    /// Compute the cache-adjusted token count used for threshold comparisons.
    ///
    /// Discounts system prompt tokens by 75% to account for prompt cache.
    pub(crate) fn cache_adjusted_tokens(&self) -> usize {
        let (system_tokens, message_tokens) = self.estimate_token_breakdown();
        system_tokens / 4 + message_tokens
    }

    /// Return the current context pressure level based on multi-level thresholds.
    ///
    /// The pressure level determines how aggressively the system should compact:
    /// - `Normal`: Below warning threshold. No action needed.
    /// - `Warning`: Above warning threshold. Log a warning for observability.
    /// - `High`: Above auto-compact threshold. Trigger auto-compact.
    /// - `Critical`: Above hard limit. Force compact even if consolidation just ran.
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
    ///
    /// Uses a cache-aware heuristic: the system prompt is largely served from
    /// prompt cache (especially the static prefix before the cache boundary),
    /// so we discount it by 75% when estimating effective context usage.
    /// This prevents premature auto-compact when the system prompt is large
    /// but mostly cached.
    ///
    /// Returns `true` when the context pressure level is `High` or `Critical`.
    pub fn should_compact(&self) -> bool {
        self.context_pressure_level() >= ContextPressureLevel::High
    }

    /// Run context compression (compact) with incremental boundary support
    /// and semantic preservation.
    ///
    /// Compresses the conversation history into a summary, preserving:
    /// - The most recent `compact_preserve_recent` messages (positional)
    /// - Messages marked as `preserved = true` (semantic)
    ///
    /// ## Incremental Compression (compactBoundary)
    ///
    /// If a previous compact summary exists (identified by `COMPACT_BOUNDARY_PREFIX`),
    /// only messages *after* the boundary are compressed. The previous summary is
    /// passed to the LLM as context, and the new summary replaces both the old
    /// summary and the new messages. This makes compact cost O(ΔN) instead of O(N).
    ///
    /// ## Partial Compact
    ///
    /// When `range` is provided, only messages within that range are compressed.
    /// Messages outside the range are kept verbatim. This allows fine-grained
    /// control over which part of the conversation to compress.
    ///
    /// ## Algorithm
    ///
    /// 1. Auto-detect semantically important messages (`auto_mark_preserved`)
    /// 2. Find the most recent compact boundary (if any)
    /// 3. Split messages into groups:
    ///    - **Previous summary**: the boundary message (if exists)
    ///    - **To compress**: non-preserved messages after boundary, except recent N
    ///    - **To preserve**: recent N messages + semantically preserved messages
    /// 4. Call the LLM to generate a structured summary
    /// 5. Replace the message list with: `[new_compact_summary] + [preserved messages]`
    /// 6. Reset the consolidation cursor
    ///
    /// ## Arguments
    /// * `llm` — The LLM provider for generating the summary.
    /// * `custom_instruction` — Optional user instruction to focus the summary.
    /// * `range` — Optional `(start, end)` range of message indices to compress.
    ///   If `None`, compresses all messages except the most recent N.
    ///
    /// ## Returns
    /// * `Ok(CompactResult)` with statistics on success.
    /// * `Err` if the LLM call fails (the original messages are NOT modified).
    pub async fn compact(
        &mut self,
        llm: &dyn LlmApi,
        custom_instruction: Option<&str>,
    ) -> anyhow::Result<CompactResult> {
        self.compact_with_range(llm, custom_instruction, None).await
    }

    /// Run partial compact on a specific range of messages.
    ///
    /// `range` is `(start_index, end_index)` — inclusive start, exclusive end.
    /// Messages within the range are compressed; messages outside are kept verbatim.
    /// Semantically preserved messages within the range are still kept.
    pub async fn compact_range(
        &mut self,
        llm: &dyn LlmApi,
        custom_instruction: Option<&str>,
        range: (usize, usize),
    ) -> anyhow::Result<CompactResult> {
        self.compact_with_range(llm, custom_instruction, Some(range)).await
    }

    /// Internal implementation of compact with optional range support.
    async fn compact_with_range(
        &mut self,
        llm: &dyn LlmApi,
        custom_instruction: Option<&str>,
        range: Option<(usize, usize)>,
    ) -> anyhow::Result<CompactResult> {
        // Auto-detect semantically important messages before compressing.
        self.auto_mark_preserved();

        let total_messages = self.messages.len();
        let estimated_tokens_before = self.estimate_token_count();

        // Determine the compressible range.
        let (range_start, range_end) = match range {
            Some((s, e)) => (s.min(total_messages), e.min(total_messages)),
            None => {
                // Default: compress everything except the most recent N.
                let preserve_count = self.config.compact_preserve_recent.min(total_messages);
                (0, total_messages - preserve_count)
            }
        };

        if range_start >= range_end {
            return Ok(CompactResult {
                messages_before: total_messages,
                messages_after: total_messages,
                estimated_tokens_before,
                estimated_tokens_after: estimated_tokens_before,
            });
        }

        // Within the compressible range, find the compact boundary.
        let boundary_idx = self.messages[range_start..range_end]
            .iter()
            .rposition(|msg| msg.content.starts_with(COMPACT_BOUNDARY_PREFIX))
            .map(|idx| range_start + idx);

        // Extract previous summary and determine actual compress start.
        let (previous_summary, compress_start) = match boundary_idx {
            Some(idx) => {
                let summary = self.messages[idx].content.clone();
                (Some(summary), idx + 1)
            }
            None => (None, range_start),
        };

        // Separate messages into: to-compress vs semantically-preserved.
        // Messages with `preserved = true` within the compress range are
        // extracted and kept verbatim.
        let mut messages_to_compress = Vec::new();
        let mut preserved_in_range = Vec::new();

        for (i, msg) in self.messages[compress_start..range_end].iter().enumerate() {
            if msg.preserved {
                preserved_in_range.push((compress_start + i, msg.clone()));
            } else {
                messages_to_compress.push(msg);
            }
        }

        let preserved_semantic_count = preserved_in_range.len();

        // If there's nothing to compress, skip the LLM call.
        if messages_to_compress.is_empty() {
            return Ok(CompactResult {
                messages_before: total_messages,
                messages_after: total_messages,
                estimated_tokens_before,
                estimated_tokens_after: self.estimate_token_count(),
            });
        }

        // Build text representation of messages to compress.
        let messages_text = messages_to_compress
            .iter()
            .map(|msg| format!("[{}]: {}", msg.role, msg.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        let user_prompt = super::prompts::compact_user_prompt(
            &messages_text,
            custom_instruction,
            previous_summary.as_deref(),
        );

        let llm_messages = vec![
            ChatMessage::system(super::prompts::COMPACT_SYSTEM_PROMPT),
            ChatMessage::user(user_prompt),
        ];

        let response = llm.chat(&llm_messages, None).await?;
        let summary = Self::parse_compact_response(&response.content);

        // Rebuild the message list:
        // [before_range] + [compact_summary] + [preserved_in_range] + [after_range]
        let before_range: Vec<ChatMessage> = if range_start > 0 {
            // For non-partial compact (range_start == 0), there's nothing before.
            // For partial compact, keep messages before the range.
            self.messages[..range_start].to_vec()
        } else {
            Vec::new()
        };

        // Remove boundary from before_range if it was included
        // (boundary is consumed into the new summary).

        let after_range: Vec<ChatMessage> = self.messages[range_end..].to_vec();

        let compressed_count = messages_to_compress.len()
            + if boundary_idx.is_some() { 1 } else { 0 }; // boundary is also consumed

        self.messages.clear();
        self.messages.extend(before_range);
        self.messages.push(ChatMessage::user(format!(
            "[Previous conversation context \u{2014} {} messages compressed into summary]\n\n{}",
            compressed_count, summary,
        )));
        // Re-insert semantically preserved messages (in original order).
        for (_orig_idx, msg) in preserved_in_range {
            self.messages.push(msg);
        }
        self.messages.extend(after_range);

        // Reset consolidation cursor.
        self.consolidation_cursor = 1;

        // Reset circuit breaker on successful compact.
        self.compact_failure_count = 0;

        let estimated_tokens_after = self.estimate_token_count();

        let is_incremental = previous_summary.is_some();
        let is_partial = range.is_some();
        tracing::info!(
            messages_before = total_messages,
            messages_after = self.messages.len(),
            tokens_before = estimated_tokens_before,
            tokens_after = estimated_tokens_after,
            compressed = compressed_count,
            preserved_semantic = preserved_semantic_count,
            incremental = is_incremental,
            partial = is_partial,
            "Context compact complete"
        );

        Ok(CompactResult {
            messages_before: total_messages,
            messages_after: self.messages.len(),
            estimated_tokens_before,
            estimated_tokens_after,
        })
    }
    /// Parse the compact LLM response, extracting the summary content.
    ///
    /// Tries to extract content between `<compact_summary>` tags.
    /// Falls back to using the entire response if tags are not found.
    fn parse_compact_response(response: &str) -> String {
        let response = response.trim();

        // Try to extract content between <compact_summary> tags
        if let Some(start) = response.find("<compact_summary>") {
            let content_start = start + "<compact_summary>".len();
            if let Some(end) = response[content_start..].find("</compact_summary>") {
                return response[content_start..content_start + end].trim().to_string();
            }
        }

        // Fallback: use the entire response
        response.to_string()
    }

    /// Run auto-compact if the context window is approaching the budget.
    ///
    /// This is called by the memory middleware after each turn. It checks
    /// `should_compact()` and, if true, runs `compact()` with no custom
    /// instruction.
    ///
    /// Includes a circuit breaker: after `MAX_COMPACT_FAILURES` consecutive
    /// failures, auto-compact stops retrying until a manual `/compact` succeeds
    /// or a new session starts. This prevents wasting API calls when the LLM
    /// provider is experiencing issues.
    ///
    /// Compact failures are logged but never propagated — they must not
    /// disrupt the main conversation flow.
    pub async fn maybe_compact(&mut self, llm: &dyn LlmApi) {
        // Circuit breaker: stop retrying after too many consecutive failures.
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
                // compact() already resets compact_failure_count
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

    /// Run automatic consolidation if the threshold is reached.
    ///
    /// This is the main entry point for triggering consolidation from the
    /// middleware layer. It checks `should_consolidate()`, and if true:
    /// 1. Extracts the messages to consolidate
    /// 2. Calls the LLM to generate a summary and updated long-term memory
    /// 3. Applies the consolidation result
    ///
    /// Consolidation failures are logged but never propagated — they must not
    /// disrupt the main conversation flow.
    pub async fn maybe_consolidate(&mut self, llm: &dyn LlmApi) {
        if !self.should_consolidate() {
            return;
        }

        let to_consolidate = self.messages_to_consolidate();
        if to_consolidate.is_empty() {
            return;
        }

        let consolidated_up_to = self.messages.len().saturating_sub(self.config.retention_window);

        // Build text representation of messages to consolidate
        let messages_text = to_consolidate
            .iter()
            .map(|msg| format!("[{}]: {}", msg.role, msg.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        // Get current long-term memory state for context
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
                match Self::parse_consolidation_response(&response.content) {
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
    pub(crate) fn parse_consolidation_response(response: &str) -> Option<ConsolidationResult> {
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

    // ── Workspace persistence ──

    /// Save all persistent memory state to the workspace.
    ///
    /// Saves:
    /// - LongTermMemory → `memory/long_term.json`
    /// - HistoryLog → `memory/history.jsonl`
    /// - SessionState (messages + consolidation_cursor) → `memory/session_messages.json`
    pub fn save_to_workspace(
        &self,
        ltm_path: &std::path::Path,
        history_path: &std::path::Path,
        session_state_path: &std::path::Path,
    ) -> anyhow::Result<()> {
        self.persistent.long_term_memory.save(ltm_path)?;
        HistoryEntry::save_all(&self.persistent.history_log, history_path)?;
        save_session_state(&self.messages, self.consolidation_cursor, session_state_path)?;
        tracing::info!(
            messages = self.messages.len(),
            consolidation_cursor = self.consolidation_cursor,
            "SlidingWindowMemory persisted to workspace"
        );
        Ok(())
    }

    /// Load persistent memory state from the workspace.
    ///
    /// Loads:
    /// - LongTermMemory from `memory/long_term.json`
    /// - HistoryLog from `memory/history.jsonl`
    /// - SessionState (messages + consolidation_cursor) from `memory/session_messages.json`
    pub fn load_from_workspace(
        &mut self,
        ltm_path: &std::path::Path,
        history_path: &std::path::Path,
        session_state_path: &std::path::Path,
    ) -> anyhow::Result<()> {
        self.persistent.long_term_memory = LongTermMemory::load(ltm_path)?;
        self.persistent.history_log = HistoryEntry::load_all(history_path)?;
        let loaded = load_session_state(session_state_path)?;
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
    ///
    /// This is the "Layer 0" compression — applied on every `build_messages()`
    /// call with zero LLM cost. It scans the message list and truncates
    /// tool context messages (identified by the `[Tool call round ` marker)
    /// that are outside the recent preservation window.
    ///
    /// ## Design rationale
    ///
    /// In a coding agent, tool results (file contents, grep output, bash output)
    /// dominate the context — often 60-80% of total tokens. Most of these results
    /// become stale after a few turns: the LLM only needs the *most recent* tool
    /// outputs for decision-making. Older tool outputs are kept as truncated
    /// summaries that preserve the tool name and call structure (so the LLM knows
    /// *what* was done) without the full output (which is no longer needed).
    ///
    /// ## Arguments
    /// * `messages` — The full message list (system + conversation). Modified in-place.
    ///   The first message (system prompt) is always skipped.
    fn micro_compact(messages: &mut [ChatMessage]) {
        use crate::llm::ChatRole;

        // Need at least system + MICRO_COMPACT_PRESERVE_RECENT conversation messages
        // before there's anything to truncate.
        let total = messages.len();
        if total <= 1 + MICRO_COMPACT_PRESERVE_RECENT {
            return;
        }

        // The preservation window covers the last N conversation messages.
        // Everything before that is eligible for truncation.
        // Index 0 is the system prompt — always skip it.
        let cutoff = total - MICRO_COMPACT_PRESERVE_RECENT;

        for msg in &mut messages[1..cutoff] {
            // Only truncate assistant messages that contain tool context.
            // User messages are never truncated (preserve user intent).
            if msg.role != ChatRole::Assistant {
                continue;
            }
            if !msg.content.contains(TOOL_CONTEXT_MARKER) {
                continue;
            }

            // Already short enough — skip.
            if msg.content.len() <= MICRO_COMPACT_MAX_CHARS {
                continue;
            }

            // Truncate each tool call line individually, preserving the structure.
            // Each line starts with "[Tool call round N: tool_name(...) -> ...]"
            let truncated_lines: Vec<String> = msg.content
                .lines()
                .map(|line| {
                    if line.starts_with(TOOL_CONTEXT_MARKER) && line.len() > MICRO_COMPACT_MAX_CHARS {
                        // Keep the tool name and truncate the rest.
                        // Find the closing "]" of the tool call to preserve the structure.
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

        // Apply micro-compact: truncate old tool context messages to reduce
        // context size without any LLM cost. Only the most recent N messages
        // keep their full tool context; older ones are truncated to a short
        // summary that preserves the tool name and call structure.
        Self::micro_compact(&mut messages);

        // Filter out non-system messages with empty content.
        // Some LLM APIs (Venus/Claude) reject empty content with
        // "message has no content" error. This can happen when tool-only
        // turns store an empty assistant message.
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
        // Delegate to the typed internal method and convert to strings.
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
