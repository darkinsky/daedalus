use anyhow::{Context, Result};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{
    EnvFilter,
    fmt,
    fmt::time::OffsetTime,
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

/// Log output format.
#[derive(Debug, Clone, Default)]
pub enum LogFormat {
    /// Human-readable, colored output (default)
    #[default]
    Pretty,
    /// Compact single-line output
    Compact,
    /// Structured JSON output
    Json,
    /// Full verbose output with all metadata
    Full,
}

impl LogFormat {
    /// Parse a log format string (case-insensitive), falling back to default.
    pub fn parse_or_default(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "json" => Self::Json,
            "compact" => Self::Compact,
            "full" => Self::Full,
            _ => Self::Pretty,
        }
    }
}

/// Log file rotation policy.
#[derive(Debug, Clone, Default)]
pub enum LogRotation {
    /// Rotate log files every minute (useful for testing)
    Minutely,
    /// Rotate log files every hour
    Hourly,
    /// Rotate log files every day (default)
    #[default]
    Daily,
    /// Never rotate — single log file
    Never,
}

impl LogRotation {
    /// Parse a rotation string (case-insensitive), falling back to default.
    pub fn parse_or_default(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "minutely" | "minute" => Self::Minutely,
            "hourly" | "hour" => Self::Hourly,
            "daily" | "day" => Self::Daily,
            "never" | "none" => Self::Never,
            _ => Self::Daily,
        }
    }

    /// Convert to tracing_appender's Rotation type.
    fn to_appender_rotation(&self) -> tracing_appender::rolling::Rotation {
        match self {
            Self::Minutely => tracing_appender::rolling::Rotation::MINUTELY,
            Self::Hourly => tracing_appender::rolling::Rotation::HOURLY,
            Self::Daily => tracing_appender::rolling::Rotation::DAILY,
            Self::Never => tracing_appender::rolling::Rotation::NEVER,
        }
    }
}

/// Display options for a log layer.
///
/// Extracted to avoid repeating the same six booleans in every layer builder.
#[derive(Debug, Clone)]
struct LayerDisplayOpts {
    with_file: bool,
    with_line_number: bool,
    with_target: bool,
    with_thread_names: bool,
    with_thread_ids: bool,
    with_ansi: bool,
}

impl LayerDisplayOpts {
    /// Build display options from user-facing `LogConfig` (for stderr).
    fn from_config(config: &LogConfig) -> Self {
        Self {
            with_file: config.with_file,
            with_line_number: config.with_line_number,
            with_target: config.with_target,
            with_thread_names: config.with_thread_names,
            with_thread_ids: config.with_thread_ids,
            with_ansi: config.with_ansi,
        }
    }

    /// Build display options for file logging (full metadata, no ANSI).
    fn for_file() -> Self {
        Self {
            with_file: true,
            with_line_number: true,
            with_target: true,
            with_thread_names: true,
            with_thread_ids: true,
            with_ansi: false,
        }
    }
}

/// Apply common display options to a tracing layer.
///
/// This macro eliminates the repetitive `.with_file(...).with_line_number(...)`
/// chains that were previously duplicated across every format variant.
macro_rules! apply_display_opts {
    ($layer:expr, $timer:expr, $writer:expr, $opts:expr) => {
        $layer
            .with_timer($timer)
            .with_writer($writer)
            .with_file($opts.with_file)
            .with_line_number($opts.with_line_number)
            .with_target($opts.with_target)
            .with_thread_names($opts.with_thread_names)
            .with_thread_ids($opts.with_thread_ids)
            .with_ansi($opts.with_ansi)
    };
}

