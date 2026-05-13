//! Tool loop checkpoint persistence for crash recovery and resume.
//!
//! Saves the tool loop's intermediate state (tool history, round number,
//! accumulated usage, files read) to disk periodically. If the process
//! crashes mid-loop, the `/resume` command can restore from the last
//! checkpoint and continue execution.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::llm::{TokenUsage, ToolCall, ToolResponse, ToolRound};
use crate::memory::persistence::atomic_write;

/// How often (in rounds) to auto-save a checkpoint.
pub(crate) const CHECKPOINT_INTERVAL: usize = 5;

// ── Serializable types ──

#[derive(Serialize, Deserialize)]
struct SerializableToolCall {
    call_id: String,
    function_name: String,
    arguments: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
struct SerializableToolResponse {
    call_id: String,
    content: String,
    success: bool,
}

#[derive(Serialize, Deserialize)]
struct SerializableToolRound {
    calls: Vec<SerializableToolCall>,
    responses: Vec<SerializableToolResponse>,
    reasoning_content: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct SerializableUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
    cached_tokens: Option<u64>,
}

/// Serializable snapshot of the tool loop's intermediate state.
#[derive(Serialize, Deserialize)]
pub(crate) struct ToolLoopCheckpoint {
    /// The user's original input that started this tool loop.
    pub user_input: String,
    /// Completed tool rounds so far.
    tool_history: Vec<SerializableToolRound>,
    /// Accumulated token usage.
    usage: SerializableUsage,
    /// 1-based round number of the last completed round.
    pub last_round: usize,
    /// Total tool calls executed so far.
    pub total_tool_calls: usize,
    /// Unique file paths read during the session.
    pub files_read: Vec<String>,
    /// Timestamp when the checkpoint was saved.
    pub saved_at: String,
}

impl ToolLoopCheckpoint {
    /// Create a checkpoint from the current tool loop state.
    pub fn new(
        user_input: &str,
        tool_history: &[ToolRound],
        usage: &TokenUsage,
        last_round: usize,
        total_tool_calls: usize,
        files_read: &HashSet<String>,
    ) -> Self {
        Self {
            user_input: user_input.to_string(),
            tool_history: tool_history.iter().map(|r| SerializableToolRound {
                calls: r.calls.iter().map(|c| SerializableToolCall {
                    call_id: c.call_id.clone(),
                    function_name: c.function_name.clone(),
                    arguments: c.arguments.clone(),
                }).collect(),
                responses: r.responses.iter().map(|r| SerializableToolResponse {
                    call_id: r.call_id.clone(),
                    content: r.content.clone(),
                    success: r.success,
                }).collect(),
                reasoning_content: r.reasoning_content.clone(),
            }).collect(),
            usage: SerializableUsage {
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                total_tokens: usage.total_tokens,
                cached_tokens: usage.cached_tokens,
            },
            last_round,
            total_tool_calls,
            files_read: files_read.iter().cloned().collect(),
            saved_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        }
    }

    /// Restore the tool history from this checkpoint.
    pub fn restore_tool_history(&self) -> Vec<ToolRound> {
        self.tool_history.iter().map(|r| ToolRound {
            calls: r.calls.iter().map(|c| ToolCall {
                call_id: c.call_id.clone(),
                function_name: c.function_name.clone(),
                arguments: c.arguments.clone(),
            }).collect(),
            responses: r.responses.iter().map(|r| ToolResponse {
                call_id: r.call_id.clone(),
                content: r.content.clone(),
                success: r.success,
            }).collect(),
            reasoning_content: r.reasoning_content.clone(),
        }).collect()
    }

    /// Restore the token usage from this checkpoint.
    pub fn restore_usage(&self) -> TokenUsage {
        TokenUsage {
            prompt_tokens: self.usage.prompt_tokens,
            completion_tokens: self.usage.completion_tokens,
            total_tokens: self.usage.total_tokens,
            cached_tokens: self.usage.cached_tokens,
        }
    }

    /// Restore the files_read set from this checkpoint.
    pub fn restore_files_read(&self) -> HashSet<String> {
        self.files_read.iter().cloned().collect()
    }

    /// Save the checkpoint to disk atomically.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create checkpoint directory: {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(self)
            .context("Failed to serialize tool loop checkpoint")?;
        atomic_write(path, json.as_bytes())
            .with_context(|| format!("Failed to write checkpoint to: {}", path.display()))?;
        tracing::info!(
            round = self.last_round,
            tool_calls = self.total_tool_calls,
            path = %path.display(),
            "Tool loop checkpoint saved"
        );
        Ok(())
    }

