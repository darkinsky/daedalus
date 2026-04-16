use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};

use crate::memory::persistence::MemoryPersistence;
use crate::memory::strip_directive_prefix;
use crate::memory::truncate_to_token_budget;

use super::config::CheatsheetConfig;
use super::entry::CheatsheetEntry;

/// Dynamic Cheatsheet — a persistent, evolving memory of problem-solving insights.
///
/// Inspired by the DC paper (arxiv:2504.07952), this module maintains a
/// structured collection of insights that the LLM accumulates across
/// interactions. After each conversation turn, the LLM reflects on its
/// response and extracts reusable strategies, error patterns, and code
/// snippets into the cheatsheet.
///
/// ## Lifecycle
///
/// 1. **Inject**: Before each LLM call, the cheatsheet is rendered as
///    Markdown and injected into the system prompt.
/// 2. **Reflect**: After the LLM responds, a reflection call extracts
///    new insights from the interaction.
/// 3. **Update**: New insights are merged into the cheatsheet — duplicates
///    are consolidated, outdated entries are refined, and reinforcement
///    counts are incremented.
///
/// ## Integration with SlidingWindowMemory
///
/// DC operates as a parallel memory layer. `SlidingWindowMemory` manages
/// conversation context (messages, windowing, consolidation), while DC
/// manages accumulated problem-solving knowledge. Both inject their
/// content into the system prompt via `effective_system_prompt()`.
pub struct DynamicCheatsheet {
    /// All cheatsheet entries, in insertion order.
    entries: Vec<CheatsheetEntry>,
    /// Configuration parameters.
    config: CheatsheetConfig,
}

#[allow(dead_code)]
impl DynamicCheatsheet {
    /// Create a new empty cheatsheet with the given config.
    pub fn new(config: CheatsheetConfig) -> Self {
        Self {
            entries: Vec::new(),
            config,
        }
    }

