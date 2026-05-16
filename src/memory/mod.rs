//! Memory subsystem — pluggable conversation memory strategies.
//!
//! ## Module structure
//!
//! - `traits`   — `Memory` trait, `MemoryFactory` trait, `PersistentState`
//! - `utils`    — Token estimation, text truncation, `MessageBuffer`
//! - `factory`  — Strategy → factory mapping (`create_memory_factory`)
//!
//! ## Strategy modules
//!
//! - `sliding_window`      — Default dual-layer memory with compact
//! - `dynamic_cheatsheet`  — Adaptive cheatsheet via LLM reflection
//! - `agentic`             — A-MEM knowledge graph memory
//! - `wiki`                — LLM Wiki (Karpathy pattern)
//! - `ace`                 — ACE Playbook memory
//! - `mempalace`           — Memory Palace with spatial organization

// ── Strategy modules ──
pub mod ace;
pub mod agentic;
pub mod dynamic_cheatsheet;
pub mod mempalace;
pub mod persistence;
pub mod sliding_window;
pub mod wiki;

// ── Internal modules ──
mod traits;
mod utils;
mod factory;

// ── Re-exports: traits and types (public API) ──
pub use traits::{Memory, MemoryFactory, PersistentState};

// ── Re-exports: strategy factories ──
pub use ace::AceFactory;
pub use sliding_window::SlidingWindowFactory;
pub use sliding_window::ContextPressureLevel;
pub use dynamic_cheatsheet::CheatsheetFactory;
pub use agentic::AgenticFactory;
pub use mempalace::MemPalaceFactory;
pub use wiki::WikiFactory;

// ── Re-exports: factory selection ──
pub use factory::create_memory_factory;

// ── Re-exports: utilities (crate-internal) ──
pub(crate) use utils::{
    estimate_tokens, truncate_to_token_budget, strip_directive_prefix,
    MessageBuffer, DEFAULT_MAX_MESSAGES,
};

// Items used only within memory submodules — not re-exported, accessed via `super::utils::`.
// - estimate_tokens_with_mode, TokenEstimationMode, is_cjk, CHARS_PER_TOKEN
