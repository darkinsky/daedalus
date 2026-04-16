/// Configuration for the ACE (Agentic Context Engineering) memory module.
///
/// Controls capacity limits, eviction behavior, and whether automatic
/// reflection is enabled after each conversation turn.
///
/// Can be deserialized from YAML configuration files.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct AceConfig {
    /// Maximum number of sections in the playbook.
    /// When exceeded, least-used sections are evicted.
    pub max_sections: usize,
    /// Maximum number of bullets per section.
    /// When exceeded, least-reinforced bullets in that section are evicted.
    pub max_bullets_per_section: usize,
    /// Maximum approximate token budget for the playbook when rendered
    /// as Markdown. Prevents the playbook from consuming too much of
    /// the context window. Estimated at ~4 chars per token.
    pub max_token_budget: usize,
    /// Whether to enable automatic reflection after each turn.
    pub auto_reflect: bool,
    /// Minimum reinforcement count to survive eviction.
    /// Bullets below this threshold are candidates for removal when
    /// a section exceeds `max_bullets_per_section`.
    pub min_reinforcement_for_retention: u32,
}

impl Default for AceConfig {
    fn default() -> Self {
        Self {
            max_sections: 10,
            max_bullets_per_section: 15,
            max_token_budget: 4000,
            auto_reflect: true,
            min_reinforcement_for_retention: 2,
        }
    }
}

#[allow(dead_code)]
impl AceConfig {
    /// Create a config with reflection disabled (useful for testing).
    pub fn no_reflect() -> Self {
        Self {
            auto_reflect: false,
            ..Default::default()
        }
    }
}
