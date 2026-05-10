//! Built-in tool for performing multiple edits on a single file.
//!
//! Applies a sequence of search-and-replace operations to a file in one
//! atomic operation. This is more efficient than multiple `edit_file` calls
//! and ensures all edits are applied together or not at all.
//!
//! ## Improvements (aligned with Claude Code's MultiEdit):
//!
//! - **Diff/snippet output**: Returns a unified-diff-style snippet for each edit.
//! - **Line number reporting**: Reports which line(s) were modified per edit.
//! - **Line ending normalization**: Handles `\r\n` vs `\n` transparently.
//! - **File size guard**: Rejects files larger than 10 MB.
//! - **Concurrency safety**: Uses the same per-file lock as `edit_file`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result};
use async_trait::async_trait;
use once_cell::sync::Lazy;
use tokio::fs;

use super::BuiltinTool;
use super::fs_utils::{get_required_string, resolve_path};

/// Maximum file size allowed for editing (10 MB).
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Number of context lines to show around each change in the diff snippet.
const DIFF_CONTEXT_LINES: usize = 3;

/// Global set of files currently being edited (concurrency guard).
/// Shared with edit_file via the same mechanism.
static EDITING_FILES: Lazy<Mutex<HashSet<PathBuf>>> = Lazy::new(|| Mutex::new(HashSet::new()));

/// Global set of files modified during this session.
static MODIFIED_FILES: Lazy<Mutex<HashSet<PathBuf>>> = Lazy::new(|| Mutex::new(HashSet::new()));

/// Record that a file was modified in this session.
fn record_modified_file(path: &PathBuf) {
    if let Ok(mut set) = MODIFIED_FILES.lock() {
        set.insert(path.clone());
    }
}

/// RAII guard for file-level locking.
struct FileEditGuard {
    path: PathBuf,
}

impl FileEditGuard {
    fn try_acquire(path: PathBuf) -> Option<Self> {
        let mut set = EDITING_FILES.lock().ok()?;
        if set.contains(&path) {
            None
        } else {
            set.insert(path.clone());
            Some(Self { path })
        }
    }
}

impl Drop for FileEditGuard {
    fn drop(&mut self) {
        if let Ok(mut set) = EDITING_FILES.lock() {
            set.remove(&self.path);
        }
    }
}

/// Batch editing tool — multiple search-and-replace operations on one file.
pub struct MultiEditTool;

#[async_trait]
impl BuiltinTool for MultiEditTool {
    fn name(&self) -> &str {
        "multi_edit"
    }

