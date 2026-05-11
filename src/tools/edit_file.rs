//! Built-in tool for precise file editing via search-and-replace.
//!
//! Provides a safe, targeted editing mechanism that replaces specific text
//! in a file without rewriting the entire content. This is the primary
//! editing tool for LLMs — much safer than `write_file` for modifications.
//!
//! ## Key features (aligned with Claude Code's FileEditTool):
//!
//! - **Diff/snippet output**: Returns a unified-diff-style snippet showing
//!   the change with surrounding context lines, so the LLM can verify edits.
//! - **Line number reporting**: Reports which line(s) were modified.
//! - **Enhanced diagnostics**: When `old_string` is not found, provides
//!   whitespace-aware hints and fuzzy suggestions.
//! - **Line ending normalization**: Handles `\r\n` vs `\n` transparently.
//! - **File size guard**: Rejects files larger than 10 MB.
//! - **Concurrency safety**: Per-file locking prevents race conditions.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result};
use async_trait::async_trait;
use once_cell::sync::Lazy;
use tokio::fs;

use super::BuiltinTool;
use super::fs_utils::{get_optional_bool, get_required_string, resolve_path, EDITING_FILES};

/// Maximum file size allowed for editing (10 MB).
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Number of context lines to show around each change in the diff snippet.
const DIFF_CONTEXT_LINES: usize = 3;

/// Global set of files modified during this session (file history tracking).
///
/// Used for context management — knowing which files are "dirty".
static MODIFIED_FILES: Lazy<Mutex<HashSet<PathBuf>>> = Lazy::new(|| Mutex::new(HashSet::new()));
/// Precise file editing via search-and-replace.
pub struct EditFileTool;

/// Record that a file was modified in this session.
fn record_modified_file(path: &PathBuf) {
    if let Ok(mut set) = MODIFIED_FILES.lock() {
        set.insert(path.clone());
    }
}

/// Get the set of files modified in this session.
#[allow(dead_code)]
pub fn get_modified_files() -> HashSet<PathBuf> {
    MODIFIED_FILES
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default()
}

/// RAII guard for file-level locking.
struct FileEditGuard {
    path: PathBuf,
}

