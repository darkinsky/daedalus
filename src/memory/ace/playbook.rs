use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::memory::persistence::MemoryPersistence;
use crate::memory::truncate_to_token_budget;

// ── Bullet — atomic knowledge unit ──

/// A single bullet point in a playbook section.
///
/// Each bullet captures a specific insight, strategy, error pattern,
/// or best practice learned from past interactions. Bullets are
/// accumulated over time within their parent section and injected
/// into the system prompt to help the LLM avoid repeating mistakes
/// and reuse proven strategies.
///
/// Inspired by the ACE paper (arxiv:2510.04618).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bullet {
    /// Unique identifier for this bullet.
    pub id: Uuid,
    /// The insight content — concise, actionable.
    pub content: String,
    /// How many times this bullet has been reinforced (used/validated).
    pub reinforcement_count: u32,
    /// Which conversation turn produced this bullet.
    pub source_turn: usize,
    /// When this bullet was first created.
    pub created_at: DateTime<Local>,
    /// When this bullet was last updated or reinforced.
    pub updated_at: DateTime<Local>,
}

impl Bullet {
    /// Create a new bullet with the given content.
    pub fn new(content: String, source_turn: usize) -> Self {
        let now = Local::now();
        Self {
            id: Uuid::new_v4(),
            content,
            reinforcement_count: 1,
            source_turn,
            created_at: now,
            updated_at: now,
        }
    }

    /// Reinforce this bullet (increment count and update timestamp).
    #[allow(dead_code)]
    pub fn reinforce(&mut self) {
        self.reinforcement_count += 1;
        self.updated_at = Local::now();
    }

    /// Update the content of this bullet and refresh the timestamp.
    pub fn update_content(&mut self, new_content: String) {
        self.content = new_content;
        self.updated_at = Local::now();
    }
}

// ── Section — semantic grouping of bullets ──

/// A named section in the playbook, grouping related bullets.
///
/// Sections provide hierarchical organization that prevents context
/// collapse — the Curator merges delta entries into specific sections
/// rather than rewriting the entire playbook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section {
    /// Section title (e.g., "Error Handling Strategies").
    pub title: String,
    /// Ordered list of bullets in this section.
    pub bullets: Vec<Bullet>,
}

impl Section {
    /// Create a new empty section with the given title.
    pub fn new(title: String) -> Self {
        Self {
            title,
            bullets: Vec::new(),
        }
    }

    /// Find a bullet by its UUID.
    #[allow(dead_code)]
    pub fn find_bullet(&self, id: &Uuid) -> Option<&Bullet> {
        self.bullets.iter().find(|b| b.id == *id)
    }

    /// Find a bullet by its UUID (mutable).
    #[allow(dead_code)]
    pub fn find_bullet_mut(&mut self, id: &Uuid) -> Option<&mut Bullet> {
        self.bullets.iter_mut().find(|b| b.id == *id)
    }

    /// Find a bullet by 1-based index within this section.
    #[allow(dead_code)]
    pub fn bullet_by_index(&self, one_based: usize) -> Option<&Bullet> {
        if one_based == 0 || one_based > self.bullets.len() {
            return None;
        }
        Some(&self.bullets[one_based - 1])
    }

    /// Find a bullet by 1-based index (mutable).
    pub fn bullet_by_index_mut(&mut self, one_based: usize) -> Option<&mut Bullet> {
        if one_based == 0 || one_based > self.bullets.len() {
            return None;
        }
        Some(&mut self.bullets[one_based - 1])
    }

    /// Check if any bullet has content that exactly matches the given text.
    #[allow(dead_code)]
    pub fn has_exact_content(&self, content: &str) -> Option<&Bullet> {
        self.bullets.iter().find(|b| b.content == content)
    }

    /// Reinforce a bullet by exact content match.
    ///
    /// If a bullet with the given content exists, reinforce it and return `true`.
    /// Otherwise return `false`. This avoids the two-step lookup pattern of
    /// `has_exact_content()` → `find_bullet_mut()` that required borrowing `self`
    /// twice.
    pub fn reinforce_by_content(&mut self, content: &str) -> bool {
        if let Some(bullet) = self.bullets.iter_mut().find(|b| b.content == content) {
            bullet.reinforce();
            true
        } else {
            false
        }
    }
}

// ── DeltaEntry — incremental update instruction ──

/// A delta entry produced by the Reflector, consumed by the Curator.
///
/// This is the key anti-collapse mechanism: the LLM only produces
/// small delta items, never rewrites the entire playbook. The Curator
/// applies these deltas using deterministic logic (no LLM calls).
#[derive(Debug, Clone)]
pub enum DeltaEntry {
    /// Add a new bullet to a section (create section if needed).
    Add {
        section: String,
        content: String,
    },
    /// Update an existing bullet's content (by 1-based index within section).
    Update {
        section: String,
        bullet_index: usize,
        new_content: String,
    },
    /// Reinforce an existing bullet (by 1-based index within section).
    Reinforce {
        section: String,
        bullet_index: usize,
    },
    /// Remove a bullet that is outdated or incorrect (by 1-based index).
    Remove {
        section: String,
        bullet_index: usize,
    },
    /// No changes needed.
    #[allow(dead_code)]
    NoOp,
}