/// Logging configuration.
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// Log level filter directive (e.g., "daedalus=debug,rig=info")
    pub filter: String,
    /// Output format
    pub format: LogFormat,
    /// Whether to include source file location in log output
    pub with_file: bool,
    /// Whether to include line numbers in log output
    pub with_line_number: bool,
    /// Whether to include the target module path
    pub with_target: bool,
    /// Whether to include thread names
    pub with_thread_names: bool,
    /// Whether to include thread IDs
    pub with_thread_ids: bool,
    /// Whether to use ANSI color codes
    pub with_ansi: bool,
    /// Directory for rolling log files (None = no file logging)
    pub log_dir: Option<String>,
    /// Log file name prefix (default: "daedalus")
    pub log_file_prefix: String,
    /// Log file rotation policy
    pub rotation: LogRotation,
    /// Output format for file logging (defaults to Json if not specified)
    pub file_format: Option<LogFormat>,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            filter: "daedalus=debug".to_string(),
            format: LogFormat::default(),
            with_file: false,
            with_line_number: false,
            with_target: true,
            with_thread_names: false,
            with_thread_ids: false,
            with_ansi: true,
            log_dir: None,
            log_file_prefix: "daedalus".to_string(),
            rotation: LogRotation::default(),
            file_format: None,
        }
    }
}

impl LogConfig {
    /// Load log configuration from environment variables.
    ///
    /// Supported env vars:
    /// - `RUST_LOG`: Standard log filter directive (overrides default filter)
    /// - `DAEDALUS_LOG_FORMAT`: Output format ("pretty", "compact", "json", "full")
    /// - `DAEDALUS_LOG_FILE`: Whether to show source file ("true"/"false")
    /// - `DAEDALUS_LOG_LINE`: Whether to show line numbers ("true"/"false")
    /// - `DAEDALUS_LOG_TARGET`: Whether to show target module ("true"/"false", default: true)
    /// - `DAEDALUS_LOG_THREAD_NAMES`: Whether to show thread names ("true"/"false")
    /// - `DAEDALUS_LOG_ANSI`: Whether to use ANSI colors ("true"/"false", default: true)
    /// - `DAEDALUS_LOG_DIR`: Directory for rolling log files (enables file logging)
    /// - `DAEDALUS_LOG_FILE_PREFIX`: Log file name prefix (default: "daedalus")
    /// - `DAEDALUS_LOG_ROTATION`: Rotation policy ("minutely", "hourly", "daily", "never")
    /// - `DAEDALUS_LOG_FILE_FORMAT`: File log format ("pretty", "compact", "json", "full"; default: "json")
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(filter) = std::env::var("RUST_LOG") {
            config.filter = filter;
        }

        if let Ok(format) = std::env::var("DAEDALUS_LOG_FORMAT") {
            config.format = LogFormat::parse_or_default(&format);
        }

        if let Ok(val) = std::env::var("DAEDALUS_LOG_FILE") {
            config.with_file = val.eq_ignore_ascii_case("true") || val == "1";
        }

        if let Ok(val) = std::env::var("DAEDALUS_LOG_LINE") {
            config.with_line_number = val.eq_ignore_ascii_case("true") || val == "1";
        }

        if let Ok(val) = std::env::var("DAEDALUS_LOG_TARGET") {
            config.with_target = val.eq_ignore_ascii_case("true") || val == "1";
        }

        if let Ok(val) = std::env::var("DAEDALUS_LOG_THREAD_NAMES") {
            config.with_thread_names = val.eq_ignore_ascii_case("true") || val == "1";
        }

        if let Ok(val) = std::env::var("DAEDALUS_LOG_ANSI") {
            config.with_ansi = val.eq_ignore_ascii_case("true") || val == "1";
        }

        if let Ok(dir) = std::env::var("DAEDALUS_LOG_DIR") {
            if !dir.is_empty() {
                config.log_dir = Some(dir);
            }
        }

        if let Ok(prefix) = std::env::var("DAEDALUS_LOG_FILE_PREFIX") {
            if !prefix.is_empty() {
                config.log_file_prefix = prefix;
            }
        }

        if let Ok(rotation) = std::env::var("DAEDALUS_LOG_ROTATION") {
            config.rotation = LogRotation::parse_or_default(&rotation);
        }

        if let Ok(file_format) = std::env::var("DAEDALUS_LOG_FILE_FORMAT") {
            config.file_format = Some(LogFormat::parse_or_default(&file_format));
        }

        config
    }

    /// Build the `EnvFilter` from the configured filter string.
    fn build_filter(&self) -> EnvFilter {
        EnvFilter::try_new(&self.filter).unwrap_or_else(|e| {
            eprintln!(
                "Warning: invalid log filter '{}': {}, falling back to default",
                self.filter, e
            );
            EnvFilter::new("daedalus=debug")
        })
    }
}

