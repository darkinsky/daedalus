use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::fs;

use super::BuiltinTool;
use super::fs_utils::{get_required_string, get_optional_u64, resolve_path};

/// Read the contents of a file.
///
/// Supports optional line offset and limit for reading portions of large files.
pub struct ReadFileTool;

#[async_trait]
impl BuiltinTool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read the contents of a file. Supports optional offset and limit parameters \
         for reading specific line ranges from large files. Returns the file content \
         with line numbers."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The absolute or relative path to the file to read."
                },
                "offset": {
                    "type": "integer",
                    "description": "The line number to start reading from (1-based). If not specified, reads from the beginning."
                },
                "limit": {
                    "type": "integer",
                    "description": "The maximum number of lines to read. If not specified, reads the entire file."
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String> {
        let path_str = get_required_string(&arguments, "path")?;
        let offset = get_optional_u64(&arguments, "offset").map(|v| v as usize);
        let limit = get_optional_u64(&arguments, "limit").map(|v| v as usize);

        let path = resolve_path(&path_str)?;

        let content = fs::read_to_string(&path)
            .await
            .with_context(|| format!("Failed to read file: {}", path.display()))?;

        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();

        // Apply offset (1-based) and limit
        let start = offset.map(|o| o.saturating_sub(1)).unwrap_or(0);
        let end = limit
            .map(|l| (start + l).min(total_lines))
            .unwrap_or(total_lines);

        if start >= total_lines {
            return Ok(format!(
                "File has {} lines, but offset {} is beyond the end.",
                total_lines,
                start + 1
            ));
        }

        let mut result = String::new();
        for (idx, line) in lines[start..end].iter().enumerate() {
            let line_num = start + idx + 1;
            result.push_str(&format!("{:>4} | {}\n", line_num, line));
        }

        if end < total_lines {
            result.push_str(&format!(
                "\n... ({} more lines, {} total)\n",
                total_lines - end,
                total_lines
            ));
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_read_file_tool_schema() {
        let tool = ReadFileTool;
        assert_eq!(tool.name(), "read_file");
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["path"].is_object());
    }

    #[tokio::test]
    async fn test_to_openai_json() {
        let tool = ReadFileTool;
        let json = tool.to_openai_json();
        assert_eq!(json["type"], "function");
        assert_eq!(json["function"]["name"], "read_file");
        assert!(json["function"]["description"].as_str().unwrap().len() > 0);
        assert!(json["function"]["parameters"].is_object());
    }
}
