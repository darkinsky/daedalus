//! Tracing configuration types.

/// Configuration for the tracing subsystem.
///
/// Loaded from the `tracing` section of `daedalus.yaml`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct TracingConfig {
    /// Whether tracing is enabled globally.
    pub enabled: bool,
    /// Master switch: when true, overrides all content sub-options to true.
    /// Records full input/output content without truncation.
    pub full_content: bool,
    /// Fine-grained content recording options.
    /// Only effective when `full_content` is false.
    pub content: ContentConfig,
    /// Collector configurations.
    pub collectors: Vec<CollectorConfig>,
}

/// Fine-grained control over which content is recorded without truncation.
///
/// Each field defaults to `false` (truncated). When `TracingConfig::full_content`
/// is `true`, all fields are treated as `true` regardless of their actual value.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct ContentConfig {
    /// Record full LLM input messages (system prompt + conversation history).
    pub llm_input: bool,
    /// Record full LLM output (response content, reasoning, tool call arguments).
    pub llm_output: bool,
    /// Record full tool execution results and subagent results.
    pub tool_result: bool,
}

/// Resolved content recording flags (after merging `full_content` master switch).
///
/// Used at runtime by `TracingManager`, `TraceContext`, `SpanGuard`, and exporters
/// to decide whether to truncate each category of content.
#[derive(Debug, Clone, Copy)]
pub struct ContentFlags {
    /// Record full LLM input messages without truncation.
    pub llm_input: bool,
    /// Record full LLM output without truncation.
    pub llm_output: bool,
    /// Record full tool results without truncation.
    pub tool_result: bool,
}

impl ContentFlags {
    /// Resolve flags from the tracing configuration.
    ///
    /// If `full_content` master switch is on, all flags are true.
    /// Otherwise, uses the individual `content.*` settings.
    pub fn from_config(config: &TracingConfig) -> Self {
        if config.full_content {
            Self { llm_input: true, llm_output: true, tool_result: true }
        } else {
            Self {
                llm_input: config.content.llm_input,
                llm_output: config.content.llm_output,
                tool_result: config.content.tool_result,
            }
        }
    }

    /// Create flags with everything disabled (all truncated).
    pub fn none() -> Self {
        Self { llm_input: false, llm_output: false, tool_result: false }
    }

    /// Create flags with everything enabled (no truncation).
    #[allow(dead_code)]
    pub fn all() -> Self {
        Self { llm_input: true, llm_output: true, tool_result: true }
    }
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