    /// Load a checkpoint from disk. Returns None if the file doesn't exist.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read checkpoint from: {}", path.display()))?;
        let checkpoint: Self = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse checkpoint from: {}", path.display()))?;
        tracing::info!(
            round = checkpoint.last_round,
            tool_calls = checkpoint.total_tool_calls,
            saved_at = %checkpoint.saved_at,
            "Tool loop checkpoint loaded"
        );
        Ok(Some(checkpoint))
    }

    /// Delete the checkpoint file (called after successful completion).
    pub fn clear(path: &Path) {
        if path.exists() {
            if let Err(e) = std::fs::remove_file(path) {
                tracing::debug!(error = %e, "Failed to remove checkpoint file");
            } else {
                tracing::debug!(path = %path.display(), "Checkpoint file cleared");
            }
        }
    }

    /// Return a human-readable summary of this checkpoint.
    pub fn summary(&self) -> String {
        let input_preview = if self.user_input.len() > 80 {
            // Find a char boundary at or before byte 80 to avoid panic
            // on multi-byte UTF-8 characters (e.g., CJK input).
            let mut end = 80;
            while end > 0 && !self.user_input.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}...", &self.user_input[..end])
        } else {
            self.user_input.clone()
        };
        format!(
            "Checkpoint from {} — round {}, {} tool calls, {} files read\n  Task: {}",
            self.saved_at,
            self.last_round,
            self.total_tool_calls,
            self.files_read.len(),
            input_preview,
        )
    }
}

/// Get the default checkpoint file path for a workspace.
pub(crate) fn checkpoint_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join("memory/tool_loop_checkpoint.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_checkpoint_roundtrip() {
        let mut files = HashSet::new();
        files.insert("/foo/bar.rs".to_string());
        files.insert("/baz/qux.rs".to_string());

        let usage = TokenUsage {
            prompt_tokens: Some(100),
            completion_tokens: Some(50),
            total_tokens: Some(150),
            cached_tokens: Some(20),
        };

        let tool_history = vec![ToolRound {
            calls: vec![ToolCall {
                call_id: "call-1".to_string(),
                function_name: "read_file".to_string(),
                arguments: serde_json::json!({"path": "/foo/bar.rs"}),
            }],
            responses: vec![ToolResponse::new("call-1", "file content here")],
            reasoning_content: Some("thinking...".to_string()),
        }];

        let cp = ToolLoopCheckpoint::new(
            "Fix the bug in auth.rs",
            &tool_history,
            &usage,
            3,
            5,
            &files,
        );

        // Verify roundtrip
        let restored_history = cp.restore_tool_history();
        assert_eq!(restored_history.len(), 1);
        assert_eq!(restored_history[0].calls[0].function_name, "read_file");
        assert_eq!(restored_history[0].responses[0].content, "file content here");
        assert_eq!(restored_history[0].reasoning_content, Some("thinking...".to_string()));

        let restored_usage = cp.restore_usage();
        assert_eq!(restored_usage.prompt_tokens, Some(100));
        assert_eq!(restored_usage.cached_tokens, Some(20));

        let restored_files = cp.restore_files_read();
        assert!(restored_files.contains("/foo/bar.rs"));
        assert!(restored_files.contains("/baz/qux.rs"));
        assert_eq!(cp.last_round, 3);
        assert_eq!(cp.total_tool_calls, 5);
    }

    #[test]
    fn test_checkpoint_summary() {
        let cp = ToolLoopCheckpoint::new(
            "A very long task description that should be truncated in the summary display for readability purposes and user experience",
            &[],
            &TokenUsage::default(),
            10,
            25,
            &HashSet::new(),
        );
        let summary = cp.summary();
        assert!(summary.contains("round 10"));
        assert!(summary.contains("25 tool calls"));
        assert!(summary.contains("..."));
    }

    #[test]
    fn test_checkpoint_save_load() {
        let dir = std::env::temp_dir().join("daedalus_checkpoint_test_sl");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("checkpoint.json");

        let cp = ToolLoopCheckpoint::new(
            "test task",
            &[],
            &TokenUsage::default(),
            1,
            0,
            &HashSet::new(),
        );
        cp.save(&path).unwrap();

        let loaded = ToolLoopCheckpoint::load(&path).unwrap().unwrap();
        assert_eq!(loaded.user_input, "test task");
        assert_eq!(loaded.last_round, 1);

        ToolLoopCheckpoint::clear(&path);
        assert!(!path.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_checkpoint_load_nonexistent() {
        let path = std::env::temp_dir().join("nonexistent_checkpoint_xyz.json");
        let result = ToolLoopCheckpoint::load(&path).unwrap();
        assert!(result.is_none());
    }
}
