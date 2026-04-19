//! Middleware pipeline configuration.
//!
//! Defines the YAML structure for configuring which middleware layers are
//! active and their parameters. Loaded from the `middleware` section of
//! `daedalus.yaml`.
//!
//! ## Ordering convention
//!
//! Middleware entries are listed **innermost first** in the config:
//!
//! ```yaml
//! middleware:
//!   turn:
//!     - name: memory           # 1st = innermost (closest to core)
//!     - name: request_logging  # 2nd = middle
//!     - name: tracing          # 3rd = outermost (runs first on request)
//! ```
//!
//! This matches the `.with()` call order in code, eliminating any
//! hidden reversal. The outermost middleware wraps everything else.

/// Top-level middleware configuration.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct MiddlewareConfig {
    /// Turn-level middleware stack.
    ///
    /// Order: **innermost first** (closest to core handler).
    /// The last entry becomes the outermost wrapper.
    pub turn: Vec<MiddlewareEntry>,

    /// Tool-level middleware stack.
    ///
    /// Order: **innermost first** (closest to tool executor).
    /// The last entry becomes the outermost wrapper.
    pub tool: Vec<MiddlewareEntry>,
}

impl MiddlewareConfig {
    /// Validate the configuration and emit warnings for common mistakes.
    ///
    /// Called during bootstrap after loading from YAML.
    pub fn validate(&self) {
        // Warn if turn pipeline is non-empty but missing memory
        if !self.turn.is_empty() {
            let has_memory = self.turn.iter().any(|e| e.name == "memory" && e.enabled);
            if !has_memory {
                tracing::warn!(
                    "Middleware config: turn pipeline is configured but 'memory' is missing or disabled. \
                     Conversation history will not work! Add '- name: memory' to middleware.turn."
                );
            }
        }
    }
}

/// A single middleware entry in the configuration.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct MiddlewareEntry {
    /// Middleware name (must match a built-in name).
    ///
    /// Built-in turn middleware: `memory`, `cost`, `metrics`, `request_logging`, `tracing`
    /// Built-in tool middleware: `event`, `tool_logging`, `permission`, `tracing`
    pub name: String,

    /// Whether this middleware is enabled (default: true).
    ///
    /// Note: `tracing` middleware additionally respects the top-level
    /// `tracing.enabled` setting — if that is `false`, the tracing
    /// middleware becomes a no-op regardless of this field.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Middleware-specific configuration (opaque JSON/YAML map).
    ///
    /// Currently used by:
    /// - `permission`: `{ policy: "allow" | "deny_list" | "allow_list", tools: [...] }`
    #[serde(default)]
    pub config: serde_json::Value,
}

fn default_true() -> bool {
    true
}
