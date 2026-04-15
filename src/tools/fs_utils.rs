use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Resolve a file path to an absolute path.
///
/// If the path is already absolute, it is returned as-is.
/// If the path is relative, it is resolved against the current working directory.
pub fn resolve_path(path_str: &str) -> Result<PathBuf> {
    let path = Path::new(path_str);
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        let cwd = std::env::current_dir().context("Failed to get current working directory")?;
        Ok(cwd.join(path))
    }
}

/// Extract a required string parameter from JSON arguments.
pub fn get_required_string(args: &serde_json::Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("Missing required parameter: '{}'", key))
}

/// Extract an optional string parameter from JSON arguments.
#[allow(dead_code)]
pub fn get_optional_string(args: &serde_json::Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

/// Extract an optional integer parameter from JSON arguments.
pub fn get_optional_u64(args: &serde_json::Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| v.as_u64())
}

/// Extract an optional boolean parameter from JSON arguments.
pub fn get_optional_bool(args: &serde_json::Value, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| v.as_bool())
}

/// Format a file size in human-readable form.
pub fn format_size(bytes: u64) -> String {
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

/// Directory names to skip during recursive file search.
///
/// These are common noise directories that rarely contain user-relevant files
/// and can significantly slow down searches if traversed.
pub const IGNORED_DIRS: &[&str] = &["node_modules", "target", "__pycache__", ".git"];

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
}