// ── Playbook — top-level container ──

/// The evolving playbook — a structured collection of strategies and insights.
///
/// The playbook is the core data structure of the ACE memory strategy.
/// It organizes knowledge into named sections, each containing ordered
/// bullets. The playbook is rendered as Markdown and injected into the
/// system prompt before each LLM call.
///
/// Key design principles (from the ACE paper):
/// - **Incremental updates**: Only delta entries are applied, never full rewrites.
/// - **Structured sections**: Knowledge is organized by topic, preventing collapse.
/// - **Deterministic merging**: The Curator applies deltas without LLM calls.
///
/// Reference: ACE (arxiv:2510.04618), kayba-ai/agentic-context-engine
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Playbook {
    /// Ordered list of sections.
    pub sections: Vec<Section>,
    /// Total conversation turns processed.
    pub turn_count: usize,
}

impl Playbook {
    /// Create a new empty playbook.
    pub fn new() -> Self {
        Self {
            sections: Vec::new(),
            turn_count: 0,
        }
    }

    /// Return the total number of bullets across all sections.
    pub fn total_bullets(&self) -> usize {
        self.sections.iter().map(|s| s.bullets.len()).sum()
    }

    /// Check if the playbook is empty (no sections or all sections empty).
    pub fn is_empty(&self) -> bool {
        self.sections.is_empty() || self.sections.iter().all(|s| s.bullets.is_empty())
    }

    /// Find a section by title (case-insensitive).
    #[allow(dead_code)]
    pub fn find_section(&self, title: &str) -> Option<&Section> {
        let lower = title.to_lowercase();
        self.sections.iter().find(|s| s.title.to_lowercase() == lower)
    }

    /// Find a section by title (mutable, case-insensitive).
    pub fn find_section_mut(&mut self, title: &str) -> Option<&mut Section> {
        let lower = title.to_lowercase();
        self.sections.iter_mut().find(|s| s.title.to_lowercase() == lower)
    }

    /// Get or create a section by title.
    ///
    /// If a section with the given title exists (case-insensitive match),
    /// returns a mutable reference to it. Otherwise, creates a new section
    /// and returns a mutable reference.
    pub fn get_or_create_section(&mut self, title: &str) -> &mut Section {
        let lower = title.to_lowercase();
        let idx = self.sections.iter().position(|s| s.title.to_lowercase() == lower);
        match idx {
            Some(i) => &mut self.sections[i],
            None => {
                self.sections.push(Section::new(title.to_string()));
                self.sections.last_mut().unwrap()
            }
        }
    }

    // ── Rendering ──

    /// Render the playbook as Markdown for system prompt injection.
    ///
    /// Groups bullets by section and formats them as a structured reference.
    /// Respects the `max_token_budget` by truncating if necessary.
    /// Returns `None` if the playbook is empty.
    pub fn to_markdown(&self, max_token_budget: usize) -> Option<String> {
        if self.is_empty() {
            return None;
        }

        let mut section_texts = Vec::new();
        for section in &self.sections {
            if section.bullets.is_empty() {
                continue;
            }
            let items: Vec<String> = section.bullets
                .iter()
                .enumerate()
                .map(|(i, b)| {
                    format!("{}. {} (×{})", i + 1, b.content, b.reinforcement_count)
                })
                .collect();
            section_texts.push(format!("### {}\n{}", section.title, items.join("\n")));
        }

        let body = section_texts.join("\n\n");
        let full = format!("## Playbook — Accumulated Strategies & Insights\n\n{}", body);

        Some(truncate_to_token_budget(
            full,
            max_token_budget,
            "*(playbook truncated for token budget)*",
        ))
    }

    /// Render the playbook as numbered text for the reflection prompt.
    ///
    /// Each section and bullet is numbered so the LLM can reference
    /// entries by section title and bullet number when suggesting updates.
    pub fn to_numbered_text(&self) -> String {
        if self.is_empty() {
            return "(empty — no entries yet)".to_string();
        }

        let mut parts = Vec::new();
        for section in &self.sections {
            if section.bullets.is_empty() {
                continue;
            }
            let mut section_text = format!("[{}]", section.title);
            for (i, bullet) in section.bullets.iter().enumerate() {
                section_text.push_str(&format!(
                    "\n  {}. {} (×{})",
                    i + 1,
                    bullet.content,
                    bullet.reinforcement_count,
                ));
            }
            parts.push(section_text);
        }
        parts.join("\n")
    }

    /// Increment the turn counter.
    pub fn advance_turn(&mut self) {
        self.turn_count += 1;
    }
}

impl Default for Playbook {
    fn default() -> Self {
        Self::new()
    }
}

// ── Persistence ──

