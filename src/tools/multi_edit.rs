//! Built-in tool for performing multiple edits on a single file.
//!
//! Applies a sequence of search-and-replace operations to a file in one
//! atomic operation. This is more efficient than multiple `edit_file` calls
//! and ensures all edits are applied together or not at all.

use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::fs;

use super::BuiltinTool;
use super::fs_utils::{get_required_string, resolve_path};

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

        // Read the file
        let mut content = fs::read_to_string(&path)
            .await
            .with_context(|| format!("Failed to read file: {}", path.display()))?;

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

            // Validate
            if old_string.is_empty() {
                anyhow::bail!("Edit #{}: old_string is empty. Use edit_file to create new files.", i + 1);
            }

            if old_string == new_string {
                anyhow::bail!("Edit #{}: old_string and new_string are identical.", i + 1);
            }

            if !content.contains(old_string) {
                anyhow::bail!(
                    "Edit #{}: old_string not found in {} (after applying previous edits). \
                     The text must match exactly. Earlier edits may have changed the content.",
                    i + 1,
                    path.display()
                );
            }

            // Apply replacement
            let count = if replace_all {
                let c = content.matches(old_string).count();
                content = content.replace(old_string, new_string);
                c
            } else {
                let match_count = content.matches(old_string).count();
                if match_count > 1 {
                    anyhow::bail!(
                        "Edit #{}: old_string matches {} locations. \
                         Add more context to uniquely identify the target, \
                         or set replace_all=true.",
                        i + 1,
                        match_count
                    );
                }
                content = content.replacen(old_string, new_string, 1);
                1
            };

            total_replacements += count;
            edit_results.push(format!("  Edit #{}: {} replacement(s)", i + 1, count));
        }

        // Write back atomically
        fs::write(&path, &content)
            .await
            .with_context(|| format!("Failed to write file: {}", path.display()))?;

        let lines = content.lines().count();
        let mut output = format!(
            "Successfully applied {} edit(s) with {} total replacement(s) to {} ({} lines):\n",
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

        let content = tokio::fs::read_to_string(&file).await.unwrap();
        assert_eq!(content, "xxx\nyyy\nzzz\n");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_multi_edit_chained_dependency() {
        // Edit 1 changes content that Edit 2 depends on
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
        // Edit 2 should fail, and the file should remain unchanged
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
}