impl FileEditGuard {
    /// Attempt to acquire an edit lock on the given file.
    /// Returns `None` if the file is already being edited.
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

#[async_trait]
impl BuiltinTool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Edit a file by replacing specific text with new text (search-and-replace). \
         This is safer than write_file because it only modifies the targeted section. \
         The old_string must match exactly (including whitespace and indentation). \
         If old_string is empty and the file doesn't exist, creates a new file with new_string. \
         Use replace_all=true to replace all occurrences instead of just the first one."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The absolute or relative path to the file to edit."
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact text to find and replace. Must match the file content exactly, \
                                    including whitespace and indentation. If empty, creates a new file."
                },
                "new_string": {
                    "type": "string",
                    "description": "The replacement text. If empty, the old_string is deleted."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "If true, replace all occurrences of old_string. Default: false (replace first occurrence only)."
                }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    fn is_read_only(&self) -> bool {
        false
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String> {
        let path_str = get_required_string(&arguments, "path")?;
        let old_string = get_required_string(&arguments, "old_string")?;
        let new_string = get_required_string(&arguments, "new_string")?;
        let replace_all = get_optional_bool(&arguments, "replace_all").unwrap_or(false);

        let path = resolve_path(&path_str)?;

        // Special case: empty old_string means create a new file
        if old_string.is_empty() {
            return create_new_file(&path, &new_string).await;
        }

        // Concurrency guard: prevent parallel edits to the same file
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

        // Read existing file
        let raw_content = fs::read_to_string(&path)
            .await
            .with_context(|| format!("Failed to read file: {}", path.display()))?;

        // Normalize line endings for matching (handle \r\n transparently)
        let content = normalize_line_endings(&raw_content);
        let old_normalized = normalize_line_endings(&old_string);
        let new_normalized = normalize_line_endings(&new_string);

        // Validate: old_string must exist in the file
        if !content.contains(&old_normalized) {
            let suggestion = find_similar_match(&content, &old_normalized);
            let mut msg = format!(
                "old_string not found in {}. The text must match exactly \
                 (including whitespace and indentation).",
                path.display()
            );
            if let Some(hint) = suggestion {
                msg.push_str(&format!("\n\n{}", hint));
            }
            anyhow::bail!(msg);
        }

        // Validate: old_string and new_string must differ
        if old_normalized == new_normalized {
            anyhow::bail!("old_string and new_string are identical — no change needed.");
        }

        // Perform replacement
        let (new_content, count, first_match_pos) = if replace_all {
            let count = content.matches(&old_normalized).count();
            let first_pos = content.find(&old_normalized).unwrap_or(0);
            (
                content.replace(&old_normalized, &new_normalized),
                count,
                first_pos,
            )
        } else {
            // Ensure unique match for single replacement
            let match_count = content.matches(&old_normalized).count();
            if match_count > 1 {
                // Provide line numbers of all matches to help the LLM
                let match_lines = find_match_line_numbers(&content, &old_normalized);
                anyhow::bail!(
                    "old_string matches {} locations in {} (at lines {}). \
                     Add more surrounding context to uniquely identify the target, \
                     or use replace_all=true to replace all occurrences.",
                    match_count,
                    path.display(),
                    match_lines
                        .iter()
                        .map(|n| n.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            let first_pos = content.find(&old_normalized).unwrap_or(0);
            (
                content.replacen(&old_normalized, &new_normalized, 1),
                1,
                first_pos,
            )
        };

        // Write back
        fs::write(&path, &new_content)
            .await
            .with_context(|| format!("Failed to write file: {}", path.display()))?;

        // Record file modification
        record_modified_file(&path);

        // Calculate line number of the first replacement
        let start_line = content[..first_match_pos].matches('\n').count() + 1;
        let old_line_count = old_normalized.matches('\n').count() + 1;
        let new_line_count = new_normalized.matches('\n').count() + 1;

        // Generate diff snippet
        let snippet = generate_diff_snippet(
            &content,
            &new_content,
            first_match_pos,
            &old_normalized,
            &new_normalized,
        );

        let total_lines = new_content.lines().count();
        let mut output = format!(
            "Successfully replaced {} occurrence(s) in {} (line {}, {} total lines)\n",
            count,
            path.display(),
            start_line,
            total_lines
        );

        if old_line_count != new_line_count {
            output.push_str(&format!(
                "Lines changed: {} → {} (delta: {:+})\n",
                old_line_count,
                new_line_count,
                new_line_count as i64 - old_line_count as i64
            ));
        }

        output.push('\n');
        output.push_str(&snippet);

        Ok(output)
    }
}

/// Normalize line endings: convert \r\n to \n.
fn normalize_line_endings(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// Generate a unified-diff-style snippet showing the change with context.
fn generate_diff_snippet(
    old_content: &str,
    new_content: &str,
    match_pos: usize,
    old_str: &str,
    new_str: &str,
) -> String {
    let old_lines: Vec<&str> = old_content.lines().collect();
    let new_lines: Vec<&str> = new_content.lines().collect();

    // Find the line number where the change starts
    let change_start_line = old_content[..match_pos].matches('\n').count();
    let old_line_count = old_str.matches('\n').count() + 1;
    let new_line_count = new_str.matches('\n').count() + 1;

    // Calculate context range
    let ctx_start = change_start_line.saturating_sub(DIFF_CONTEXT_LINES);
    let ctx_end_old = (change_start_line + old_line_count + DIFF_CONTEXT_LINES).min(old_lines.len());
    let ctx_end_new = (change_start_line + new_line_count + DIFF_CONTEXT_LINES).min(new_lines.len());

    let mut snippet = String::new();

    // Header
    snippet.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        ctx_start + 1,
        ctx_end_old - ctx_start,
        ctx_start + 1,
        ctx_end_new - ctx_start,
    ));

    // Leading context
    for i in ctx_start..change_start_line {
        if i < old_lines.len() {
            snippet.push_str(&format!(" {}\n", old_lines[i]));
        }
    }

    // Removed lines
    for i in change_start_line..(change_start_line + old_line_count) {
        if i < old_lines.len() {
            snippet.push_str(&format!("-{}\n", old_lines[i]));
        }
    }

    // Added lines
    for i in change_start_line..(change_start_line + new_line_count) {
        if i < new_lines.len() {
            snippet.push_str(&format!("+{}\n", new_lines[i]));
        }
    }

    // Trailing context
    let trailing_start_new = change_start_line + new_line_count;
    let trailing_count = DIFF_CONTEXT_LINES.min(
        new_lines.len().saturating_sub(trailing_start_new)
    );
    for i in 0..trailing_count {
        let new_idx = trailing_start_new + i;
        if new_idx < new_lines.len() {
            snippet.push_str(&format!(" {}\n", new_lines[new_idx]));
        }
    }

    snippet
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

/// Create a new file with the given content.
async fn create_new_file(path: &std::path::Path, content: &str) -> Result<String> {
    if path.exists() {
        anyhow::bail!(
            "File already exists: {}. Use a non-empty old_string to edit existing files.",
            path.display()
        );
    }

    // Create parent directories
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("Failed to create directories: {}", parent.display()))?;
    }

    let normalized = normalize_line_endings(content);
    fs::write(path, &normalized)
        .await
        .with_context(|| format!("Failed to create file: {}", path.display()))?;

    // Record file modification
    record_modified_file(&path.to_path_buf());

    let lines = normalized.lines().count();
    Ok(format!(
        "Created new file {} ({} lines)",
        path.display(),
        lines
    ))
}

/// Try to find a similar match in the content for better error messages.
///
/// Enhanced diagnostics:
/// 1. Check if the text exists with different whitespace/line-endings
/// 2. Look for lines that partially match the first line of old_string
/// 3. Report line numbers for potential matches
fn find_similar_match(content: &str, old_string: &str) -> Option<String> {
    let mut hints: Vec<String> = Vec::new();

    // Hint 1: Check if trimmed version matches (whitespace issue)
    let trimmed_old: String = old_string
        .lines()
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join("\n");
    let trimmed_content: String = content
        .lines()
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join("\n");
    if trimmed_content.contains(&trimmed_old) {
        hints.push(
            "Hint: The text exists but with different leading/trailing whitespace or indentation. \
             Check spaces vs tabs and indentation level."
                .to_string(),
        );
    }

    // Hint 2: Check for tab vs space issues
    if old_string.contains('\t') && !content.contains('\t') {
        hints.push(
            "Hint: old_string contains tabs but the file uses spaces for indentation.".to_string(),
        );
    } else if !old_string.contains('\t') && content.contains('\t') {
        hints.push(
            "Hint: old_string uses spaces but the file uses tabs for indentation.".to_string(),
        );
    }

    // Hint 3: Find lines that partially match the first line
    let first_line = old_string.lines().next().unwrap_or("").trim();
    if !first_line.is_empty() && first_line.len() > 3 {
        let matches: Vec<(usize, &str)> = content
            .lines()
            .enumerate()
            .filter(|(_, line)| line.contains(first_line))
            .take(5)
            .collect();

        if !matches.is_empty() {
            let mut partial = String::from("Possible matches found:\n");
            for (line_num, line) in matches {
                partial.push_str(&format!("  Line {}: {}\n", line_num + 1, line.trim()));
            }
            hints.push(partial);
        } else {
            // Try a substring of the first line (first 20 chars)
            let substr = &first_line[..first_line.len().min(30)];
            let matches: Vec<(usize, &str)> = content
                .lines()
                .enumerate()
                .filter(|(_, line)| line.contains(substr))
                .take(3)
                .collect();

            if !matches.is_empty() {
                let mut partial = String::from("Partial matches (substring) found:\n");
                for (line_num, line) in matches {
                    partial.push_str(&format!("  Line {}: {}\n", line_num + 1, line.trim()));
                }
                hints.push(partial);
            }
        }
    }

    if hints.is_empty() {
        None
    } else {
        Some(hints.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_edit_file_tool_schema() {
        let tool = EditFileTool;
        assert_eq!(tool.name(), "edit_file");
        let schema = tool.input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::json!("path")));
        assert!(required.contains(&serde_json::json!("old_string")));
        assert!(required.contains(&serde_json::json!("new_string")));
    }

    #[tokio::test]
    async fn test_edit_replace_single() {
        let dir = std::env::temp_dir().join("daedalus_edit_test_single");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let file = dir.join("test.txt");
        tokio::fs::write(&file, "hello world\nfoo bar\n").await.unwrap();

        let tool = EditFileTool;
        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "old_string": "hello world",
            "new_string": "hello rust"
        });
        let result = tool.execute(args).await.unwrap();
        assert!(result.contains("1 occurrence"));
        assert!(result.contains("line 1"));
        // Should contain diff snippet
        assert!(result.contains("-hello world"));
        assert!(result.contains("+hello rust"));

        let content = tokio::fs::read_to_string(&file).await.unwrap();
        assert_eq!(content, "hello rust\nfoo bar\n");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_edit_replace_all() {
        let dir = std::env::temp_dir().join("daedalus_edit_test_all");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let file = dir.join("test.txt");
        tokio::fs::write(&file, "aaa bbb aaa ccc aaa\n").await.unwrap();

        let tool = EditFileTool;
        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "old_string": "aaa",
            "new_string": "xxx",
            "replace_all": true
        });
        let result = tool.execute(args).await.unwrap();
        assert!(result.contains("3 occurrence"));

        let content = tokio::fs::read_to_string(&file).await.unwrap();
        assert_eq!(content, "xxx bbb xxx ccc xxx\n");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_edit_not_found() {
        let dir = std::env::temp_dir().join("daedalus_edit_test_notfound");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let file = dir.join("test.txt");
        tokio::fs::write(&file, "hello world\n").await.unwrap();

        let tool = EditFileTool;
        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "old_string": "nonexistent text",
            "new_string": "replacement"
        });
        let result = tool.execute(args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_edit_ambiguous_match() {
        let dir = std::env::temp_dir().join("daedalus_edit_test_ambiguous");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let file = dir.join("test.txt");
        tokio::fs::write(&file, "foo\nbar\nfoo\n").await.unwrap();

        let tool = EditFileTool;
        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "old_string": "foo",
            "new_string": "baz"
        });
        let result = tool.execute(args).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("2 locations"));
        // Should report line numbers
        assert!(err_msg.contains("lines"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_edit_create_new_file() {
        let dir = std::env::temp_dir().join("daedalus_edit_test_create");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let file = dir.join("new_file.txt");

        let tool = EditFileTool;
        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "old_string": "",
            "new_string": "brand new content\n"
        });
        let result = tool.execute(args).await.unwrap();
        assert!(result.contains("Created new file"));

        let content = tokio::fs::read_to_string(&file).await.unwrap();
        assert_eq!(content, "brand new content\n");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_edit_identical_strings() {
        let dir = std::env::temp_dir().join("daedalus_edit_test_identical");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let file = dir.join("test.txt");
        tokio::fs::write(&file, "hello world\n").await.unwrap();

        let tool = EditFileTool;
        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "old_string": "hello world",
            "new_string": "hello world"
        });
        let result = tool.execute(args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("identical"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_edit_crlf_normalization() {
        let dir = std::env::temp_dir().join("daedalus_edit_test_crlf");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let file = dir.join("test.txt");
        // File has \r\n line endings
        tokio::fs::write(&file, "hello world\r\nfoo bar\r\n").await.unwrap();

        let tool = EditFileTool;
        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            // LLM sends \n line endings
            "old_string": "hello world\nfoo bar",
            "new_string": "hello rust\nfoo baz"
        });
        let result = tool.execute(args).await.unwrap();
        assert!(result.contains("1 occurrence"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_find_similar_match_whitespace() {
        let content = "    fn hello_world() {\n        println!(\"hi\");\n    }\n";
        let old_string = "fn hello_world() {\n    println!(\"hi\");\n}";
        let hint = find_similar_match(content, old_string);
        assert!(hint.is_some());
        let hint_text = hint.unwrap();
        assert!(hint_text.contains("whitespace") || hint_text.contains("indentation"));
    }

    #[tokio::test]
    async fn test_find_similar_match_partial() {
        let content = "fn hello_world() {\n    println!(\"hi\");\n}\n";
        let old_string = "fn hello_world() {\n    println!(\"bye\");\n}";
        let hint = find_similar_match(content, old_string);
        assert!(hint.is_some());
        assert!(hint.unwrap().contains("hello_world"));
    }

    #[test]
    fn test_find_match_line_numbers() {
        let content = "aaa\nbbb\naaa\nccc\naaa\n";
        let positions = find_match_line_numbers(content, "aaa");
        assert_eq!(positions, vec![1, 3, 5]);
    }

    #[test]
    fn test_normalize_line_endings() {
        assert_eq!(normalize_line_endings("a\r\nb\r\n"), "a\nb\n");
        assert_eq!(normalize_line_endings("a\nb\n"), "a\nb\n");
        assert_eq!(normalize_line_endings("no newlines"), "no newlines");
    }

    #[test]
    fn test_generate_diff_snippet() {
        let old = "line1\nline2\nline3\nline4\nline5\n";
        let new_str = "changed";
        let old_str = "line3";
        let new_content = old.replacen(old_str, new_str, 1);
        let pos = old.find(old_str).unwrap();
        let snippet = generate_diff_snippet(old, &new_content, pos, old_str, new_str);
        assert!(snippet.contains("-line3"));
        assert!(snippet.contains("+changed"));
        assert!(snippet.contains(" line2")); // context line
    }
}
