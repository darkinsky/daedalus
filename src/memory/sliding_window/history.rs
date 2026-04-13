use chrono::{Local, DateTime};

/// A single entry in the history event log (cold data).
///
/// History entries are append-only summaries of past conversations.
/// They are NOT automatically loaded into the LLM context — the agent
/// searches them on demand via keyword matching.
#[derive(Debug, Clone)]
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
}
