//! Built-in tool for recording intermediate findings during long-running tasks.
//!
//! In multi-round tool-calling loops (like code review), the LLM's "findings"
//! exist only in the context history. When truncation compresses old rounds,
//! those findings are lost. This tool provides an explicit mechanism for the
//! LLM to persist notes that survive context truncation.
//!
//! Notes are stored in memory and injected into the session metadata
//! alongside the file-access index (see `tool_loop.rs`), ensuring they
//! remain visible to the LLM throughout the entire session.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;

use super::BuiltinTool;

/// Shared note storage accessible by both the tool and the tool loop.
///
/// The `Arc<Mutex<Vec<String>>>` is shared between `TakeNoteTool` and
/// the `run_tool_loop` function, which injects accumulated notes into
/// the session metadata sent to the LLM each round.
pub type SharedNotes = Arc<Mutex<Vec<String>>>;

/// Create a new empty shared notes container.
pub fn new_shared_notes() -> SharedNotes {
    Arc::new(Mutex::new(Vec::new()))
}

/// Built-in tool that records a note for later reference.
///
/// Notes are accumulated in a shared `Vec<String>` and injected into the
/// LLM's context each round by `run_tool_loop`, ensuring they survive
/// tool history truncation.
pub struct TakeNoteTool {
    notes: SharedNotes,
}

impl TakeNoteTool {
    /// Create a new take_note tool backed by the given shared notes.
    pub fn new(notes: SharedNotes) -> Self {
        Self { notes }
    }
}

#[async_trait]
impl BuiltinTool for TakeNoteTool {
    fn name(&self) -> &str {
        "take_note"
    }

    fn description(&self) -> &str {
        "Record a finding, observation, or intermediate result that should be preserved \
         across the session. Notes survive context truncation and will be visible in \
         every subsequent round. Use this to avoid losing important discoveries when \
         working on long tasks like code review, exploration, or auditing."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "note": {
                    "type": "string",
                    "description": "The finding or observation to record. Be concise but specific — include file paths, line numbers, and severity when applicable."
                }
            },
            "required": ["note"]
        })
    }

    fn is_metadata_only(&self) -> bool {
        true
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String> {
        let note = arguments
            .get("note")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: 'note'"))?;

        if note.trim().is_empty() {
            return Ok("Note is empty, nothing recorded.".to_string());
        }

        let count = {
            let mut notes = self.notes.lock()
                .map_err(|_| anyhow::anyhow!("Failed to acquire notes lock"))?;
            notes.push(note.to_string());
            notes.len()
        };

        tracing::debug!(
            note_count = count,
            note_len = note.len(),
            "Note recorded"
        );

        Ok(format!("Note #{} recorded.", count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_take_note_basic() {
        let notes = new_shared_notes();
        let tool = TakeNoteTool::new(Arc::clone(&notes));

        let result = tool.execute(serde_json::json!({"note": "Found bug in line 42"})).await.unwrap();
        assert!(result.contains("Note #1"));
        assert_eq!(notes.lock().unwrap().len(), 1);
        assert_eq!(notes.lock().unwrap()[0], "Found bug in line 42");
    }

    #[tokio::test]
    async fn test_take_note_multiple() {
        let notes = new_shared_notes();
        let tool = TakeNoteTool::new(Arc::clone(&notes));

        tool.execute(serde_json::json!({"note": "Issue 1"})).await.unwrap();
        let result = tool.execute(serde_json::json!({"note": "Issue 2"})).await.unwrap();
        assert!(result.contains("Note #2"));
        assert_eq!(notes.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_take_note_empty() {
        let notes = new_shared_notes();
        let tool = TakeNoteTool::new(Arc::clone(&notes));

        let result = tool.execute(serde_json::json!({"note": "  "})).await.unwrap();
        assert!(result.contains("empty"));
        assert_eq!(notes.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_take_note_missing_param() {
        let notes = new_shared_notes();
        let tool = TakeNoteTool::new(Arc::clone(&notes));

        let result = tool.execute(serde_json::json!({})).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_take_note_schema() {
        let notes = new_shared_notes();
        let tool = TakeNoteTool::new(notes);
        assert_eq!(tool.name(), "take_note");
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["note"].is_object());
    }
}
