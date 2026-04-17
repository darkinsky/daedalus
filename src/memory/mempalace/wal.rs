//! Write-Ahead Log (WAL) for MemPalace write operations.
//!
//! Every write operation is logged to a JSONL file before execution.
//! This provides an audit trail for detecting memory poisoning and
//! enables review/rollback of writes from external or untrusted sources.
//!
//! Matches the original MemPalace `_wal_log()` implementation.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::Local;
use serde_json::Value;

/// Keys whose values should be redacted in WAL entries to avoid logging
/// sensitive content.
const WAL_REDACT_KEYS: &[&str] = &[
    "content",
    "content_preview",
    "document",
    "entry",
    "entry_preview",
    "query",
    "text",
];

/// Write-Ahead Log for audit trail of write operations.
pub struct WriteAheadLog {
    /// Path to the WAL JSONL file.
    wal_file: PathBuf,
    /// Whether WAL is enabled.
    enabled: bool,
}

impl WriteAheadLog {
    /// Create a new WAL instance.
    pub fn new(wal_dir: &Path, enabled: bool) -> Self {
        let wal_file = wal_dir.join("write_log.jsonl");
        if enabled {
            if let Err(e) = fs::create_dir_all(wal_dir) {
                tracing::warn!(error = %e, "Failed to create WAL directory");
            }
        }
        Self { wal_file, enabled }
    }

    /// Log a write operation to the WAL.
    pub fn log(&self, operation: &str, params: &serde_json::Map<String, Value>) {
        if !self.enabled {
            return;
        }

        // Redact sensitive content
        let mut safe_params = serde_json::Map::new();
        for (k, v) in params {
            if WAL_REDACT_KEYS.contains(&k.as_str()) {
                if let Some(s) = v.as_str() {
                    safe_params.insert(
                        k.clone(),
                        Value::String(format!("[REDACTED {} chars]", s.len())),
                    );
                } else {
                    safe_params.insert(k.clone(), Value::String("[REDACTED]".to_string()));
                }
            } else {
                safe_params.insert(k.clone(), v.clone());
            }
        }

        let entry = serde_json::json!({
            "timestamp": Local::now().to_rfc3339(),
            "operation": operation,
            "params": safe_params,
        });

        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.wal_file)
        {
            Ok(mut file) => {
                if let Err(e) = writeln!(file, "{}", entry) {
                    tracing::error!(error = %e, "WAL write failed");
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "Failed to open WAL file");
            }
        }
    }

    /// Log a simple operation with string parameters.
    pub fn log_simple(&self, operation: &str, key: &str, value: &str) {
        let mut params = serde_json::Map::new();
        params.insert(key.to_string(), Value::String(value.to_string()));
        self.log(operation, &params);
    }
}