    /// Create a cheatsheet with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(CheatsheetConfig::default())
    }

    /// Return the number of entries in the cheatsheet.
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Check if the cheatsheet is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Get a reference to the configuration.
    pub fn config(&self) -> &CheatsheetConfig {
        &self.config
    }

    /// Get all entries as a slice.
    pub fn entries(&self) -> &[CheatsheetEntry] {
        &self.entries
    }

    // ── Rendering ──

    /// Render the cheatsheet as Markdown for system prompt injection.
    ///
    /// Groups entries by category and formats them as a structured reference.
    /// Respects the `max_token_budget` by truncating if necessary.
    /// Returns `None` if the cheatsheet is empty.
    pub fn to_markdown(&self) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }

        // Group entries by category, preserving order within each group.
        let mut grouped: BTreeMap<&str, Vec<&CheatsheetEntry>> = BTreeMap::new();
        for entry in &self.entries {
            grouped.entry(entry.category.as_str()).or_default().push(entry);
        }

        let mut sections = Vec::new();
        for (category, entries) in &grouped {
            let items: Vec<String> = entries
                .iter()
                .map(|e| format!("- {}", e.content))
                .collect();
            sections.push(format!("### {}\n{}", category, items.join("\n")));
        }

        let body = sections.join("\n\n");
        let full = format!("## Dynamic Cheatsheet\n\n{}", body);

        Some(truncate_to_token_budget(
            full,
            self.config.max_token_budget,
            "*(cheatsheet truncated for token budget)*",
        ))
    }

    /// Render the cheatsheet as a numbered list for the reflection prompt.
    ///
    /// Each entry is numbered so the LLM can reference entries by number
    /// when suggesting updates.
    pub fn to_numbered_text(&self) -> String {
        if self.entries.is_empty() {
            return "(empty — no entries yet)".to_string();
        }
        self.entries
            .iter()
            .enumerate()
            .map(|(i, e)| format!("{}. [{}] {}", i + 1, e.category, e.content))
            .collect::<Vec<_>>()
            .join("\n")
    }

    // ── LLM reflection ──

    /// Perform a full reflection cycle: call the LLM and apply the response.
    ///
    /// This is the shared entry point for all memory strategies that use
    /// Dynamic Cheatsheet reflection. It encapsulates the complete flow:
    /// 1. Check `auto_reflect` config
    /// 2. Build the reflection prompt (with current cheatsheet state)
    /// 3. Call the LLM
    /// 4. Parse and apply the response
    ///
    /// Reflection failures are logged but never propagated — they must not
    /// disrupt the main conversation flow.
    pub async fn reflect(
        &mut self,
        user_input: &str,
        assistant_response: &str,
        llm: &dyn crate::llm::LlmApi,
    ) {
        if !self.config.auto_reflect {
            return;
        }

        let current_cheatsheet = self.to_numbered_text();
        let user_prompt = super::prompts::reflection_user_prompt(
            user_input, assistant_response, &current_cheatsheet,
        );

        let messages = vec![
            crate::llm::ChatMessage::system(super::prompts::REFLECTION_SYSTEM_PROMPT),
            crate::llm::ChatMessage::user(user_prompt),
        ];

        match llm.chat(&messages, None).await {
            Ok(response) => {
                self.apply_reflection_response(&response.content);
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Dynamic Cheatsheet reflection LLM call failed, continuing without update"
                );
            }
        }
    }

    // ── Reflection response processing ──

    /// Apply a reflection response from the LLM.
    ///
    /// This is the data-only counterpart of [`reflect()`](Self::reflect).
    /// It parses the LLM's text response, applies updates to existing
    /// entries, merges new entries, and evicts low-value entries if over
    /// capacity.
    ///
    /// Call `reflect()` for the full cycle (including the LLM call), or
    /// call this method directly when you already have the response text
    /// (e.g., in tests).
    ///
    /// Returns `true` if any changes were made, `false` otherwise.
    pub fn apply_reflection_response(&mut self, response_text: &str) -> bool {
        let response_text = response_text.trim();

        if response_text.eq_ignore_ascii_case("NO_NEW_INSIGHTS") {
            tracing::debug!("Dynamic Cheatsheet reflection: no new insights");
            return false;
        }

        let (new_entries, updates) = Self::parse_reflection_response(response_text);

        if new_entries.is_empty() && updates.is_empty() {
            return false;
        }

        // Apply updates to existing entries.
        for (index, new_content) in &updates {
            if *index < self.entries.len() {
                self.entries[*index].update_content(new_content.clone());
                tracing::debug!(
                    entry_index = index,
                    "Dynamic Cheatsheet: updated existing entry"
                );
            }
        }

        // Merge new entries.
        if !new_entries.is_empty() {
            tracing::debug!(
                new_count = new_entries.len(),
                "Dynamic Cheatsheet: adding new entries"
            );
            self.merge_entries(new_entries);
        }

        // Evict if over capacity.
        self.evict_if_needed();

        tracing::info!(
            total_entries = self.entries.len(),
            "Dynamic Cheatsheet reflection complete"
        );

        true
    }

    /// Parse the LLM's reflection response into new entries and updates.
    ///
    /// Expected format:
    /// - `NEW: <category> | <content>`
    /// - `UPDATE: <number> | <refined_content>`
    fn parse_reflection_response(response: &str) -> (Vec<CheatsheetEntry>, Vec<(usize, String)>) {
        let mut new_entries = Vec::new();
        let mut updates = Vec::new();

        for line in response.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            if let Some(rest) = strip_directive_prefix(line, "NEW:") {
                if let Some(entry) = Self::parse_new_directive(rest) {
                    new_entries.push(entry);
                }
            } else if let Some(rest) = strip_directive_prefix(line, "UPDATE:") {
                if let Some(update) = Self::parse_update_directive(rest) {
                    updates.push(update);
                }
            }
        }

        (new_entries, updates)
    }

    /// Parse a `NEW:` directive body into a `CheatsheetEntry`.
    ///
    /// Expected: `<category> | <content>`
    fn parse_new_directive(body: &str) -> Option<CheatsheetEntry> {
        let (category, content) = body.split_once('|')?;
        let content = content.trim();
        if content.is_empty() {
            return None;
        }
        Some(CheatsheetEntry::new(
            category.trim().to_lowercase(),
            content.to_string(),
        ))
    }

    /// Parse an `UPDATE:` directive body into an (index, content) pair.
    ///
    /// Expected: `<1-based number> | <refined_content>`
    fn parse_update_directive(body: &str) -> Option<(usize, String)> {
        let (num_str, content) = body.split_once('|')?;
        let num: usize = num_str.trim().parse().ok()?;
        let content = content.trim();
        if content.is_empty() || num < 1 {
            return None;
        }
        // Convert 1-based index to 0-based.
        Some((num - 1, content.to_string()))
    }

    // ── Merging and eviction ──

    /// Merge new entries into the cheatsheet.
    ///
    /// Simple append strategy — deduplication is handled by the LLM
    /// during reflection (it sees the current cheatsheet and is instructed
    /// to only extract NEW insights).
    fn merge_entries(&mut self, new_entries: Vec<CheatsheetEntry>) {
        self.entries.extend(new_entries);
    }

    /// Evict low-value entries when the cheatsheet exceeds capacity.
    ///
    /// Eviction strategy (two-phase):
    /// 1. First, remove entries below `min_reinforcement_for_retention`
    ///    (sorted by reinforcement_count ASC, then updated_at ASC).
    /// 2. If still over capacity, remove the lowest-value entries
    ///    regardless of reinforcement count.
    fn evict_if_needed(&mut self) {
        if self.entries.len() <= self.config.max_entries {
            return;
        }

        // Sort by (reinforcement_count ASC, updated_at ASC) so lowest-value
        // entries are at the front.
        self.entries.sort_by(|a, b| {
            a.reinforcement_count.cmp(&b.reinforcement_count)
                .then_with(|| a.updated_at.cmp(&b.updated_at))
        });

        // Phase 1: Prefer evicting entries below the retention threshold.
        let threshold = self.config.min_reinforcement_for_retention;
        let below_threshold = self.entries
            .iter()
            .take_while(|e| e.reinforcement_count < threshold)
            .count();

        let excess = self.entries.len() - self.config.max_entries;
        // Evict at most `excess` entries, preferring those below threshold.
        let to_evict = excess.min(below_threshold).max(
            // Phase 2: If below-threshold entries aren't enough, evict more.
            excess,
        );
        self.entries.drain(..to_evict);

        tracing::debug!(
            evicted = to_evict,
            remaining = self.entries.len(),
            "Dynamic Cheatsheet: evicted low-value entries"
        );
    }
}

