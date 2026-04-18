//! Tracing configuration types.

/// Configuration for the tracing subsystem.
///
/// Loaded from the `tracing` section of `daedalus.yaml`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct TracingConfig {
    /// Whether tracing is enabled globally.
    pub enabled: bool,
    /// Whether to record full input/output content without truncation.
    /// When false (default), content is truncated to preview lengths for efficiency.
    /// When true, all inputs and outputs are recorded in full.
    pub full_content: bool,
    /// Collector configurations.
    pub collectors: Vec<CollectorConfig>,
}

/// Configuration for a single tracing collector/exporter.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CollectorConfig {
    /// Write traces to JSON Lines files.
    File {
        /// Directory to write trace files to.
        /// Defaults to `.daedalus/traces/`.
        path: Option<String>,
        /// Output format: "jsonl" (one JSON object per line) or "pretty" (indented).
        #[serde(default = "default_format")]
        format: FileFormat,
    },
    /// Print trace summaries to stderr (for development).
    Console {
        /// Verbosity level: "summary" (default) or "full".
        #[serde(default)]
        verbosity: ConsoleVerbosity,
    },
    /// Send traces to an OpenTelemetry collector via OTLP/HTTP JSON.
    Otel {
        /// OTLP endpoint URL (e.g., "http://localhost:4318").
        /// The `/v1/traces` path is appended automatically.
        endpoint: String,
        /// Service name reported in resource attributes.
        /// Defaults to "daedalus-agent".
        service_name: Option<String>,
    },
    /// Send traces to Langfuse for LLM observability.
    Langfuse {
        /// Langfuse project public key (pk-...).
        public_key: String,
        /// Langfuse project secret key (sk-...).
        secret_key: String,
        /// Custom Langfuse host URL.
        /// Defaults to "https://cloud.langfuse.com".
        host: Option<String>,
    },
}

/// File output format.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileFormat {
    /// One JSON object per line (compact, machine-readable).
    #[default]
    Jsonl,
    /// Pretty-printed JSON (human-readable, one file per trace).
    Pretty,
    /// YAML-like indented text format (most human-readable, tree structure).
    Yaml,
}

/// Console output verbosity.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsoleVerbosity {
    /// Print only a one-line summary per trace.
    #[default]
    Summary,
    /// Print full span tree.
    Full,
}

fn default_format() -> FileFormat {
    FileFormat::Jsonl
}