impl MemoryPersistence for Playbook {
    fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)
            .context("Failed to serialize Playbook")?;
        crate::memory::persistence::atomic_write(path, json.as_bytes())
            .with_context(|| format!("Failed to write Playbook to: {}", path.display()))?;
        tracing::debug!(
            path = %path.display(),
            sections = self.sections.len(),
            bullets = self.total_bullets(),
            "Playbook saved (atomic)"
        );
        Ok(())
    }

    fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            tracing::debug!(path = %path.display(), "No Playbook file found, using default");
            return Ok(Self::new());
        }
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read Playbook from: {}", path.display()))?;
        let playbook: Playbook = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse Playbook from: {}", path.display()))?;
        tracing::info!(
            path = %path.display(),
            sections = playbook.sections.len(),
            bullets = playbook.total_bullets(),
            "Playbook loaded from disk"
        );
        Ok(playbook)
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_playbook() {
        let pb = Playbook::new();
        assert!(pb.is_empty());
        assert_eq!(pb.total_bullets(), 0);
        assert_eq!(pb.turn_count, 0);
    }

    #[test]
    fn test_bullet_creation() {
        let bullet = Bullet::new("Use binary search".to_string(), 1);
        assert_eq!(bullet.content, "Use binary search");
        assert_eq!(bullet.reinforcement_count, 1);
        assert_eq!(bullet.source_turn, 1);
    }

    #[test]
    fn test_bullet_reinforce() {
        let mut bullet = Bullet::new("Test insight".to_string(), 1);
        let original_count = bullet.reinforcement_count;
        bullet.reinforce();
        assert_eq!(bullet.reinforcement_count, original_count + 1);
    }

    #[test]
    fn test_bullet_update_content() {
        let mut bullet = Bullet::new("Old content".to_string(), 1);
        bullet.update_content("New refined content".to_string());
        assert_eq!(bullet.content, "New refined content");
    }

    #[test]
    fn test_section_find_bullet_by_index() {
        let mut section = Section::new("Test".to_string());
        section.bullets.push(Bullet::new("First".to_string(), 1));
        section.bullets.push(Bullet::new("Second".to_string(), 2));

        assert!(section.bullet_by_index(0).is_none()); // 0 is invalid (1-based)
        assert_eq!(section.bullet_by_index(1).unwrap().content, "First");
        assert_eq!(section.bullet_by_index(2).unwrap().content, "Second");
        assert!(section.bullet_by_index(3).is_none());
    }

    #[test]
    fn test_section_has_exact_content() {
        let mut section = Section::new("Test".to_string());
        section.bullets.push(Bullet::new("Exact match".to_string(), 1));

        assert!(section.has_exact_content("Exact match").is_some());
        assert!(section.has_exact_content("No match").is_none());
    }

    #[test]
    fn test_playbook_get_or_create_section() {
        let mut pb = Playbook::new();

        // Create new section
        let section = pb.get_or_create_section("Error Handling");
        section.bullets.push(Bullet::new("Check errors".to_string(), 1));

        assert_eq!(pb.sections.len(), 1);

        // Get existing section (case-insensitive)
        let section = pb.get_or_create_section("error handling");
        assert_eq!(section.bullets.len(), 1);
        assert_eq!(pb.sections.len(), 1); // No new section created
    }

    #[test]
    fn test_playbook_to_markdown_empty() {
        let pb = Playbook::new();
        assert!(pb.to_markdown(4000).is_none());
    }

    #[test]
    fn test_playbook_to_markdown_with_entries() {
        let mut pb = Playbook::new();
        let section = pb.get_or_create_section("Strategies");
        section.bullets.push(Bullet::new("Use binary search".to_string(), 1));
        section.bullets.push(Bullet::new("Cache results".to_string(), 2));

        let md = pb.to_markdown(4000).unwrap();
        assert!(md.contains("Playbook"));
        assert!(md.contains("Strategies"));
        assert!(md.contains("binary search"));
        assert!(md.contains("Cache results"));
        assert!(md.contains("×1")); // reinforcement count
    }

    #[test]
    fn test_playbook_to_markdown_truncation() {
        let mut pb = Playbook::new();
        let section = pb.get_or_create_section("Test");
        section.bullets.push(Bullet::new(
            "A very long insight that should exceed the tiny token budget".to_string(),
            1,
        ));

        let md = pb.to_markdown(5).unwrap(); // Very small budget (~20 chars)
        assert!(md.contains("truncated"));
    }

    #[test]
    fn test_playbook_to_numbered_text() {
        let mut pb = Playbook::new();
        let section = pb.get_or_create_section("Error Handling");
        section.bullets.push(Bullet::new("Check return values".to_string(), 1));
        section.bullets.push(Bullet::new("Log context".to_string(), 2));

        let text = pb.to_numbered_text();
        assert!(text.contains("[Error Handling]"));
        assert!(text.contains("1. Check return values"));
        assert!(text.contains("2. Log context"));
    }

    #[test]
    fn test_playbook_to_numbered_text_empty() {
        let pb = Playbook::new();
        assert_eq!(pb.to_numbered_text(), "(empty — no entries yet)");
    }

    #[test]
    fn test_playbook_advance_turn() {
        let mut pb = Playbook::new();
        assert_eq!(pb.turn_count, 0);
        pb.advance_turn();
        assert_eq!(pb.turn_count, 1);
        pb.advance_turn();
        assert_eq!(pb.turn_count, 2);
    }
}
