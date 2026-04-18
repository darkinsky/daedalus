pub(crate) mod config;
mod consolidation;
mod factory;
mod history;
mod long_term;
mod memory;

#[cfg(test)]
mod tests;

pub use factory::SlidingWindowFactory;

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
