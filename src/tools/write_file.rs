use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::fs;

use super::BuiltinTool;
use super::fs_utils::{get_required_string, resolve_path};

/// Write content to a file, creating it if it doesn't exist.
pub struct WriteFileTool;

#[async_trait]
impl BuiltinTool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write content to a file. Creates the file (and parent directories) if they \
         don't exist. Overwrites the file if it already exists. Use with caution."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The absolute or relative path to the file to write."
                },
                "content": {
                    "type": "string",
                    "description": "The content to write to the file."
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String> {
        let path_str = get_required_string(&arguments, "path")?;
        let content = get_required_string(&arguments, "content")?;

        let path = resolve_path(&path_str)?;

        // Create parent directories if they don't exist
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("Failed to create directories: {}", parent.display()))?;
        }

        fs::write(&path, &content)
            .await
            .with_context(|| format!("Failed to write file: {}", path.display()))?;

        let bytes = content.len();
        let lines = content.lines().count();
        Ok(format!(
            "Successfully wrote {} bytes ({} lines) to {}",
            bytes,
            lines,
            path.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_write_file_tool_schema() {
        let tool = WriteFileTool;
        assert_eq!(tool.name(), "write_file");
        let schema = tool.input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::json!("path")));
        assert!(required.contains(&serde_json::json!("content")));
    }
}
