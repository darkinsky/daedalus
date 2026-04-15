use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;
use tokio::fs;

use super::BuiltinTool;
use super::fs_utils::{get_optional_u64, get_required_string, resolve_path, IGNORED_DIRS};

/// Search for files matching a pattern.
pub struct SearchFilesTool;

#[async_trait]
impl BuiltinTool for SearchFilesTool {
    fn name(&self) -> &str {
        "search_files"
    }

    fn description(&self) -> &str {
        "Search for files by name pattern in a directory tree. Returns matching file paths. \
         The pattern is matched against file names (case-insensitive substring match)."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The root directory to search in."
                },
                "pattern": {
                    "type": "string",
                    "description": "The search pattern to match against file names (case-insensitive substring match)."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return. Defaults to 50."
                }
            },
            "required": ["path", "pattern"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String> {
        let path_str = get_required_string(&arguments, "path")?;
        let pattern = get_required_string(&arguments, "pattern")?;
        let max_results = get_optional_u64(&arguments, "max_results")
            .map(|v| v as usize)
            .unwrap_or(50);

        let path = resolve_path(&path_str)?;

        if !path.is_dir() {
            anyhow::bail!("'{}' is not a directory", path.display());
        }

        let pattern_lower = pattern.to_lowercase();
        let mut results = Vec::new();
        search_recursive(&path, &pattern_lower, max_results, &mut results).await?;

        if results.is_empty() {
            return Ok(format!(
                "No files matching '{}' found in {}",
                pattern,
                path.display()
            ));
        }

        let mut output = format!(
            "Found {} file(s) matching '{}' in {}:\n\n",
            results.len(),
            pattern,
            path.display()
        );

        for result_path in &results {
            output.push_str(&format!("  {}\n", result_path));
        }

        if results.len() >= max_results {
            output.push_str(&format!(
                "\n... (results truncated at {})\n",
                max_results
            ));
        }

        Ok(output)
    }
}

/// Recursively search for files matching a pattern.
async fn search_recursive(
    dir: &Path,
    pattern: &str,
    max_results: usize,
    results: &mut Vec<String>,
) -> Result<()> {
    let mut read_dir = match fs::read_dir(dir).await {
        Ok(rd) => rd,
        Err(_) => return Ok(()), // Skip directories we can't read
    };

    while let Some(entry) = read_dir.next_entry().await? {
        if results.len() >= max_results {
            break;
        }

        let path = entry.path();
        let metadata = match entry.metadata().await {
            Ok(m) => m,
            Err(_) => continue, // Skip entries we can't stat
        };

        if metadata.is_dir() {
            // Skip hidden directories and common noise directories
            let dir_name = entry.file_name().to_string_lossy().to_string();
            if dir_name.starts_with('.')
                || IGNORED_DIRS.contains(&dir_name.as_str())
            {
                continue;
            }
            Box::pin(search_recursive(&path, pattern, max_results, results)).await?;
        } else {
            let file_name = entry.file_name().to_string_lossy().to_lowercase();
            if file_name.contains(pattern) {
                results.push(path.to_string_lossy().to_string());
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_search_files_tool_schema() {
        let tool = SearchFilesTool;
        assert_eq!(tool.name(), "search_files");
    }
}
