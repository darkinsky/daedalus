mod agent_config;
mod logging;

pub use agent_config::AgentConfig;
pub use logging::{LogConfig, init as init_logging};

// Re-export for potential external use.
#[allow(unused_imports)]
pub use agent_config::DEFAULT_SYSTEM_PROMPT;
#[allow(unused_imports)]
pub use logging::LogGuard;
