use std::path::Path;

use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::fs;

use super::BuiltinTool;
use super::fs_utils::{
    format_size, get_optional_bool, get_optional_u64, get_required_string, resolve_path,
};

/// List the contents of a directory.
pub struct ListDirectoryTool;

#[async_trait]
impl BuiltinTool for ListDirectoryTool {
    fn name(&self) -> &str {
        "list_directory"
    }

    fn description(&self) -> &str {
        "List the contents of a directory. Returns file names, types (file/directory), \
         and sizes. Supports optional recursive listing."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The absolute or relative path to the directory to list."
                },
                "recursive": {
                    "type": "boolean",
                    "description": "If true, list contents recursively. Defaults to false."
                },
                "max_entries": {
                    "type": "integer",
                    "description": "Maximum number of entries to return. Defaults to 100."
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String> {
        let path_str = get_required_string(&arguments, "path")?;
        let recursive = get_optional_bool(&arguments, "recursive").unwrap_or(false);
        let max_entries = get_optional_u64(&arguments, "max_entries")
            .map(|v| v as usize)
            .unwrap_or(100);

        let path = resolve_path(&path_str)?;

        if !path.is_dir() {
            anyhow::bail!("'{}' is not a directory", path.display());
        }

        let mut entries = Vec::new();
        collect_entries(&path, &path, recursive, max_entries, &mut entries).await?;

        let mut result = format!("Directory: {}\n", path.display());
        result.push_str(&format!("Entries: {}\n\n", entries.len()));

        for entry in &entries {
            result.push_str(entry);
            result.push('\n');
        }

        if entries.len() >= max_entries {
            result.push_str(&format!(
                "\n... (output truncated at {} entries)\n",
                max_entries
            ));
        }

        Ok(result)
    }
}

/// Recursively collect directory entries.
async fn collect_entries(
    base: &Path,
    dir: &Path,
    recursive: bool,
    max_entries: usize,
    entries: &mut Vec<String>,
) -> Result<()> {
    let mut read_dir = fs::read_dir(dir)
        .await
        .with_context(|| format!("Failed to read directory: {}", dir.display()))?;

    while let Some(entry) = read_dir.next_entry().await? {
        if entries.len() >= max_entries {
            break;
        }

        let path = entry.path();
        let metadata = entry.metadata().await?;
        let relative = path
            .strip_prefix(base)
            .unwrap_or(&path)
            .to_string_lossy();

        if metadata.is_dir() {
            entries.push(format!("  📁 {}/", relative));
            if recursive {
                // Use Box::pin for recursive async call
                Box::pin(collect_entries(base, &path, true, max_entries, entries)).await?;
            }
        } else {
            let size = metadata.len();
            let size_str = format_size(size);
            entries.push(format!("  📄 {} ({})", relative, size_str));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_list_directory_tool_schema() {
        let tool = ListDirectoryTool;
        assert_eq!(tool.name(), "list_directory");
    }
}
