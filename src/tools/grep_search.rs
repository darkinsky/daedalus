//! Built-in tool for searching file contents using a bundled ripgrep binary.
//!
//! Provides a structured interface for the LLM to search code by content.
//! Uses a bundled `rg` binary shipped in the project's `bin/` directory,
//! falling back to a system-installed `rg`, then to a built-in regex walker.

use std::path::Path;

use anyhow::{Context, Result};
use async_trait::async_trait;
use once_cell::sync::Lazy;
use tokio::process::Command;

use super::BuiltinTool;
use super::fs_utils::{get_optional_bool, get_optional_string, get_optional_u64, get_required_string, resolve_path};

/// Maximum number of matching lines returned by default.
const DEFAULT_MAX_RESULTS: usize = 100;

/// Maximum output bytes to prevent unbounded memory usage.
const MAX_OUTPUT_BYTES: usize = 128 * 1024; // 128 KB

/// Resolve the path to the `rg` binary.
///
/// Priority:
/// 1. Bundled `bin/rg` next to the executable (or ancestor directories)
/// 2. Bundled `bin/rg` relative to cwd (for development)
/// 3. System `rg` on PATH
fn find_rg_binary() -> Option<String> {
    // 1. Search from the executable directory upward (handles both installed
    //    and cargo target/debug layouts without nested if-let chains).
    if let Ok(exe_path) = std::env::current_exe() {
        let mut dir = exe_path.parent();
        // Walk up at most 3 levels: exe_dir, exe_dir/.., exe_dir/../..
        for _ in 0..3 {
            if let Some(d) = dir {
                let candidate = d.join("bin/rg");
                if candidate.is_file() {
                    return Some(candidate.to_string_lossy().into_owned());
                }
                dir = d.parent();
            } else {
                break;
            }
        }
    }

    // 2. Relative to cwd (for development: project_root/bin/rg)
    if let Ok(cwd) = std::env::current_dir() {
        let candidate = cwd.join("bin/rg");
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }

    // 3. System rg on PATH
    if which_sync("rg") {
        return Some("rg".to_string());
    }

    None
}

/// Check if a command exists on PATH (synchronous).
fn which_sync(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Cached rg binary path, resolved once at first use.
static RG_BINARY: Lazy<Option<String>> = Lazy::new(find_rg_binary);

/// Search file contents using regex or literal patterns.
pub struct GrepSearchTool;

#[async_trait]
impl BuiltinTool for GrepSearchTool {
    fn name(&self) -> &str {
        "grep_search"
    }

    fn description(&self) -> &str {
        "Search file contents using regex or literal string patterns. Uses ripgrep (rg) \
         for fast, precise searches across a directory tree. Returns matching lines with \
         file paths and line numbers. Automatically respects .gitignore rules and skips \
         binary files, hidden directories, and common noise directories."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "The search pattern (regex by default, or literal if use_regex is false)."
                },
                "path": {
                    "type": "string",
                    "description": "The directory or file to search in. Defaults to the current working directory."
                },
                "include_pattern": {
                    "type": "string",
                    "description": "Glob pattern to filter files (e.g., '*.rs', '*.py'). Only files matching this pattern are searched."
                },
                "use_regex": {
                    "type": "boolean",
                    "description": "Whether to treat the pattern as a regex (default: true). Set to false for literal string matching."
                },
                "case_sensitive": {
                    "type": "boolean",
                    "description": "Whether the search is case-sensitive (default: true). Set to false for case-insensitive search."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of matching lines to return. Defaults to 100."
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String> {
        let pattern = get_required_string(&arguments, "pattern")?;
        let path_str = get_optional_string(&arguments, "path");
        let include_pattern = get_optional_string(&arguments, "include_pattern");
        let use_regex = get_optional_bool(&arguments, "use_regex").unwrap_or(true);
        let case_sensitive = get_optional_bool(&arguments, "case_sensitive").unwrap_or(true);
        let max_results = get_optional_u64(&arguments, "max_results")
            .map(|v| v as usize)
            .unwrap_or(DEFAULT_MAX_RESULTS);

        let search_path = match &path_str {
            Some(p) => resolve_path(p)?,
            None => std::env::current_dir().context("Failed to get current working directory")?,
        };

        if !search_path.exists() {
            anyhow::bail!("Search path does not exist: {}", search_path.display());
        }

        let rg_path = RG_BINARY.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "ripgrep (rg) not found. Place the rg binary in bin/rg or install it on your system."
            )
        })?;

        search_with_rg(
            rg_path,
            &pattern,
            &search_path,
            include_pattern.as_deref(),
            use_regex,
            case_sensitive,
            max_results,
        )
        .await
    }
}

