use std::path::Path;

use anyhow::{Context, Result};
use chrono::{Local, DateTime};
use serde::{Deserialize, Serialize};

/// A single entry in the history event log (cold data).
///
/// History entries are append-only summaries of past conversations.
/// They are NOT automatically loaded into the LLM context — the agent
/// searches them on demand via keyword matching.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct HistoryEntry {
    /// Timestamp when this entry was created.
    pub timestamp: DateTime<Local>,
    /// A 2-5 sentence summary of the conversation segment.
    pub summary: String,
    /// Keywords for grep-style searching.
    pub keywords: Vec<String>,
}

#[allow(dead_code)]
impl HistoryEntry {
    /// Create a new history entry with the current timestamp.
    pub fn new(summary: String, keywords: Vec<String>) -> Self {
        Self {
            timestamp: Local::now(),
            summary,
            keywords,
        }
    }

    /// Create a history entry with a specific timestamp (for testing).
    #[cfg(test)]
    pub fn with_timestamp(
        timestamp: DateTime<Local>,
        summary: String,
        keywords: Vec<String>,
    ) -> Self {
        Self { timestamp, summary, keywords }
    }

    /// Format this entry as a single log line:
    /// `[YYYY-MM-DD HH:MM] summary [keywords: kw1, kw2]`
    pub fn to_log_line(&self) -> String {
        let ts = self.timestamp.format("%Y-%m-%d %H:%M");
        if self.keywords.is_empty() {
            format!("[{}] {}", ts, self.summary)
        } else {
            format!("[{}] {} [keywords: {}]", ts, self.summary, self.keywords.join(", "))
        }
    }

    // ── JSONL persistence ──

    /// Append this entry to a JSONL file (one JSON object per line).
    ///
    /// Creates the file if it doesn't exist. This is the preferred
    /// persistence method for history entries since it supports
    /// efficient append-only writes.
    pub fn append_to_file(&self, path: &Path) -> Result<()> {
        use std::io::Write;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }
        let json = serde_json::to_string(self)
            .context("Failed to serialize HistoryEntry")?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("Failed to open history log: {}", path.display()))?;
        writeln!(file, "{}", json)
            .with_context(|| format!("Failed to write to history log: {}", path.display()))?;
        Ok(())
    }

    /// Load all history entries from a JSONL file.
    ///
    /// Returns an empty vector if the file doesn't exist.
    /// Malformed lines are silently skipped (graceful degradation).
    pub fn load_all(path: &Path) -> Result<Vec<Self>> {
        if !path.exists() {
            tracing::debug!(path = %path.display(), "No history log file found");
            return Ok(Vec::new());
        }
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read history log: {}", path.display()))?;
        let entries: Vec<Self> = content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| {
                serde_json::from_str(line)
                    .map_err(|e| {
                        tracing::warn!(error = %e, "Skipping malformed history entry");
                        e
                    })
                    .ok()
            })
            .collect();
        tracing::info!(
            path = %path.display(),
            entries = entries.len(),
            "History log loaded from disk"
        );
        Ok(entries)
    }

    /// Save all entries to a JSONL file (overwrite mode, atomic).
    ///
    /// Used for bulk persistence (e.g., on shutdown). Uses atomic write
    /// (write-to-temp-then-rename) to prevent data corruption on crash.
    pub fn save_all(entries: &[Self], path: &Path) -> Result<()> {
        let mut buf = String::new();
        for entry in entries {
            let json = serde_json::to_string(entry)
                .context("Failed to serialize HistoryEntry")?;
            buf.push_str(&json);
            buf.push('\n');
        }
        crate::memory::persistence::atomic_write(path, buf.as_bytes())
            .with_context(|| format!("Failed to save history log: {}", path.display()))?;
        tracing::debug!(
            path = %path.display(),
            entries = entries.len(),
            "History log saved to disk (atomic)"
        );
        Ok(())
    }
}
