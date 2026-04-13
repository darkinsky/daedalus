/// Configuration for the sliding window memory with consolidation.
#[derive(Debug, Clone)]
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
}

impl Default for SlidingWindowConfig {
    fn default() -> Self {
        Self {
            max_messages: None,
            consolidation_threshold: 100,
            retention_window: 50,
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
        }
    }
}
