use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::fs;

use super::BuiltinTool;
use super::fs_utils::{format_size, get_required_string, resolve_path};

/// Get metadata about a file or directory.
pub struct GetFileInfoTool;

#[async_trait]
impl BuiltinTool for GetFileInfoTool {
    fn name(&self) -> &str {
        "get_file_info"
    }

    fn description(&self) -> &str {
        "Get detailed metadata about a file or directory, including size, \
         permissions, modification time, and type."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The absolute or relative path to the file or directory."
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String> {
        let path_str = get_required_string(&arguments, "path")?;
        let path = resolve_path(&path_str)?;

        let metadata = fs::metadata(&path)
            .await
            .with_context(|| format!("Failed to get metadata for: {}", path.display()))?;

        let file_type = if metadata.is_dir() {
            "directory"
        } else if metadata.is_file() {
            "file"
        } else if metadata.is_symlink() {
            "symlink"
        } else {
            "other"
        };

        let size = metadata.len();
        let size_str = format_size(size);

        let modified = metadata
            .modified()
            .ok()
            .and_then(|t| {
                let datetime: chrono::DateTime<chrono::Local> = t.into();
                Some(datetime.format("%Y-%m-%d %H:%M:%S").to_string())
            })
            .unwrap_or_else(|| "unknown".to_string());

        let readonly = metadata.permissions().readonly();

        let mut result = format!("Path: {}\n", path.display());
        result.push_str(&format!("Type: {}\n", file_type));
        result.push_str(&format!("Size: {} ({} bytes)\n", size_str, size));
        result.push_str(&format!("Modified: {}\n", modified));
        result.push_str(&format!("Read-only: {}\n", readonly));

        // For files, also show line count
        if metadata.is_file() {
            match fs::read_to_string(&path).await {
                Ok(content) => {
                    let line_count = content.lines().count();
                    result.push_str(&format!("Lines: {}\n", line_count));
                }
                Err(_) => {
                    result.push_str("Lines: (binary or unreadable)\n");
                }
            }
        }

        // For directories, show child count
        if metadata.is_dir() {
            match fs::read_dir(&path).await {
                Ok(mut rd) => {
                    let mut count = 0;
                    while rd.next_entry().await?.is_some() {
                        count += 1;
                    }
                    result.push_str(&format!("Children: {}\n", count));
                }
                Err(_) => {
                    result.push_str("Children: (unreadable)\n");
                }
            }
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_file_info_tool_schema() {
        let tool = GetFileInfoTool;
        assert_eq!(tool.name(), "get_file_info");
    }
}