/// Guard that keeps the non-blocking file writer alive.
///
/// **Important**: This must be held for the entire lifetime of the application.
/// Dropping it will flush and close the log file writer.
pub struct LogGuard {
    _file_guard: Option<WorkerGuard>,
}

/// Initialize the global tracing subscriber with the given configuration.
///
/// Returns a `LogGuard` that **must** be held until the application exits.
/// Dropping the guard flushes any buffered log entries to the file.
///
/// # Example
/// ```no_run
/// use daedalus::logging::{LogConfig, init};
///
/// // Use default configuration (stderr only)
/// let _guard = init(&LogConfig::default()).unwrap();
///
/// // Enable file logging via env vars:
/// //   DAEDALUS_LOG_DIR=./logs DAEDALUS_LOG_ROTATION=daily cargo run
/// ```
pub fn init(config: &LogConfig) -> Result<LogGuard> {
    let filter = config.build_filter();
    let timer = local_timer();

    if let Some(ref log_dir) = config.log_dir {
        // File logging mode: output to rolling file only (no stderr)
        let rotation = config.rotation.to_appender_rotation();
        let file_appender = tracing_appender::rolling::RollingFileAppender::new(
            rotation,
            log_dir,
            &config.log_file_prefix,
        );
        let (non_blocking, file_guard) = tracing_appender::non_blocking(file_appender);

        let file_format = config.file_format.as_ref().unwrap_or(&LogFormat::Json);
        let file_opts = LayerDisplayOpts::for_file();
        let file_layer = build_format_layer(file_format, non_blocking, timer, &file_opts);

        tracing_subscriber::registry()
            .with(filter)
            .with(file_layer)
            .try_init()
            .context("Failed to initialize tracing subscriber with file logging")?;

        tracing::debug!(
            "Logging initialized with filter: {}, file output: {}/{}",
            config.filter,
            log_dir,
            config.log_file_prefix
        );

        Ok(LogGuard {
            _file_guard: Some(file_guard),
        })
    } else {
        // No file logging — stderr only
        let stderr_opts = LayerDisplayOpts::from_config(config);
        let stderr_layer = build_format_layer(&config.format, std::io::stderr, timer, &stderr_opts);

        tracing_subscriber::registry()
            .with(filter)
            .with(stderr_layer)
            .try_init()
            .context("Failed to initialize tracing subscriber")?;

        tracing::debug!("Logging initialized with filter: {}", config.filter);

        Ok(LogGuard {
            _file_guard: None,
        })
    }
}

/// Create a local-timezone timer for log timestamps.
///
/// Attempts to detect the local UTC offset at startup. Falls back to UTC
/// if the offset cannot be determined (e.g., on some sandboxed environments).
fn local_timer() -> OffsetTime<time::format_description::well_known::Rfc3339> {
    let offset = time::UtcOffset::current_local_offset()
        .unwrap_or(time::UtcOffset::UTC);
    OffsetTime::new(
        offset,
        time::format_description::well_known::Rfc3339,
    )
}

/// Build a formatting layer for the given format, writer, and display options.
///
/// This is the single entry point for creating all log layers (both stderr and
/// file), eliminating the previous code duplication between `build_layer` and
/// `build_layer_with_opts`.
fn build_format_layer<S, W>(
    format: &LogFormat,
    writer: W,
    timer: OffsetTime<time::format_description::well_known::Rfc3339>,
    opts: &LayerDisplayOpts,
) -> Box<dyn tracing_subscriber::Layer<S> + Send + Sync>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    W: for<'w> fmt::MakeWriter<'w> + Send + Sync + 'static,
{
    match format {
        LogFormat::Json => Box::new(
            apply_display_opts!(fmt::layer().json(), timer, writer, opts),
        ),
        LogFormat::Compact => Box::new(
            apply_display_opts!(fmt::layer().compact(), timer, writer, opts),
        ),
        LogFormat::Full => Box::new(
            apply_display_opts!(fmt::layer(), timer, writer, opts),
        ),
        LogFormat::Pretty => Box::new(
            apply_display_opts!(fmt::layer().pretty(), timer, writer, opts),
        ),
    }
}
