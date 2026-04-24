/// Curator operating mode — controls how the cheatsheet is updated after reflection.
///
/// Aligned with the Dynamic Cheatsheet paper (arxiv:2504.07952):
/// - **FullRewrite**: The Curator outputs a complete updated cheatsheet each time,
///   handling compression, merging, and eviction via LLM judgment. This is the
///   paper's original design and is recommended for best quality.
/// - **Incremental**: The Curator outputs `NEW:`/`UPDATE:`/`REINFORCE:` directives
///   and the code applies them programmatically. Lighter-weight but loses the
///   Curator's ability to do global reorganization and semantic compression.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CuratorMode {
    /// Paper-faithful mode: Curator outputs the full updated cheatsheet.
    /// The LLM decides what to keep, compress, merge, or discard.
    #[default]
    FullRewrite,
    /// Lightweight mode: Curator outputs incremental directives (NEW/UPDATE/REINFORCE).
    /// Code handles merging and eviction.
    Incremental,
}

/// Configuration for the Dynamic Cheatsheet module.
///
/// Controls capacity limits, eviction behavior, curator mode, and whether
/// automatic reflection is enabled after each conversation turn.
///
/// Can be deserialized from YAML configuration files.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct CheatsheetConfig {
    /// Maximum number of entries in the cheatsheet.
    /// When exceeded, least-reinforced entries are evicted (Incremental mode)
    /// or the Curator is instructed to compress (FullRewrite mode).
    pub max_entries: usize,
    /// Maximum approximate token budget for the cheatsheet when rendered
    /// as Markdown. Prevents the cheatsheet from consuming too much of
    /// the context window. Estimated at ~4 chars per token.
    pub max_token_budget: usize,
    /// Whether to enable automatic reflection after each turn.
    pub auto_reflect: bool,
    /// Minimum reinforcement count to survive eviction (Incremental mode only).
    /// Entries below this threshold are candidates for removal when
    /// the cheatsheet exceeds `max_entries`.
    pub min_reinforcement_for_retention: u32,
    /// How the Curator updates the cheatsheet.
    /// - `full_rewrite` (default): Curator outputs complete updated cheatsheet.
    /// - `incremental`: Curator outputs NEW/UPDATE/REINFORCE directives.
    pub curator_mode: CuratorMode,
}

impl Default for CheatsheetConfig {
    fn default() -> Self {
        Self {
            max_entries: 50,
            max_token_budget: 2000,
            auto_reflect: true,
            min_reinforcement_for_retention: 1,
            curator_mode: CuratorMode::default(),
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

    /// Create a config using incremental curator mode.
    pub fn incremental() -> Self {
        Self {
            curator_mode: CuratorMode::Incremental,
            ..Default::default()
        }
    }
}
