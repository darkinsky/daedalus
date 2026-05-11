use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};

use crate::memory::persistence::MemoryPersistence;
use crate::memory::strip_directive_prefix;
use crate::memory::truncate_to_token_budget;

use super::config::{CheatsheetConfig, CuratorMode};
use super::entry::CheatsheetEntry;

/// Dynamic Cheatsheet — a persistent, evolving memory of problem-solving insights.
///
/// Implements the DC paper (arxiv:2504.07952) with two Curator modes:
///
/// - **FullRewrite** (default, paper-faithful): The Curator LLM outputs a complete
///   updated cheatsheet each time, handling compression, merging, and eviction
///   via LLM judgment. This preserves the paper's core design where the Curator
///   decides what to keep, compress, merge, or discard.
///
/// - **Incremental**: The Curator outputs `NEW:`/`UPDATE:`/`REINFORCE:` directives
///   and code handles merging. Lighter-weight but loses global reorganization.
///
/// ## Lifecycle
///
/// 1. **Inject**: Before each LLM call, the cheatsheet is rendered as
///    Markdown and injected into the system prompt with a Generator preamble
///    that instructs the LLM to actively consult it.
/// 2. **Reflect**: After the LLM responds, the Curator analyzes the interaction.
///    - FullRewrite: Curator outputs entire updated cheatsheet (with correctness verification).
///    - Incremental: Curator outputs structured directives.
/// 3. **Update**: The cheatsheet state is replaced (FullRewrite) or patched (Incremental).
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
    /// Raw cheatsheet text (used in FullRewrite mode).
    /// When present, this is the Curator's latest output and takes priority
    /// over `entries` for rendering.
    raw_text: Option<String>,
    /// Configuration parameters.
    config: CheatsheetConfig,
}

