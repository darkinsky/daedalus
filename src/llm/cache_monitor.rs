//! Prompt cache break detection.
//!
//! Monitors `cached_tokens` across LLM calls to detect unexpected cache
//! invalidations. When cache reads drop from a high value to zero without
//! a known cause (compact, micro_compact), this indicates a problem with
//! message construction that should be investigated.
//!
//! Modeled after Claude Code's `PROMPT_CACHE_BREAK_DETECTION` feature.

use crate::llm::TokenUsage;

/// Monitors prompt cache hit rates across LLM calls.
///
/// Detects unexpected cache breaks — situations where the cached prefix
/// is accidentally invalidated by changes to message construction.
#[derive(Debug, Clone)]
pub struct CacheMonitor {
    /// Cached tokens from the last LLM call.
    last_cached_tokens: u64,
    /// Whether the next cache miss is expected (due to compact/micro_compact).
    expected_invalidation: bool,
    /// Total number of cache breaks detected (for diagnostics).
    unexpected_breaks: u64,
    /// Total number of expected invalidations (for diagnostics).
    expected_breaks: u64,
}

impl CacheMonitor {
    /// Create a new cache monitor.
    pub fn new() -> Self {
        Self {
            last_cached_tokens: 0,
            expected_invalidation: false,
            unexpected_breaks: 0,
            expected_breaks: 0,
        }
    }

    /// Record token usage from an LLM call.
    ///
    /// Should be called after every successful LLM response.
    /// Detects cache breaks and logs warnings when unexpected.
    pub fn record_usage(&mut self, usage: &TokenUsage) {
        let cached = usage.cached_tokens.unwrap_or(0);

        // Detect a potential cache break:
        // Last call had significant cache reads, this call has zero.
        if self.last_cached_tokens > 1000 && cached == 0 {
            if self.expected_invalidation {
                // Expected — compact or micro_compact was performed
                self.expected_breaks += 1;
                tracing::debug!(
                    last_cached = self.last_cached_tokens,
                    "Prompt cache break (expected — after compact/micro_compact)"
                );
            } else {
                // Unexpected — something changed the message prefix
                self.unexpected_breaks += 1;
                tracing::warn!(
                    last_cached = self.last_cached_tokens,
                    unexpected_breaks = self.unexpected_breaks,
                    "Unexpected prompt cache break detected — check message \
                     construction for unintended prefix changes"
                );
            }
        }

        self.last_cached_tokens = cached;
        self.expected_invalidation = false;
    }

    /// Notify the monitor that a cache invalidation is expected.
    ///
    /// Call this after any operation that intentionally changes the message
    /// prefix (compact, micro_compact, new session, etc.).
    pub fn notify_expected_invalidation(&mut self) {
        self.expected_invalidation = true;
    }

    /// Return the number of unexpected cache breaks detected.
    pub fn unexpected_breaks(&self) -> u64 {
        self.unexpected_breaks
    }

    /// Return the last observed cached token count.
    pub fn last_cached_tokens(&self) -> u64 {
        self.last_cached_tokens
    }
}

impl Default for CacheMonitor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(cached: u64) -> TokenUsage {
        TokenUsage {
            prompt_tokens: Some(10000),
            completion_tokens: Some(500),
            total_tokens: Some(10500),
            cached_tokens: Some(cached),
        }
    }

    #[test]
    fn test_no_break_when_cache_stays_warm() {
        let mut mon = CacheMonitor::new();
        mon.record_usage(&usage(5000));
        mon.record_usage(&usage(5000));
        mon.record_usage(&usage(6000));
        assert_eq!(mon.unexpected_breaks(), 0);
    }

    #[test]
    fn test_detects_unexpected_break() {
        let mut mon = CacheMonitor::new();
        mon.record_usage(&usage(5000));
        mon.record_usage(&usage(0)); // Unexpected drop
        assert_eq!(mon.unexpected_breaks(), 1);
    }

    #[test]
    fn test_expected_break_not_counted() {
        let mut mon = CacheMonitor::new();
        mon.record_usage(&usage(5000));
        mon.notify_expected_invalidation();
        mon.record_usage(&usage(0)); // Expected drop after compact
        assert_eq!(mon.unexpected_breaks(), 0);
        assert_eq!(mon.expected_breaks, 1);
    }

    #[test]
    fn test_low_cache_drop_not_flagged() {
        let mut mon = CacheMonitor::new();
        mon.record_usage(&usage(500)); // Low baseline
        mon.record_usage(&usage(0));   // Drop from low — not flagged
        assert_eq!(mon.unexpected_breaks(), 0);
    }
}
