use anyhow::{Context, Result};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use tokio::fs;

use super::BuiltinTool;

// ── Helper functions ──

/// Resolve a file path to an absolute path.
///
/// If the path is already absolute, it is returned as-is.
/// If the path is relative, it is resolved against the current working directory.
fn resolve_path(path_str: &str) -> Result<PathBuf> {
    let path = Path::new(path_str);
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        let cwd = std::env::current_dir().context("Failed to get current working directory")?;
        Ok(cwd.join(path))
    }
}

/// Extract a required string parameter from JSON arguments.
fn get_required_string(args: &serde_json::Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("Missing required parameter: '{}'", key))
}

/// Extract an optional string parameter from JSON arguments.
#[allow(dead_code)]
fn get_optional_string(args: &serde_json::Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

/// Extract an optional integer parameter from JSON arguments.
fn get_optional_u64(args: &serde_json::Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| v.as_u64())
}

/// Extract an optional boolean parameter from JSON arguments.
fn get_optional_bool(args: &serde_json::Value, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| v.as_bool())
}

// ── read_file ──

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

// ── write_file ──

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

// ── list_directory ──

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

/// Format a file size in human-readable form.
fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

// ── search_files ──

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

/// Directory names to skip during recursive file search.
///
/// These are common noise directories that rarely contain user-relevant files
/// and can significantly slow down searches if traversed.
const IGNORED_DIRS: &[&str] = &["node_modules", "target", "__pycache__", ".git"];

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

// ── get_file_info ──

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

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(1048576), "1.0 MB");
        assert_eq!(format_size(1073741824), "1.0 GB");
    }

    #[test]
    fn test_resolve_path_absolute() {
        let result = resolve_path("/tmp/test.txt").unwrap();
        assert_eq!(result, PathBuf::from("/tmp/test.txt"));
    }

    #[test]
    fn test_resolve_path_relative() {
        let result = resolve_path("test.txt").unwrap();
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(result, cwd.join("test.txt"));
    }

    #[test]
    fn test_get_required_string() {
        let args = serde_json::json!({"path": "/tmp/test.txt"});
        assert_eq!(
            get_required_string(&args, "path").unwrap(),
            "/tmp/test.txt"
        );
        assert!(get_required_string(&args, "missing").is_err());
    }

    #[test]
    fn test_get_optional_string() {
        let args = serde_json::json!({"path": "/tmp/test.txt"});
        assert_eq!(
            get_optional_string(&args, "path"),
            Some("/tmp/test.txt".to_string())
        );
        assert_eq!(get_optional_string(&args, "missing"), None);
    }

    #[test]
    fn test_get_optional_u64() {
        let args = serde_json::json!({"limit": 10});
        assert_eq!(get_optional_u64(&args, "limit"), Some(10));
        assert_eq!(get_optional_u64(&args, "missing"), None);
    }

    #[test]
    fn test_get_optional_bool() {
        let args = serde_json::json!({"recursive": true});
        assert_eq!(get_optional_bool(&args, "recursive"), Some(true));
        assert_eq!(get_optional_bool(&args, "missing"), None);
    }

    #[tokio::test]
    async fn test_read_file_tool_schema() {
        let tool = ReadFileTool;
        assert_eq!(tool.name(), "read_file");
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["path"].is_object());
    }

    #[tokio::test]
    async fn test_write_file_tool_schema() {
        let tool = WriteFileTool;
        assert_eq!(tool.name(), "write_file");
        let schema = tool.input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::json!("path")));
        assert!(required.contains(&serde_json::json!("content")));
    }

    #[tokio::test]
    async fn test_list_directory_tool_schema() {
        let tool = ListDirectoryTool;
        assert_eq!(tool.name(), "list_directory");
    }

    #[tokio::test]
    async fn test_search_files_tool_schema() {
        let tool = SearchFilesTool;
        assert_eq!(tool.name(), "search_files");
    }

    #[tokio::test]
    async fn test_get_file_info_tool_schema() {
        let tool = GetFileInfoTool;
        assert_eq!(tool.name(), "get_file_info");
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
