use super::config::AceConfig;
use super::playbook::{Bullet, DeltaEntry, Playbook};

/// Deterministic merge engine for applying delta entries to a Playbook.
///
/// This is the key anti-collapse mechanism in ACE: the Curator applies
/// delta entries using **deterministic logic** (no LLM calls), preventing
/// the brevity bias and context collapse that occur when LLMs rewrite
/// entire contexts.
///
/// The Curator handles:
/// 1. Section creation (if a delta targets a non-existent section)
/// 2. Bullet insertion with dedup check (exact content match → reinforce)
/// 3. Bullet update/reinforce/remove by 1-based index
/// 4. Section-level capacity eviction (per `max_bullets_per_section`)
/// 5. Global section count eviction (per `max_sections`)
///
/// Reference: ACE (arxiv:2510.04618), kayba-ai/agentic-context-engine
pub struct Curator;

impl Curator {
    /// Apply a batch of delta entries to the playbook.
    ///
    /// This is a **deterministic** operation — no LLM calls.
    /// Deltas are applied sequentially in the order provided.
    /// After all deltas are applied, eviction is performed if needed.
    pub fn apply_deltas(playbook: &mut Playbook, deltas: Vec<DeltaEntry>, config: &AceConfig) {
        for delta in deltas {
            match delta {
                DeltaEntry::Add { section, content } => {
                    let current_turn = playbook.turn_count;
                    Self::add_bullet(playbook, &section, content, current_turn);
                }
                DeltaEntry::Update { section, bullet_index, new_content } => {
                    Self::with_bullet_mut(playbook, &section, bullet_index, "UPDATE", |bullet| {
                        bullet.update_content(new_content);
                    });
                }
                DeltaEntry::Reinforce { section, bullet_index } => {
                    Self::with_bullet_mut(playbook, &section, bullet_index, "REINFORCE", |bullet| {
                        bullet.reinforce();
                    });
                }
                DeltaEntry::Remove { section, bullet_index } => {
                    Self::remove_bullet(playbook, &section, bullet_index);
                }
                DeltaEntry::NoOp => {}
            }
        }

        // Evict if over capacity.
        Self::evict_if_needed(playbook, config);
    }

    /// Add a bullet to a section, creating the section if needed.
    ///
    /// Performs exact content dedup: if a bullet with the same content
    /// already exists in the section, reinforce it instead of adding
    /// a duplicate.
    fn add_bullet(playbook: &mut Playbook, section_title: &str, content: String, turn: usize) {
        let section = playbook.get_or_create_section(section_title);

        // Dedup: if exact content already exists, reinforce instead.
        if section.reinforce_by_content(&content) {
            tracing::debug!(
                section = section_title,
                "ACE Curator: reinforced existing bullet (exact dedup)"
            );
            return;
        }

        section.bullets.push(Bullet::new(content, turn));
        tracing::debug!(
            section = section_title,
            "ACE Curator: added new bullet"
        );
    }

    /// Locate a bullet by section title and 1-based index, then apply an action.
    ///
    /// This helper eliminates the repetitive `find_section_mut` → `bullet_by_index_mut`
    /// → warn pattern shared by UPDATE, REINFORCE, and similar operations.
    fn with_bullet_mut(
        playbook: &mut Playbook,
        section_title: &str,
        bullet_index: usize,
        op_name: &str,
        action: impl FnOnce(&mut Bullet),
    ) {
        if let Some(section) = playbook.find_section_mut(section_title) {
            if let Some(bullet) = section.bullet_by_index_mut(bullet_index) {
                action(bullet);
                tracing::debug!(
                    section = section_title,
                    bullet_index,
                    op = op_name,
                    "ACE Curator: applied delta"
                );
            } else {
                tracing::warn!(
                    section = section_title,
                    bullet_index,
                    op = op_name,
                    "ACE Curator: bullet index out of range"
                );
            }
        } else {
            tracing::warn!(
                section = section_title,
                op = op_name,
                "ACE Curator: section not found"
            );
        }
    }

    /// Remove a bullet by 1-based index within a section.
    ///
    /// Removal requires a separate method because it operates on the section's
    /// bullet list directly (via `Vec::remove`) rather than on a `&mut Bullet`.
    /// Empty sections are cleaned up during eviction.
    fn remove_bullet(playbook: &mut Playbook, section_title: &str, bullet_index: usize) {
        if let Some(section) = playbook.find_section_mut(section_title) {
            if bullet_index >= 1 && bullet_index <= section.bullets.len() {
                section.bullets.remove(bullet_index - 1);
                tracing::debug!(
                    section = section_title,
                    bullet_index,
                    "ACE Curator: removed bullet"
                );
            } else {
                tracing::warn!(
                    section = section_title,
                    bullet_index,
                    "ACE Curator: bullet index out of range for REMOVE"
                );
            }
        } else {
            tracing::warn!(
                section = section_title,
                "ACE Curator: section not found for REMOVE"
            );
        }
    }

