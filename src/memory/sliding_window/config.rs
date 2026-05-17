/// Context pressure level indicating how close the context window is to capacity.
///
/// Used by the middleware layer to make decisions about when to compact
/// and whether to override the consolidation/compact mutual exclusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ContextPressureLevel {
    /// Context usage is within normal bounds. No action needed.
    Normal,
    /// Context usage exceeds the warning threshold.
    /// The agent should be aware that compact may trigger soon.
    Warning,
    /// Context usage exceeds the auto-compact threshold.
    /// Auto-compact should be triggered.
    High,
    /// Context usage exceeds the hard limit.
    /// Compact MUST run immediately, even if consolidation just ran
    /// (overrides the mutual exclusion rule).
    Critical,
}

/// Configuration for the sliding window memory with consolidation.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
#[allow(dead_code)]
pub struct SlidingWindowConfig {
    /// Maximum number of messages in the hot-data window sent to the LLM.
    /// `None` means unlimited (full history, no windowing).
    pub max_messages: Option<usize>,
    /// Number of unconsolidated messages that triggers automatic consolidation.
    /// When `unconsolidated_count >= consolidation_threshold`, consolidation
    /// should be triggered.
    pub consolidation_threshold: usize,
    /// Number of recent messages to retain (not consolidate) when consolidation
    /// runs. This is the "retention window" — these messages stay as-is.
    pub retention_window: usize,

    // ── Context compression (compact) ──

    /// Estimated token budget for the context window.
    /// When the estimated token count of `build_messages()` exceeds
    /// `compact_threshold_ratio * context_budget`, auto-compact triggers.
    /// Default: 0 (not explicitly configured — will be populated from model registry).
    pub context_budget: usize,
    /// Ratio of context budget usage that triggers a warning log.
    /// E.g., 0.8 means warn when 80% of the budget is used.
    /// Default: 0.8.
    pub compact_warning_ratio: f64,
    /// Ratio of context budget usage that triggers auto-compact.
    /// E.g., 0.93 means compact when 93% of the budget is used.
    /// Default: 0.93.
    pub compact_threshold_ratio: f64,
    /// Ratio of context budget usage that forces immediate compact,
    /// overriding the consolidation/compact mutual exclusion.
    /// E.g., 0.97 means force compact when 97% of the budget is used.
    /// Default: 0.97.
    pub compact_hard_limit_ratio: f64,
    /// Number of recent messages to preserve verbatim during compact.
    /// These messages are NOT summarized — they stay as-is so the LLM
    /// retains immediate context. Default: 10.
    pub compact_preserve_recent: usize,

    /// Optional custom system prompt for the compact LLM call.
    /// When `None`, uses the built-in default from `prompts::COMPACT_SYSTEM_PROMPT`.
    /// This enables:
    /// - User customization (via YAML config)
    /// - Future "compression strategy evolution" (auto-tuning the prompt)
    #[serde(default)]
    pub compact_custom_prompt: Option<String>,
}

impl Default for SlidingWindowConfig {
    fn default() -> Self {
        Self {
            max_messages: Some(100),
            consolidation_threshold: 100,
            retention_window: 50,
            context_budget: 0,
            compact_warning_ratio: 0.8,
            compact_threshold_ratio: 0.93,
            compact_hard_limit_ratio: 0.97,
            compact_preserve_recent: 10,
            compact_custom_prompt: None,
        }
    }
}

#[allow(dead_code)]
impl SlidingWindowConfig {
    /// Create a config with a specific message window and default consolidation.
    pub fn with_max_messages(max_messages: usize) -> Self {
        Self {
            max_messages: Some(max_messages),
            ..Default::default()
        }
    }

    /// Create an unlimited config (no windowing, no consolidation).
    pub fn unlimited() -> Self {
        Self {
            max_messages: None,
            consolidation_threshold: usize::MAX,
            retention_window: 0,
            context_budget: 128_000,
            compact_warning_ratio: 1.0,
            compact_threshold_ratio: 1.0, // never auto-compact
            compact_hard_limit_ratio: 1.0,
            compact_preserve_recent: 10,
            compact_custom_prompt: None,
        }
    }
}
