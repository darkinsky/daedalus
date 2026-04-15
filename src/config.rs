//! Configuration module — unified loading and type definitions.
//!
//! ## Module structure
//!
//! - `loader`       — Unified YAML file reader (reads once, returns all sections)
//! - `agent_config` — AgentConfig type and soul file resolution
//! - `logging`      — LogConfig types and tracing subscriber initialization

mod agent_config;
mod loader;
pub(crate) mod logging;

pub use agent_config::AgentConfig;
pub use loader::load_from_workspace;
pub use logging::init as init_logging;