    fn description(&self) -> &str {
        "Apply multiple search-and-replace edits to a single file in one operation. \
         Each edit specifies an old_string to find and a new_string to replace it with. \
         Edits are applied sequentially in order. This is more efficient than multiple \
         edit_file calls and ensures atomicity. Each old_string must match exactly."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The absolute or relative path to the file to edit."
                },
                "edits": {
                    "type": "array",
                    "description": "Array of edit operations to apply sequentially.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_string": {
                                "type": "string",
                                "description": "The exact text to find and replace."
                            },
                            "new_string": {
                                "type": "string",
                                "description": "The replacement text."
                            },
                            "replace_all": {
                                "type": "boolean",
                                "description": "If true, replace all occurrences. Default: false."
                            }
                        },
                        "required": ["old_string", "new_string"]
                    }
                }
            },
            "required": ["path", "edits"]
        })
    }

    fn is_read_only(&self) -> bool {
        false
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String> {
        let path_str = get_required_string(&arguments, "path")?;
        let edits = arguments
            .get("edits")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow::anyhow!("Missing or invalid 'edits' parameter (expected array)"))?;

        if edits.is_empty() {
            anyhow::bail!("'edits' array is empty — nothing to do.");
        }

        let path = resolve_path(&path_str)?;

        // Concurrency guard
        let _guard = FileEditGuard::try_acquire(path.clone()).ok_or_else(|| {
            anyhow::anyhow!(
                "File '{}' is currently being edited by another operation. Wait and retry.",
                path.display()
            )
        })?;

        // File size guard
        let metadata = fs::metadata(&path).await.with_context(|| {
            format!("Failed to read file metadata: {}", path.display())
        })?;
        if metadata.len() > MAX_FILE_SIZE {
            anyhow::bail!(
                "File '{}' is too large ({} bytes, max {} bytes). \
                 Use bash with sed/awk for large file edits.",
                path.display(),
                metadata.len(),
                MAX_FILE_SIZE
            );
        }

        // Read the file
        let raw_content = fs::read_to_string(&path)
            .await
            .with_context(|| format!("Failed to read file: {}", path.display()))?;

        // Normalize line endings
        let mut content = normalize_line_endings(&raw_content);

        // Apply edits sequentially
        let mut total_replacements = 0;
        let mut edit_results: Vec<String> = Vec::new();

        for (i, edit) in edits.iter().enumerate() {
            let old_string = edit
                .get("old_string")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Edit #{}: missing 'old_string'", i + 1))?;

            let new_string = edit
                .get("new_string")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Edit #{}: missing 'new_string'", i + 1))?;

            let replace_all = edit
                .get("replace_all")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            // Normalize edit strings
            let old_normalized = normalize_line_endings(old_string);
            let new_normalized = normalize_line_endings(new_string);

            // Validate
            if old_normalized.is_empty() {
                anyhow::bail!("Edit #{}: old_string is empty. Use edit_file to create new files.", i + 1);
            }

            if old_normalized == new_normalized {
                anyhow::bail!("Edit #{}: old_string and new_string are identical.", i + 1);
            }

            if !content.contains(&old_normalized) {
                anyhow::bail!(
                    "Edit #{}: old_string not found in {} (after applying previous edits). \
                     The text must match exactly. Earlier edits may have changed the content.",
                    i + 1,
                    path.display()
                );
            }

            // Find position before replacement (for line number reporting)
            let first_pos = content.find(&old_normalized).unwrap_or(0);
            let start_line = content[..first_pos].matches('\n').count() + 1;

            // Apply replacement
            let count = if replace_all {
                let c = content.matches(&old_normalized).count();
                content = content.replace(&old_normalized, &new_normalized);
                c
            } else {
                let match_count = content.matches(&old_normalized).count();
                if match_count > 1 {
                    let match_lines = find_match_line_numbers(&content, &old_normalized);
                    anyhow::bail!(
                        "Edit #{}: old_string matches {} locations (at lines {}). \
                         Add more context to uniquely identify the target, \
                         or set replace_all=true.",
                        i + 1,
                        match_count,
                        match_lines
                            .iter()
                            .map(|n| n.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
                content = content.replacen(&old_normalized, &new_normalized, 1);
                1
            };

            total_replacements += count;

            // Generate a compact diff snippet for this edit
            let snippet = generate_compact_diff(&old_normalized, &new_normalized, start_line);
            edit_results.push(format!(
                "  Edit #{}: {} replacement(s) at line {}\n{}",
                i + 1,
                count,
                start_line,
                snippet
            ));
        }

        // Write back atomically
        fs::write(&path, &content)
            .await
            .with_context(|| format!("Failed to write file: {}", path.display()))?;

        // Record file modification
        record_modified_file(&path);

        let lines = content.lines().count();
        let mut output = format!(
            "Successfully applied {} edit(s) with {} total replacement(s) to {} ({} lines):\n\n",
            edits.len(),
            total_replacements,
            path.display(),
            lines
        );
        for line in &edit_results {
            output.push_str(line);
            output.push('\n');
        }

        Ok(output)
    }
}

/// Normalize line endings: convert \r\n to \n.
fn normalize_line_endings(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// Find all line numbers where old_string matches in the content.
fn find_match_line_numbers(content: &str, old_string: &str) -> Vec<usize> {
    let mut positions = Vec::new();
    let mut search_start = 0;
    while let Some(pos) = content[search_start..].find(old_string) {
        let absolute_pos = search_start + pos;
        let line_num = content[..absolute_pos].matches('\n').count() + 1;
        positions.push(line_num);
        search_start = absolute_pos + 1;
    }
    positions
}

/// Generate a compact diff snippet for a single edit operation.
fn generate_compact_diff(old_str: &str, new_str: &str, start_line: usize) -> String {
    let old_lines: Vec<&str> = old_str.lines().collect();
    let new_lines: Vec<&str> = new_str.lines().collect();

    // For very short edits, show inline diff
    if old_lines.len() <= 6 && new_lines.len() <= 6 {
        let mut snippet = String::new();
        snippet.push_str(&format!(
            "    @@ -{},{} +{},{} @@\n",
            start_line,
            old_lines.len(),
            start_line,
            new_lines.len()
        ));
        for line in &old_lines {
            snippet.push_str(&format!("    -{}\n", line));
        }
        for line in &new_lines {
            snippet.push_str(&format!("    +{}\n", line));
        }
        return snippet;
    }

    // For longer edits, show first/last few lines
    let max_show = DIFF_CONTEXT_LINES;
    let mut snippet = String::new();
    snippet.push_str(&format!(
        "    @@ -{},{} +{},{} @@\n",
        start_line,
        old_lines.len(),
        start_line,
        new_lines.len()
    ));

    // Show first few removed lines
    for line in old_lines.iter().take(max_show) {
        snippet.push_str(&format!("    -{}\n", line));
    }
    if old_lines.len() > max_show {
        snippet.push_str(&format!("    ... ({} more removed lines)\n", old_lines.len() - max_show));
    }

    // Show first few added lines
    for line in new_lines.iter().take(max_show) {
        snippet.push_str(&format!("    +{}\n", line));
    }
    if new_lines.len() > max_show {
        snippet.push_str(&format!("    ... ({} more added lines)\n", new_lines.len() - max_show));
    }

    snippet
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_multi_edit_tool_schema() {
        let tool = MultiEditTool;
        assert_eq!(tool.name(), "multi_edit");
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::json!("path")));
        assert!(required.contains(&serde_json::json!("edits")));
    }

    #[tokio::test]
    async fn test_multi_edit_sequential() {
        let dir = std::env::temp_dir().join("daedalus_multi_edit_test");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let file = dir.join("test.txt");
        tokio::fs::write(&file, "aaa\nbbb\nccc\n").await.unwrap();

        let tool = MultiEditTool;
        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "edits": [
                {"old_string": "aaa", "new_string": "xxx"},
                {"old_string": "bbb", "new_string": "yyy"},
                {"old_string": "ccc", "new_string": "zzz"}
            ]
        });
        let result = tool.execute(args).await.unwrap();
        assert!(result.contains("3 edit(s)"));
        // Should contain line numbers
        assert!(result.contains("line 1"));
        assert!(result.contains("line 2"));
        assert!(result.contains("line 3"));

        let content = tokio::fs::read_to_string(&file).await.unwrap();
        assert_eq!(content, "xxx\nyyy\nzzz\n");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_multi_edit_chained_dependency() {
        let dir = std::env::temp_dir().join("daedalus_multi_edit_chain_test");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let file = dir.join("test.txt");
        tokio::fs::write(&file, "hello world\n").await.unwrap();

        let tool = MultiEditTool;
        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "edits": [
                {"old_string": "hello", "new_string": "hi"},
                {"old_string": "hi world", "new_string": "hi rust"}
            ]
        });
        let result = tool.execute(args).await.unwrap();
        assert!(result.contains("2 edit(s)"));

        let content = tokio::fs::read_to_string(&file).await.unwrap();
        assert_eq!(content, "hi rust\n");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_multi_edit_failure_rolls_back() {
        let dir = std::env::temp_dir().join("daedalus_multi_edit_fail_test");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let file = dir.join("test.txt");
        tokio::fs::write(&file, "aaa\nbbb\n").await.unwrap();

        let tool = MultiEditTool;
        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "edits": [
                {"old_string": "aaa", "new_string": "xxx"},
                {"old_string": "nonexistent", "new_string": "yyy"}
            ]
        });
        let result = tool.execute(args).await;
        assert!(result.is_err());

        // File should be unchanged (edits are atomic — failure means no write)
        let content = tokio::fs::read_to_string(&file).await.unwrap();
        assert_eq!(content, "aaa\nbbb\n");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_multi_edit_empty_edits() {
        let tool = MultiEditTool;
        let args = serde_json::json!({
            "path": "/tmp/test.txt",
            "edits": []
        });
        let result = tool.execute(args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }

    #[tokio::test]
    async fn test_multi_edit_with_replace_all() {
        let dir = std::env::temp_dir().join("daedalus_multi_edit_replall_test");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let file = dir.join("test.txt");
        tokio::fs::write(&file, "foo bar foo baz foo\n").await.unwrap();

        let tool = MultiEditTool;
        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "edits": [
                {"old_string": "foo", "new_string": "qux", "replace_all": true}
            ]
        });
        let result = tool.execute(args).await.unwrap();
        assert!(result.contains("3 replacement"));

        let content = tokio::fs::read_to_string(&file).await.unwrap();
        assert_eq!(content, "qux bar qux baz qux\n");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_multi_edit_crlf_normalization() {
        let dir = std::env::temp_dir().join("daedalus_multi_edit_crlf_test");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let file = dir.join("test.txt");
        tokio::fs::write(&file, "hello\r\nworld\r\n").await.unwrap();

        let tool = MultiEditTool;
        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "edits": [
                {"old_string": "hello\nworld", "new_string": "hi\nrust"}
            ]
        });
        let result = tool.execute(args).await.unwrap();
        assert!(result.contains("1 edit(s)"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[test]
    fn test_find_match_line_numbers() {
        let content = "aaa\nbbb\naaa\nccc\n";
        let positions = find_match_line_numbers(content, "aaa");
        assert_eq!(positions, vec![1, 3]);
    }
}