#[allow(dead_code)]
impl DynamicCheatsheet {
    /// Create a new empty cheatsheet with the given config.
    pub fn new(config: CheatsheetConfig) -> Self {
        Self {
            entries: Vec::new(),
            raw_text: None,
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
        self.entries.is_empty() && self.raw_text.is_none()
    }

    /// Get a reference to the configuration.
    pub fn config(&self) -> &CheatsheetConfig {
        &self.config
    }

    /// Get all entries as a slice.
    pub fn entries(&self) -> &[CheatsheetEntry] {
        &self.entries
    }

    /// Override the configuration.
    ///
    /// Used by the factory to apply YAML config after loading persisted
    /// state (which doesn't include config). This ensures user-configured
    /// values like `curator_mode` and `max_entries` take effect.
    pub fn set_config(&mut self, config: CheatsheetConfig) {
        self.config = config;
    }

    // ── Rendering ──

    /// Render the cheatsheet as Markdown for system prompt injection.
    ///
    /// In FullRewrite mode, returns the raw Curator output (already formatted).
    /// In Incremental mode, groups entries by category with usage counts.
    ///
    /// Respects the `max_token_budget` by truncating if necessary.
    /// Returns `None` if the cheatsheet is empty.
    pub fn to_markdown(&self) -> Option<String> {
        // FullRewrite mode: use the raw Curator output directly.
        if let Some(ref raw) = self.raw_text {
            if !raw.trim().is_empty() {
                return Some(truncate_to_token_budget(
                    raw.clone(),
                    self.config.max_token_budget,
                    "*(cheatsheet truncated for token budget)*",
                ));
            }
        }

        if self.entries.is_empty() {
            return None;
        }

        // Incremental mode: group entries by category, include usage counts.
        let mut grouped: BTreeMap<&str, Vec<&CheatsheetEntry>> = BTreeMap::new();
        for entry in &self.entries {
            grouped.entry(entry.category.as_str()).or_default().push(entry);
        }

        let mut sections = Vec::new();
        for (category, entries) in &grouped {
            let items: Vec<String> = entries
                .iter()
                .map(|e| e.to_markdown_item())
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

    /// Render the cheatsheet as text for the Curator prompt.
    ///
    /// In FullRewrite mode, returns the raw text (so the Curator can see
    /// its own previous output and build upon it).
    /// In Incremental mode, returns a numbered list for directive references.
    pub fn to_curator_text(&self) -> String {
        // FullRewrite mode: return raw text or structured fallback.
        if self.config.curator_mode == CuratorMode::FullRewrite {
            if let Some(ref raw) = self.raw_text {
                if !raw.trim().is_empty() {
                    return raw.clone();
                }
            }
            // Fall through to structured rendering if no raw text yet.
            if self.entries.is_empty() {
                return "(empty — no entries yet)".to_string();
            }
            // Render entries as markdown for the first FullRewrite cycle.
            return self.to_markdown().unwrap_or_else(|| "(empty)".to_string());
        }

        // Incremental mode: numbered list.
        self.to_numbered_text()
    }

    /// Render the cheatsheet as a numbered list for the incremental reflection prompt.
    ///
    /// Each entry is numbered so the LLM can reference entries by number
    /// when suggesting updates. Includes usage count.
    pub fn to_numbered_text(&self) -> String {
        if self.entries.is_empty() {
            return "(empty — no entries yet)".to_string();
        }
        self.entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                if e.reinforcement_count > 1 {
                    format!("{}. [{}] {} (count: {})", i + 1, e.category, e.content, e.reinforcement_count)
                } else {
                    format!("{}. [{}] {}", i + 1, e.category, e.content)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    // ── LLM reflection ──

    /// Perform a full reflection cycle: call the Curator LLM and apply the response.
    ///
    /// Dispatches to the appropriate mode (FullRewrite vs Incremental).
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

        match self.config.curator_mode {
            CuratorMode::FullRewrite => {
                self.reflect_full_rewrite(user_input, assistant_response, llm).await;
            }
            CuratorMode::Incremental => {
                self.reflect_incremental(user_input, assistant_response, llm).await;
            }
        }
    }

    /// FullRewrite reflection: Curator outputs complete updated cheatsheet.
    async fn reflect_full_rewrite(
        &mut self,
        user_input: &str,
        assistant_response: &str,
        llm: &dyn crate::llm::LlmApi,
    ) {
        let current_cheatsheet = self.to_curator_text();
        let user_prompt = super::prompts::curator_full_rewrite_prompt(
            user_input, assistant_response, &current_cheatsheet,
        );

        let messages = vec![
            crate::llm::ChatMessage::system(super::prompts::CURATOR_FULL_REWRITE_SYSTEM_PROMPT),
            crate::llm::ChatMessage::user(user_prompt),
        ];

        match llm.chat(&messages, None).await {
            Ok(response) => {
                let new_text = response.content.trim().to_string();
                if new_text.is_empty() {
                    tracing::debug!("Dynamic Cheatsheet Curator: empty response, keeping old cheatsheet");
                    return;
                }
                // Safe fallback: if the new cheatsheet is drastically shorter
                // than the old one (>80% reduction), it might be a truncation
                // error. Keep the old one in that case.
                let old_len = current_cheatsheet.len();
                if old_len > 200 && new_text.len() < old_len / 5 {
                    tracing::warn!(
                        old_len = old_len,
                        new_len = new_text.len(),
                        "Dynamic Cheatsheet Curator: new cheatsheet is suspiciously short, \
                         keeping old version (possible truncation)"
                    );
                    return;
                }
                self.raw_text = Some(new_text);
                tracing::info!(
                    "Dynamic Cheatsheet: full-rewrite reflection complete"
                );
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Dynamic Cheatsheet Curator LLM call failed, continuing without update"
                );
            }
        }
    }

    /// Incremental reflection: Curator outputs NEW/UPDATE/REINFORCE directives.
    async fn reflect_incremental(
        &mut self,
        user_input: &str,
        assistant_response: &str,
        llm: &dyn crate::llm::LlmApi,
    ) {
        let current_cheatsheet = self.to_numbered_text();
        let user_prompt = super::prompts::curator_incremental_prompt(
            user_input, assistant_response, &current_cheatsheet,
        );

        let messages = vec![
            crate::llm::ChatMessage::system(super::prompts::CURATOR_INCREMENTAL_SYSTEM_PROMPT),
            crate::llm::ChatMessage::user(user_prompt),
        ];

        match llm.chat(&messages, None).await {
            Ok(response) => {
                self.apply_incremental_response(&response.content);
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Dynamic Cheatsheet reflection LLM call failed, continuing without update"
                );
            }
        }
    }

    // ── Reflection response processing (Incremental mode) ──

    /// Apply a reflection response from the incremental Curator.
    ///
    /// Parses NEW/UPDATE/REINFORCE directives, applies them, and evicts
    /// low-value entries if over capacity.
    ///
    /// Returns `true` if any changes were made, `false` otherwise.
    pub fn apply_incremental_response(&mut self, response_text: &str) -> bool {
        let response_text = response_text.trim();

        if response_text.eq_ignore_ascii_case("NO_NEW_INSIGHTS") {
            tracing::debug!("Dynamic Cheatsheet reflection: no new insights");
            return false;
        }

        let (new_entries, updates, reinforcements) = Self::parse_incremental_response(response_text);

        if new_entries.is_empty() && updates.is_empty() && reinforcements.is_empty() {
            return false;
        }

        // Apply reinforcements first.
        for index in &reinforcements {
            if *index < self.entries.len() {
                self.entries[*index].reinforce();
                tracing::debug!(
                    entry_index = index,
                    "Dynamic Cheatsheet: reinforced existing entry"
                );
            }
        }

        // Apply updates to existing entries (also increments reinforcement_count).
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

    /// Legacy alias for backward compatibility.
    pub fn apply_reflection_response(&mut self, response_text: &str) -> bool {
        self.apply_incremental_response(response_text)
    }

    /// Parse the incremental Curator's response into new entries, updates, and reinforcements.
    ///
    /// Expected format:
    /// - `NEW: <category> | <content>`
    /// - `UPDATE: <number> | <refined_content>`
    /// - `REINFORCE: <number>`
    fn parse_incremental_response(response: &str) -> (Vec<CheatsheetEntry>, Vec<(usize, String)>, Vec<usize>) {
        let mut new_entries = Vec::new();
        let mut updates = Vec::new();
        let mut reinforcements = Vec::new();

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
            } else if let Some(rest) = strip_directive_prefix(line, "REINFORCE:") {
                if let Some(index) = Self::parse_reinforce_directive(rest) {
                    reinforcements.push(index);
                }
            }
        }

        (new_entries, updates, reinforcements)
    }

    /// Legacy alias for backward compatibility with tests.
    pub fn parse_reflection_response(response: &str) -> (Vec<CheatsheetEntry>, Vec<(usize, String)>) {
        let (new_entries, updates, _reinforcements) = Self::parse_incremental_response(response);
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

    /// Parse a `REINFORCE:` directive body into a 0-based index.
    ///
    /// Expected: `<1-based number>`
    fn parse_reinforce_directive(body: &str) -> Option<usize> {
        let num: usize = body.trim().parse().ok()?;
        if num < 1 {
            return None;
        }
        Some(num - 1)
    }

    // ── Merging and eviction (Incremental mode) ──

    /// Merge new entries into the cheatsheet.
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
        let _below_threshold = self.entries
            .iter()
            .take_while(|e| e.reinforcement_count < threshold)
            .count();

        let excess = self.entries.len() - self.config.max_entries;
        // Phase 1: Evict entries below the retention threshold first.
        // Phase 2: If below-threshold entries aren't enough, evict the
        // remaining excess from the lowest-reinforcement entries.
        let to_evict = excess; // Always evict exactly `excess` entries.
        // The sort order ensures below-threshold entries are evicted first,
        // since they appear at the front (lowest reinforcement_count).
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
        // Persist both structured entries and raw text.
        let data = CheatsheetPersistenceData {
            entries: self.entries.clone(),
            raw_text: self.raw_text.clone(),
        };
        let json = serde_json::to_string_pretty(&data)
            .context("Failed to serialize DynamicCheatsheet")?;
        crate::memory::persistence::atomic_write(path, json.as_bytes())
            .with_context(|| format!("Failed to write DynamicCheatsheet to: {}", path.display()))?;
        tracing::debug!(
            path = %path.display(),
            entries = self.entries.len(),
            has_raw = self.raw_text.is_some(),
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

        // Try the new format first (with raw_text).
        if let Ok(data) = serde_json::from_str::<CheatsheetPersistenceData>(&content) {
            tracing::info!(
                path = %path.display(),
                entries = data.entries.len(),
                has_raw = data.raw_text.is_some(),
                "DynamicCheatsheet loaded from disk (v2 format)"
            );
            return Ok(Self {
                entries: data.entries,
                raw_text: data.raw_text,
                config: CheatsheetConfig::default(),
            });
        }

        // Fall back to legacy format (just Vec<CheatsheetEntry>).
        let entries: Vec<CheatsheetEntry> = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse DynamicCheatsheet from: {}", path.display()))?;
        tracing::info!(
            path = %path.display(),
            entries = entries.len(),
            "DynamicCheatsheet loaded from disk (legacy format)"
        );
        Ok(Self {
            entries,
            raw_text: None,
            config: CheatsheetConfig::default(),
        })
    }
}

/// Persistence format for DynamicCheatsheet (v2).
#[derive(serde::Serialize, serde::Deserialize)]
struct CheatsheetPersistenceData {
    entries: Vec<CheatsheetEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_text: Option<String>,
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
        let config = CheatsheetConfig {
            curator_mode: CuratorMode::Incremental,
            ..Default::default()
        };
        let mut cs = DynamicCheatsheet::new(config);
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
    fn test_to_markdown_with_usage_count() {
        let config = CheatsheetConfig {
            curator_mode: CuratorMode::Incremental,
            ..Default::default()
        };
        let mut cs = DynamicCheatsheet::new(config);
        let mut entry = CheatsheetEntry::new(
            "strategy".to_string(),
            "Use memoization".to_string(),
        );
        entry.reinforce();
        entry.reinforce();
        cs.entries.push(entry);

        let md = cs.to_markdown().unwrap();
        assert!(md.contains("used 3×"));
    }

    #[test]
    fn test_to_markdown_raw_text_mode() {
        let mut cs = DynamicCheatsheet::with_defaults();
        cs.raw_text = Some("## Custom Cheatsheet\n\n- Strategy A\n- Strategy B".to_string());

        let md = cs.to_markdown().unwrap();
        assert!(md.contains("Custom Cheatsheet"));
        assert!(md.contains("Strategy A"));
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
    fn test_to_numbered_text_with_count() {
        let mut cs = DynamicCheatsheet::with_defaults();
        let mut entry = CheatsheetEntry::new(
            "strategy".to_string(),
            "Insight one".to_string(),
        );
        entry.reinforce();
        cs.entries.push(entry);

        let text = cs.to_numbered_text();
        assert!(text.contains("(count: 2)"));
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
    fn test_parse_reinforce_directive() {
        let response = "REINFORCE: 3";
        let (_new, _updates, reinforcements) = DynamicCheatsheet::parse_incremental_response(response);
        assert_eq!(reinforcements.len(), 1);
        assert_eq!(reinforcements[0], 2); // 1-based → 0-based
    }

    #[test]
    fn test_apply_incremental_response_reinforce() {
        let config = CheatsheetConfig {
            curator_mode: CuratorMode::Incremental,
            ..Default::default()
        };
        let mut cs = DynamicCheatsheet::new(config);
        cs.entries.push(CheatsheetEntry::new(
            "strategy".to_string(),
            "Some insight".to_string(),
        ));
        assert_eq!(cs.entries[0].reinforcement_count, 1);

        let changed = cs.apply_incremental_response("REINFORCE: 1");
        assert!(changed);
        assert_eq!(cs.entries[0].reinforcement_count, 2);
    }

    #[test]
    fn test_apply_incremental_response_update_increments_count() {
        let config = CheatsheetConfig {
            curator_mode: CuratorMode::Incremental,
            ..Default::default()
        };
        let mut cs = DynamicCheatsheet::new(config);
        cs.entries.push(CheatsheetEntry::new(
            "strategy".to_string(),
            "Old insight".to_string(),
        ));
        assert_eq!(cs.entries[0].reinforcement_count, 1);

        let changed = cs.apply_incremental_response("UPDATE: 1 | Refined insight");
        assert!(changed);
        assert_eq!(cs.entries[0].content, "Refined insight");
        assert_eq!(cs.entries[0].reinforcement_count, 2);
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
            curator_mode: CuratorMode::Incremental,
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
            curator_mode: CuratorMode::Incremental,
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
        assert!(cs.entries.is_empty());
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
            curator_mode: CuratorMode::Incremental,
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
            curator_mode: CuratorMode::Incremental,
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

    #[test]
    fn test_is_empty_with_raw_text() {
        let mut cs = DynamicCheatsheet::with_defaults();
        assert!(cs.is_empty());
        cs.raw_text = Some("Some content".to_string());
        assert!(!cs.is_empty());
    }
}
