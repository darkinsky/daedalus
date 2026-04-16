/// Configuration for the Dynamic Cheatsheet module.
///
/// Controls capacity limits, eviction behavior, and whether automatic
/// reflection is enabled after each conversation turn.
///
/// Can be deserialized from YAML configuration files.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct CheatsheetConfig {
    /// Maximum number of entries in the cheatsheet.
    /// When exceeded, least-reinforced entries are evicted.
    pub max_entries: usize,
    /// Maximum approximate token budget for the cheatsheet when rendered
    /// as Markdown. Prevents the cheatsheet from consuming too much of
    /// the context window. Estimated at ~4 chars per token.
    pub max_token_budget: usize,
    /// Whether to enable automatic reflection after each turn.
    pub auto_reflect: bool,
    /// Minimum reinforcement count to survive eviction.
    /// Entries below this threshold are candidates for removal when
    /// the cheatsheet exceeds `max_entries`.
    pub min_reinforcement_for_retention: u32,
}

impl Default for CheatsheetConfig {
    fn default() -> Self {
        Self {
            max_entries: 50,
            max_token_budget: 2000,
            auto_reflect: true,
            min_reinforcement_for_retention: 1,
        }
    }
}

#[allow(dead_code)]
impl CheatsheetConfig {
    /// Create a config with reflection disabled (useful for testing).
    pub fn no_reflect() -> Self {
        Self {
            auto_reflect: false,
            ..Default::default()
        }
    }
}
