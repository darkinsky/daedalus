use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::workspace;

/// System path prefixes that are always off-limits, regardless of workspace root.
///
/// Prevents LLM-driven file operations from accessing sensitive system paths.
/// Note: entries must end with '/' for directories to avoid prefix-matching
/// unrelated paths (e.g., "/etc/shadow" would wrongly block "/etc/shadowcopy").
const BLOCKED_PREFIXES: &[&str] = &[
    "/etc/shadow",
    "/etc/gshadow",
    "/etc/sudoers",
    "/etc/sudoers.d/",
    "/proc/",
    "/sys/",
    "/dev/",
    "/boot/",
    "/run/secrets/",
];

/// Sensitive home-directory path suffixes that should never be read/written by tools.
///
/// Covers common credential stores, private keys, and token files.
const BLOCKED_HOME_SUFFIXES: &[&str] = &[
    ".ssh/",
    ".gnupg/",
    ".gpg/",
    ".aws/credentials",
    ".aws/config",
    ".config/gcloud/",
    ".config/op/",
    ".kube/config",
    ".netrc",
    ".git-credentials",
    ".npmrc",
    ".pypirc",
    ".docker/config.json",
    ".vault-token",
    ".authinfo",
    ".authinfo.gpg",
];

/// Resolve a file path to an absolute path with security validation.
///
/// # Security
///
/// - **Blocked system paths**: `/etc/shadow`, `/proc/`, `/sys/`, etc. are always rejected
/// - **Blocked home secrets**: `~/.ssh/`, `~/.aws/credentials`, etc. are rejected
/// - **Relative path traversal**: `../../etc/passwd` is caught by canonicalizing and
///   verifying the resolved path doesn't land in a blocked zone
/// - **Absolute paths**: Allowed (the user/LLM may legitimately reference files
///   outside CWD), but still checked against the blocklist
pub fn resolve_path(path_str: &str) -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("Failed to get current working directory")?;
    let path = Path::new(path_str);

    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };

    // Canonicalize to resolve `..`, `.`, and symlinks where possible
    let resolved = if absolute.exists() {
        absolute.canonicalize().with_context(|| {
            format!("Failed to canonicalize path: {}", absolute.display())
        })?
    } else {
        // For non-existent paths, canonicalize the parent and append filename
        if let Some(parent) = absolute.parent() {
            if parent.exists() {
                let canon_parent = parent.canonicalize().with_context(|| {
                    format!("Failed to canonicalize parent: {}", parent.display())
                })?;
                if let Some(file_name) = absolute.file_name() {
                    canon_parent.join(file_name)
                } else {
                    canon_parent
                }
            } else {
                absolute
            }
        } else {
            absolute
        }
    };

    // Check against blocked system paths.
    //
    // For directory prefixes (ending with '/'), use starts_with.
    // For exact file paths (no trailing '/'), match exactly or as a path prefix
    // followed by '/' — this avoids false-positives like "/etc/shadow" blocking
    // "/etc/shadowcopy".
    let resolved_str = resolved.to_string_lossy();
    for prefix in BLOCKED_PREFIXES {
        let blocked = if prefix.ends_with('/') {
            resolved_str.starts_with(*prefix)
        } else {
            resolved_str == *prefix
                || resolved_str.starts_with(&format!("{}/", prefix))
        };
        if blocked {
            anyhow::bail!(
                "Access denied: path '{}' resolves to a restricted system path",
                path_str
            );
        }
    }

    // Check against blocked home-directory secrets
    if let Some(home) = workspace::home_dir() {
        let home_str = home.to_string_lossy();
        for suffix in BLOCKED_HOME_SUFFIXES {
            let blocked = format!("{}/{}", home_str, suffix);
            if resolved_str.starts_with(&blocked) {
                anyhow::bail!(
                    "Access denied: path '{}' resolves to a sensitive home directory path",
                    path_str
                );
            }
        }
    }

    Ok(resolved)
}

/// Extract a required string parameter from JSON arguments.
pub fn get_required_string(args: &serde_json::Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("Missing required parameter: '{}'", key))
}

/// Extract an optional string parameter from JSON arguments.
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
        assert!(result.starts_with(&cwd));
    }

    #[test]
    fn test_resolve_path_blocked_system_path() {
        let result = resolve_path("/etc/shadow");
        assert!(result.is_err(), "/etc/shadow should be blocked");

        let result = resolve_path("/proc/self/environ");
        assert!(result.is_err(), "/proc/ should be blocked");

        let result = resolve_path("/sys/class/net");
        assert!(result.is_err(), "/sys/ should be blocked");
    }

    #[test]
    fn test_resolve_path_traversal_to_blocked() {
        // Traversal that ends up at a blocked path should be caught
        // (e.g., if CWD is /data/workspace, ../../etc/shadow resolves to /etc/shadow)
        let result = resolve_path("../../../../../../etc/shadow");
        assert!(result.is_err(), "Traversal to /etc/shadow should be blocked");
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