impl Default for DynamicCheatsheet {
    fn default() -> Self {
        Self::with_defaults()
    }
}

impl MemoryPersistence for DynamicCheatsheet {
    fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.entries)
            .context("Failed to serialize DynamicCheatsheet")?;
        crate::memory::persistence::atomic_write(path, json.as_bytes())
            .with_context(|| format!("Failed to write DynamicCheatsheet to: {}", path.display()))?;
        tracing::debug!(
            path = %path.display(),
            entries = self.entries.len(),
            "DynamicCheatsheet saved (atomic)"
        );
        Ok(())
    }

    fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            tracing::debug!(path = %path.display(), "No DynamicCheatsheet file found, using default");
            return Ok(Self::with_defaults());
        }
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read DynamicCheatsheet from: {}", path.display()))?;
        let entries: Vec<CheatsheetEntry> = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse DynamicCheatsheet from: {}", path.display()))?;
        tracing::info!(
            path = %path.display(),
            entries = entries.len(),
            "DynamicCheatsheet loaded from disk"
        );
        Ok(Self {
            entries,
            config: CheatsheetConfig::default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_cheatsheet() {
        let cs = DynamicCheatsheet::with_defaults();
        assert!(cs.is_empty());
        assert_eq!(cs.entry_count(), 0);
    }

    #[test]
    fn test_to_markdown_empty() {
        let cs = DynamicCheatsheet::with_defaults();
        assert!(cs.to_markdown().is_none());
    }

    #[test]
    fn test_to_markdown_with_entries() {
        let mut cs = DynamicCheatsheet::with_defaults();
        cs.entries.push(CheatsheetEntry::new(
            "strategy".to_string(),
            "Use binary search for sorted arrays".to_string(),
        ));
        cs.entries.push(CheatsheetEntry::new(
            "error_pattern".to_string(),
            "Off-by-one errors in loop bounds".to_string(),
        ));

        let md = cs.to_markdown().unwrap();
        assert!(md.contains("Dynamic Cheatsheet"));
        assert!(md.contains("strategy"));
        assert!(md.contains("error_pattern"));
        assert!(md.contains("binary search"));
        assert!(md.contains("Off-by-one"));
    }

    #[test]
    fn test_to_numbered_text() {
        let mut cs = DynamicCheatsheet::with_defaults();
        cs.entries.push(CheatsheetEntry::new(
            "strategy".to_string(),
            "Insight one".to_string(),
        ));
        cs.entries.push(CheatsheetEntry::new(
            "error_pattern".to_string(),
            "Insight two".to_string(),
        ));

        let text = cs.to_numbered_text();
        assert!(text.contains("1. [strategy] Insight one"));
        assert!(text.contains("2. [error_pattern] Insight two"));
    }

    #[test]
    fn test_parse_reflection_response_new() {
        let response = "NEW: strategy | Use memoization for recursive problems\nNEW: error_pattern | Check null before dereferencing";
        let (new_entries, updates) = DynamicCheatsheet::parse_reflection_response(response);
        assert_eq!(new_entries.len(), 2);
        assert_eq!(new_entries[0].category, "strategy");
        assert!(new_entries[0].content.contains("memoization"));
        assert_eq!(new_entries[1].category, "error_pattern");
        assert!(updates.is_empty());
    }

    #[test]
    fn test_parse_reflection_response_update() {
        let response = "UPDATE: 2 | Refined insight about error handling";
        let (new_entries, updates) = DynamicCheatsheet::parse_reflection_response(response);
        assert!(new_entries.is_empty());
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].0, 1); // 1-based → 0-based
        assert!(updates[0].1.contains("Refined insight"));
    }

    #[test]
    fn test_parse_reflection_response_no_insights() {
        let response = "NO_NEW_INSIGHTS";
        let (new_entries, updates) = DynamicCheatsheet::parse_reflection_response(response);
        assert!(new_entries.is_empty());
        assert!(updates.is_empty());
    }

    #[test]
    fn test_parse_reflection_response_mixed() {
        let response = "NEW: best_practice | Always validate input\nUPDATE: 1 | Updated strategy\nNEW: code_snippet | Use `?` operator for error propagation";
        let (new_entries, updates) = DynamicCheatsheet::parse_reflection_response(response);
        assert_eq!(new_entries.len(), 2);
        assert_eq!(updates.len(), 1);
    }

    #[test]
    fn test_evict_if_needed() {
        let config = CheatsheetConfig {
            max_entries: 3,
            ..Default::default()
        };
        let mut cs = DynamicCheatsheet::new(config);

        // Add 5 entries with different reinforcement counts.
        for i in 0..5 {
            let mut entry = CheatsheetEntry::new(
                "strategy".to_string(),
                format!("Insight {}", i),
            );
            entry.reinforcement_count = i as u32;
            cs.entries.push(entry);
        }

        cs.evict_if_needed();
        assert_eq!(cs.entries.len(), 3);
        // The 2 lowest-reinforcement entries (0, 1) should be evicted.
        assert!(cs.entries.iter().all(|e| e.reinforcement_count >= 2));
    }

    #[test]
    fn test_to_markdown_token_budget() {
        let config = CheatsheetConfig {
            max_token_budget: 10, // Very small budget (~40 chars)
            ..Default::default()
        };
        let mut cs = DynamicCheatsheet::new(config);
        cs.entries.push(CheatsheetEntry::new(
            "strategy".to_string(),
            "A very long insight that should exceed the tiny token budget we set for testing purposes".to_string(),
        ));

        let md = cs.to_markdown().unwrap();
        assert!(md.contains("truncated"));
    }

    // ── apply_reflection_response tests ──

    #[test]
    fn test_apply_reflection_response_new_entries() {
        let mut cs = DynamicCheatsheet::with_defaults();
        let response = "NEW: strategy | Use binary search\nNEW: error_pattern | Check bounds";
        let changed = cs.apply_reflection_response(response);
        assert!(changed);
        assert_eq!(cs.entry_count(), 2);
        assert_eq!(cs.entries()[0].category, "strategy");
        assert_eq!(cs.entries()[1].category, "error_pattern");
    }

    #[test]
    fn test_apply_reflection_response_update_existing() {
        let mut cs = DynamicCheatsheet::with_defaults();
        cs.entries.push(CheatsheetEntry::new(
            "strategy".to_string(),
            "Old insight".to_string(),
        ));
        let response = "UPDATE: 1 | Refined insight";
        let changed = cs.apply_reflection_response(response);
        assert!(changed);
        assert_eq!(cs.entries()[0].content, "Refined insight");
    }

    #[test]
    fn test_apply_reflection_response_no_insights() {
        let mut cs = DynamicCheatsheet::with_defaults();
        let changed = cs.apply_reflection_response("NO_NEW_INSIGHTS");
        assert!(!changed);
        assert!(cs.is_empty());
    }

    #[test]
    fn test_apply_reflection_response_empty_response() {
        let mut cs = DynamicCheatsheet::with_defaults();
        let changed = cs.apply_reflection_response("");
        assert!(!changed);
    }

    #[test]
    fn test_apply_reflection_response_triggers_eviction() {
        let config = CheatsheetConfig {
            max_entries: 2,
            ..Default::default()
        };
        let mut cs = DynamicCheatsheet::new(config);
        cs.entries.push(CheatsheetEntry::new(
            "strategy".to_string(),
            "Existing insight".to_string(),
        ));
        // Add 2 new entries, exceeding max_entries of 2
        let response = "NEW: strategy | Insight A\nNEW: strategy | Insight B";
        cs.apply_reflection_response(response);
        assert_eq!(cs.entry_count(), 2); // Evicted down to max_entries
    }

    #[test]
    fn test_evict_prefers_below_retention_threshold() {
        let config = CheatsheetConfig {
            max_entries: 2,
            min_reinforcement_for_retention: 3,
            ..Default::default()
        };
        let mut cs = DynamicCheatsheet::new(config);

        // Entry with reinforcement_count = 1 (below threshold of 3).
        let mut low = CheatsheetEntry::new("strategy".to_string(), "Low value".to_string());
        low.reinforcement_count = 1;
        cs.entries.push(low);

        // Entry with reinforcement_count = 5 (above threshold).
        let mut high = CheatsheetEntry::new("strategy".to_string(), "High value".to_string());
        high.reinforcement_count = 5;
        cs.entries.push(high);

        // Entry with reinforcement_count = 2 (below threshold).
        let mut mid = CheatsheetEntry::new("strategy".to_string(), "Mid value".to_string());
        mid.reinforcement_count = 2;
        cs.entries.push(mid);

        // 3 entries, max_entries = 2 → must evict 1.
        cs.evict_if_needed();
        assert_eq!(cs.entry_count(), 2);
        // The lowest-reinforcement entry ("Low value", count=1) should be evicted.
        assert!(cs.entries().iter().all(|e| e.reinforcement_count >= 2));
    }
}