    /// Evict low-value bullets and sections when over capacity.
    ///
    /// Two-phase eviction:
    /// 1. **Per-section**: For each section exceeding `max_bullets_per_section`,
    ///    remove lowest-reinforcement bullets (preferring those below retention threshold).
    /// 2. **Global**: If section count exceeds `max_sections`, remove sections
    ///    with the fewest total reinforcements (after removing empty sections).
    fn evict_if_needed(playbook: &mut Playbook, config: &AceConfig) {
        // Phase 0: Remove empty sections.
        playbook.sections.retain(|s| !s.bullets.is_empty());

        // Phase 1: Per-section bullet eviction.
        for section in &mut playbook.sections {
            if section.bullets.len() <= config.max_bullets_per_section {
                continue;
            }

            // Sort by (reinforcement_count ASC, updated_at ASC) so lowest-value
            // bullets are at the front.
            section.bullets.sort_by(|a, b| {
                a.reinforcement_count.cmp(&b.reinforcement_count)
                    .then_with(|| a.updated_at.cmp(&b.updated_at))
            });

            let excess = section.bullets.len() - config.max_bullets_per_section;

            // Prefer evicting bullets below the retention threshold.
            let below_threshold = section.bullets
                .iter()
                .take_while(|b| b.reinforcement_count < config.min_reinforcement_for_retention)
                .count();

            let to_evict = excess.max(below_threshold.min(excess));
            section.bullets.drain(..to_evict);

            tracing::debug!(
                section = section.title.as_str(),
                evicted = to_evict,
                remaining = section.bullets.len(),
                "ACE Curator: evicted low-value bullets from section"
            );
        }

        // Phase 2: Global section eviction.
        if playbook.sections.len() <= config.max_sections {
            return;
        }

        // Sort sections by total reinforcement (ascending) — least-used first.
        playbook.sections.sort_by(|a, b| {
            let a_total: u32 = a.bullets.iter().map(|bullet| bullet.reinforcement_count).sum();
            let b_total: u32 = b.bullets.iter().map(|bullet| bullet.reinforcement_count).sum();
            a_total.cmp(&b_total)
        });

        let excess = playbook.sections.len() - config.max_sections;
        let removed: Vec<String> = playbook.sections.drain(..excess)
            .map(|s| s.title)
            .collect();

        tracing::debug!(
            removed = ?removed,
            remaining = playbook.sections.len(),
            "ACE Curator: evicted low-value sections"
        );
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::playbook::Section;

    fn default_config() -> AceConfig {
        AceConfig::default()
    }

    fn small_config() -> AceConfig {
        AceConfig {
            max_sections: 2,
            max_bullets_per_section: 3,
            min_reinforcement_for_retention: 2,
            ..Default::default()
        }
    }

    #[test]
    fn test_add_bullet_creates_section() {
        let mut pb = Playbook::new();
        let deltas = vec![DeltaEntry::Add {
            section: "Strategies".to_string(),
            content: "Use binary search".to_string(),
        }];

        Curator::apply_deltas(&mut pb, deltas, &default_config());

        assert_eq!(pb.sections.len(), 1);
        assert_eq!(pb.sections[0].title, "Strategies");
        assert_eq!(pb.sections[0].bullets.len(), 1);
        assert_eq!(pb.sections[0].bullets[0].content, "Use binary search");
    }

    #[test]
    fn test_add_bullet_dedup_reinforces() {
        let mut pb = Playbook::new();
        let deltas = vec![
            DeltaEntry::Add {
                section: "Strategies".to_string(),
                content: "Use binary search".to_string(),
            },
            DeltaEntry::Add {
                section: "Strategies".to_string(),
                content: "Use binary search".to_string(), // Exact duplicate
            },
        ];

        Curator::apply_deltas(&mut pb, deltas, &default_config());

        assert_eq!(pb.sections[0].bullets.len(), 1); // No duplicate
        assert_eq!(pb.sections[0].bullets[0].reinforcement_count, 2); // Reinforced
    }

    #[test]
    fn test_update_bullet() {
        let mut pb = Playbook::new();
        let section = pb.get_or_create_section("Test");
        section.bullets.push(Bullet::new("Old content".to_string(), 1));

        let deltas = vec![DeltaEntry::Update {
            section: "Test".to_string(),
            bullet_index: 1,
            new_content: "Refined content".to_string(),
        }];

        Curator::apply_deltas(&mut pb, deltas, &default_config());
        assert_eq!(pb.sections[0].bullets[0].content, "Refined content");
    }

    #[test]
    fn test_reinforce_bullet() {
        let mut pb = Playbook::new();
        let section = pb.get_or_create_section("Test");
        section.bullets.push(Bullet::new("Insight".to_string(), 1));

        let deltas = vec![DeltaEntry::Reinforce {
            section: "Test".to_string(),
            bullet_index: 1,
        }];

        Curator::apply_deltas(&mut pb, deltas, &default_config());
        assert_eq!(pb.sections[0].bullets[0].reinforcement_count, 2);
    }

    #[test]
    fn test_remove_bullet() {
        let mut pb = Playbook::new();
        let section = pb.get_or_create_section("Test");
        section.bullets.push(Bullet::new("To remove".to_string(), 1));
        section.bullets.push(Bullet::new("To keep".to_string(), 2));

        let deltas = vec![DeltaEntry::Remove {
            section: "Test".to_string(),
            bullet_index: 1,
        }];

        Curator::apply_deltas(&mut pb, deltas, &default_config());
        assert_eq!(pb.sections[0].bullets.len(), 1);
        assert_eq!(pb.sections[0].bullets[0].content, "To keep");
    }

    #[test]
    fn test_noop_delta() {
        let mut pb = Playbook::new();
        let deltas = vec![DeltaEntry::NoOp];
        Curator::apply_deltas(&mut pb, deltas, &default_config());
        assert!(pb.is_empty());
    }

    #[test]
    fn test_section_bullet_eviction() {
        let config = small_config(); // max 3 bullets per section
        let mut pb = Playbook::new();
        let section = pb.get_or_create_section("Test");

        // Add 5 bullets with different reinforcement counts.
        for i in 0..5 {
            let mut bullet = Bullet::new(format!("Insight {}", i), i);
            bullet.reinforcement_count = i as u32;
            section.bullets.push(bullet);
        }

        Curator::apply_deltas(&mut pb, vec![], &config);

        assert_eq!(pb.sections[0].bullets.len(), 3);
        // Lowest-reinforcement bullets (0, 1) should be evicted.
        assert!(pb.sections[0].bullets.iter().all(|b| b.reinforcement_count >= 2));
    }

    #[test]
    fn test_global_section_eviction() {
        let config = small_config(); // max 2 sections
        let mut pb = Playbook::new();

        // Create 3 sections with different total reinforcements.
        for (i, name) in ["Low", "Medium", "High"].iter().enumerate() {
            let section = pb.get_or_create_section(name);
            let mut bullet = Bullet::new(format!("Insight in {}", name), 1);
            bullet.reinforcement_count = (i as u32 + 1) * 5;
            section.bullets.push(bullet);
        }

        Curator::apply_deltas(&mut pb, vec![], &config);

        assert_eq!(pb.sections.len(), 2);
        // "Low" section (total reinforcement = 5) should be evicted.
        let titles: Vec<&str> = pb.sections.iter().map(|s| s.title.as_str()).collect();
        assert!(!titles.contains(&"Low"));
    }

    #[test]
    fn test_empty_sections_removed() {
        let mut pb = Playbook::new();
        pb.sections.push(Section::new("Empty".to_string()));
        let section = pb.get_or_create_section("NonEmpty");
        section.bullets.push(Bullet::new("Has content".to_string(), 1));

        Curator::apply_deltas(&mut pb, vec![], &default_config());

        assert_eq!(pb.sections.len(), 1);
        assert_eq!(pb.sections[0].title, "NonEmpty");
    }

    #[test]
    fn test_update_nonexistent_section_warns() {
        let mut pb = Playbook::new();
        let deltas = vec![DeltaEntry::Update {
            section: "NonExistent".to_string(),
            bullet_index: 1,
            new_content: "Content".to_string(),
        }];

        // Should not panic, just warn.
        Curator::apply_deltas(&mut pb, deltas, &default_config());
        assert!(pb.is_empty());
    }

    #[test]
    fn test_update_out_of_range_warns() {
        let mut pb = Playbook::new();
        let section = pb.get_or_create_section("Test");
        section.bullets.push(Bullet::new("Only bullet".to_string(), 1));

        let deltas = vec![DeltaEntry::Update {
            section: "Test".to_string(),
            bullet_index: 99, // Out of range
            new_content: "Content".to_string(),
        }];

        Curator::apply_deltas(&mut pb, deltas, &default_config());
        assert_eq!(pb.sections[0].bullets[0].content, "Only bullet"); // Unchanged
    }

    #[test]
    fn test_mixed_deltas() {
        let mut pb = Playbook::new();
        let deltas = vec![
            DeltaEntry::Add {
                section: "Strategies".to_string(),
                content: "First insight".to_string(),
            },
            DeltaEntry::Add {
                section: "Strategies".to_string(),
                content: "Second insight".to_string(),
            },
            DeltaEntry::Add {
                section: "Errors".to_string(),
                content: "Common error".to_string(),
            },
            DeltaEntry::Reinforce {
                section: "Strategies".to_string(),
                bullet_index: 1,
            },
            DeltaEntry::Update {
                section: "Errors".to_string(),
                bullet_index: 1,
                new_content: "Refined error description".to_string(),
            },
        ];

        Curator::apply_deltas(&mut pb, deltas, &default_config());

        assert_eq!(pb.sections.len(), 2);
        assert_eq!(pb.find_section("Strategies").unwrap().bullets.len(), 2);
        assert_eq!(
            pb.find_section("Strategies").unwrap().bullets[0].reinforcement_count,
            2
        );
        assert_eq!(
            pb.find_section("Errors").unwrap().bullets[0].content,
            "Refined error description"
        );
    }
}
