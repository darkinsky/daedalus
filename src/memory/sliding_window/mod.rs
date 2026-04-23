pub(crate) mod config;
mod consolidation;
mod factory;
mod history;
mod long_term;
mod memory;
mod prompts;

#[cfg(test)]
mod tests;

pub use factory::SlidingWindowFactory;

// Re-export CompactResult for external use (e.g., by agent layer).
#[allow(unused_imports)]
pub use memory::CompactResult;

// Re-export ContextPressureLevel for use by middleware and agent layers.
pub use config::ContextPressureLevel;

// Re-exports for test use only. These types are internal implementation details
// accessed by the integration tests in `tests.rs` via `crate::memory::sliding_window::*`.
#[cfg(test)]
pub(crate) use config::SlidingWindowConfig;
#[cfg(test)]
pub(crate) use consolidation::ConsolidationResult;
#[cfg(test)]
pub(crate) use history::HistoryEntry;
#[cfg(test)]
pub(crate) use long_term::LongTermMemory;
#[cfg(test)]
pub(crate) use memory::SlidingWindowMemory;