/// Search using the ripgrep binary.
async fn search_with_rg(
    rg_path: &str,
    pattern: &str,
    search_path: &Path,
    include_pattern: Option<&str>,
    use_regex: bool,
    case_sensitive: bool,
    max_results: usize,
) -> Result<String> {
    let mut cmd = Command::new(rg_path);

    // Output format: file:line:content
    cmd.arg("--line-number")
        .arg("--no-heading")
        .arg("--color=never")
        .arg("--max-columns=256")
        .arg("--max-columns-preview");

    // Max results (per file, but combined with global limit via output truncation)
    cmd.arg(format!("--max-count={}", max_results));

    // Regex vs fixed string
    if !use_regex {
        cmd.arg("--fixed-strings");
    }

    // Case sensitivity
    if !case_sensitive {
        cmd.arg("--ignore-case");
    }

    // File type filter
    if let Some(glob) = include_pattern {
        cmd.arg("--glob").arg(glob);
    }

    // Pattern and path
    cmd.arg("--").arg(pattern).arg(search_path);

    cmd.stdin(std::process::Stdio::null());

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        cmd.output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("grep_search timed out after 30 seconds"))?
    .with_context(|| format!("Failed to execute rg at: {}", rg_path))?;

    // rg exit codes: 0 = matches found, 1 = no matches, 2 = error
    if output.status.code() == Some(2) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("ripgrep error: {}", stderr.trim());
    }

    let stdout = if output.stdout.len() > MAX_OUTPUT_BYTES {
        let truncated = String::from_utf8_lossy(&output.stdout[..MAX_OUTPUT_BYTES]);
        format!(
            "{}\n\n... (output truncated, {} bytes total)",
            truncated.trim_end(),
            output.stdout.len()
        )
    } else {
        String::from_utf8_lossy(&output.stdout).trim_end().to_string()
    };

    if stdout.is_empty() {
        return Ok(format!(
            "No matches found for pattern '{}' in {}",
            pattern,
            search_path.display()
        ));
    }

    // Count result lines
    let match_count = stdout.lines().count();
    let mut result = format!(
        "Found {} matching line(s) for '{}' in {}:\n\n",
        match_count, pattern, search_path.display()
    );
    result.push_str(&stdout);

    if match_count >= max_results {
        result.push_str(&format!("\n\n... (results capped at {})\n", max_results));
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_grep_search_tool_schema() {
        let tool = GrepSearchTool;
        assert_eq!(tool.name(), "grep_search");
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["pattern"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::json!("pattern")));
    }

    #[tokio::test]
    async fn test_to_openai_json() {
        let tool = GrepSearchTool;
        let json = tool.to_openai_json();
        assert_eq!(json["type"], "function");
        assert_eq!(json["function"]["name"], "grep_search");
    }

    #[tokio::test]
    async fn test_rg_binary_found() {
        // The bundled rg should be discoverable
        assert!(
            RG_BINARY.is_some(),
            "rg binary not found — ensure bin/rg exists in the project root"
        );
    }

    #[tokio::test]
    async fn test_search_nonexistent_path() {
        let tool = GrepSearchTool;
        let args = serde_json::json!({
            "pattern": "test",
            "path": "/nonexistent/path/that/does/not/exist"
        });
        let result = tool.execute(args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_search_in_tmp() {
        let dir = std::env::temp_dir().join("daedalus_grep_test");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(
            dir.join("test.txt"),
            "hello world\nfoo bar\nhello again\n",
        )
        .await
        .unwrap();

        let tool = GrepSearchTool;
        let args = serde_json::json!({
            "pattern": "hello",
            "path": dir.to_str().unwrap()
        });
        let result = tool.execute(args).await.unwrap();
        assert!(result.contains("hello"));
        assert!(result.contains("2 matching line"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_search_case_insensitive() {
        let dir = std::env::temp_dir().join("daedalus_grep_case_test");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(
            dir.join("test.txt"),
            "Hello World\nhello world\nHELLO WORLD\n",
        )
        .await
        .unwrap();

        let tool = GrepSearchTool;
        let args = serde_json::json!({
            "pattern": "hello",
            "path": dir.to_str().unwrap(),
            "case_sensitive": false
        });
        let result = tool.execute(args).await.unwrap();
        assert!(result.contains("3 matching line"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_search_fixed_string() {
        let dir = std::env::temp_dir().join("daedalus_grep_fixed_test");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(
            dir.join("test.txt"),
            "foo.bar\nfoo bar\nfooXbar\n",
        )
        .await
        .unwrap();

        let tool = GrepSearchTool;
        let args = serde_json::json!({
            "pattern": "foo.bar",
            "path": dir.to_str().unwrap(),
            "use_regex": false
        });
        let result = tool.execute(args).await.unwrap();
        assert!(result.contains("foo.bar"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_search_with_include_pattern() {
        let dir = std::env::temp_dir().join("daedalus_grep_include_test");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("hello.rs"), "fn hello() {}\n").await.unwrap();
        tokio::fs::write(dir.join("hello.py"), "def hello():\n").await.unwrap();

        let tool = GrepSearchTool;
        let args = serde_json::json!({
            "pattern": "hello",
            "path": dir.to_str().unwrap(),
            "include_pattern": "*.rs"
        });
        let result = tool.execute(args).await.unwrap();
        assert!(result.contains("hello.rs"));
        assert!(!result.contains("hello.py"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_search_respects_hidden_dirs() {
        let dir = std::env::temp_dir().join("daedalus_grep_hidden_test");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        let hidden = dir.join(".hidden");
        tokio::fs::create_dir_all(&hidden).await.unwrap();
        tokio::fs::write(hidden.join("secret.txt"), "hidden_match\n").await.unwrap();
        tokio::fs::write(dir.join("visible.txt"), "visible_match\n").await.unwrap();

        let tool = GrepSearchTool;
        let args = serde_json::json!({
            "pattern": "match",
            "path": dir.to_str().unwrap()
        });
        let result = tool.execute(args).await.unwrap();
        assert!(result.contains("visible_match"));
        assert!(!result.contains("hidden_match"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
