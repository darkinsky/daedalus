//! Built-in tool for precise file editing via search-and-replace.
//!
//! Provides a safe, targeted editing mechanism that replaces specific text
//! in a file without rewriting the entire content. This is the primary
//! editing tool for LLMs — much safer than `write_file` for modifications.

use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::fs;

use super::BuiltinTool;
use super::fs_utils::{get_optional_bool, get_required_string, resolve_path};

/// Precise file editing via search-and-replace.
pub struct EditFileTool;

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

        // Read existing file
        let content = fs::read_to_string(&path)
            .await
            .with_context(|| format!("Failed to read file: {}", path.display()))?;

        // Validate: old_string must exist in the file
        if !content.contains(&old_string) {
            // Provide helpful diagnostics
            let suggestion = find_similar_match(&content, &old_string);
            let mut msg = format!(
                "old_string not found in {}. The text must match exactly (including whitespace and indentation).",
                path.display()
            );
            if let Some(hint) = suggestion {
                msg.push_str(&format!("\n\nDid you mean:\n{}", hint));
            }
            anyhow::bail!(msg);
        }

        // Validate: old_string and new_string must differ
        if old_string == new_string {
            anyhow::bail!("old_string and new_string are identical — no change needed.");
        }

        // Perform replacement
        let (new_content, count) = if replace_all {
            let count = content.matches(&old_string).count();
            (content.replace(&old_string, &new_string), count)
        } else {
            // Ensure unique match for single replacement
            let match_count = content.matches(&old_string).count();
            if match_count > 1 {
                anyhow::bail!(
                    "old_string matches {} locations in {}. \
                     Add more surrounding context to uniquely identify the target, \
                     or use replace_all=true to replace all occurrences.",
                    match_count,
                    path.display()
                );
            }
            (content.replacen(&old_string, &new_string, 1), 1)
        };

        // Write back
        fs::write(&path, &new_content)
            .await
            .with_context(|| format!("Failed to write file: {}", path.display()))?;

        let lines_changed = new_content.lines().count();
        Ok(format!(
            "Successfully replaced {} occurrence(s) in {} ({} lines total)",
            count,
            path.display(),
            lines_changed
        ))
    }
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

    fs::write(path, content)
        .await
        .with_context(|| format!("Failed to create file: {}", path.display()))?;

    let lines = content.lines().count();
    Ok(format!(
        "Created new file {} ({} lines)",
        path.display(),
        lines
    ))
}

/// Try to find a similar match in the content for better error messages.
///
/// Looks for lines that partially match the first line of old_string.
fn find_similar_match(content: &str, old_string: &str) -> Option<String> {
    let first_line = old_string.lines().next()?.trim();
    if first_line.is_empty() {
        return None;
    }

    // Find lines that contain the trimmed first line
    let matches: Vec<(usize, &str)> = content
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains(first_line))
        .take(3)
        .collect();

    if matches.is_empty() {
        return None;
    }

    let mut hint = String::new();
    for (line_num, line) in matches {
        hint.push_str(&format!("  Line {}: {}\n", line_num + 1, line));
    }
    Some(hint)
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
        assert!(result.unwrap_err().to_string().contains("2 locations"));

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
    async fn test_find_similar_match() {
        let content = "    fn hello_world() {\n        println!(\"hi\");\n    }\n";
        let old_string = "fn hello_world() {";
        let hint = find_similar_match(content, old_string);
        assert!(hint.is_some());
        assert!(hint.unwrap().contains("hello_world"));
    }
}
