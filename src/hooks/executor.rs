//! Hook executor — runs shell commands with environment variable injection.

use std::collections::HashMap;
use std::time::Duration;

use tokio::process::Command;

use super::config::HookEntry;

/// Result of executing a hook command.
#[derive(Debug)]
pub struct HookResult {
    /// Whether the hook succeeded (exit code 0).
    pub success: bool,
    /// The exit code (None if the process was killed/timed out).
    pub exit_code: Option<i32>,
    /// Combined stdout + stderr output (truncated to 1KB).
    pub output: String,
}

/// Execute a hook command with the given environment variables.
///
/// The command is run via `sh -c` with a timeout. Environment variables
/// are injected to provide context about the current operation.
pub async fn execute_hook(
    hook: &HookEntry,
    env_vars: &HashMap<String, String>,
) -> HookResult {
    let timeout = Duration::from_secs(hook.timeout_secs);

    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(&hook.command);

    // Inject environment variables
    for (key, value) in env_vars {
        cmd.env(key, value);
    }

    // Capture output
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    // Spawn the child so we can kill it on timeout
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return HookResult {
                success: false,
                exit_code: None,
                output: format!("Failed to execute hook: {}", e),
            };
        }
    };

    // Read stdout/stderr concurrently with waiting for the process to exit.
    // This prevents deadlocks when the child's output fills the pipe buffer
    // (~64KB) — the child would block on write while we block on wait.
    //
    // We take the handles first, then read them in parallel with wait().
    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();

    let stdout_fut = async {
        if let Some(mut h) = stdout_handle {
            let mut buf = Vec::new();
            let _ = tokio::io::AsyncReadExt::read_to_end(&mut h, &mut buf).await;
            String::from_utf8_lossy(&buf).to_string()
        } else {
            String::new()
        }
    };

    let stderr_fut = async {
        if let Some(mut h) = stderr_handle {
            let mut buf = Vec::new();
            let _ = tokio::io::AsyncReadExt::read_to_end(&mut h, &mut buf).await;
            String::from_utf8_lossy(&buf).to_string()
        } else {
            String::new()
        }
    };

    // Run all three concurrently: read stdout, read stderr, wait for exit.
    // The timeout wraps the entire operation.
    let wait_result = tokio::time::timeout(
        timeout,
        async {
            let (stdout, stderr, status) = tokio::join!(stdout_fut, stderr_fut, child.wait());
            (stdout, stderr, status)
        },
    ).await;

    match wait_result {
        Ok((stdout, stderr, Ok(status))) => {
            let combined = if stderr.is_empty() {
                stdout
            } else {
                format!("{}\n{}", stdout, stderr)
            };

            // Truncate output to 1KB (UTF-8 safe)
            let truncated = truncate_utf8(&combined, 1024);

            HookResult {
                success: status.success(),
                exit_code: status.code(),
                output: truncated,
            }
        }
        Ok((_, _, Err(e))) => {
            HookResult {
                success: false,
                exit_code: None,
                output: format!("Failed to execute hook: {}", e),
            }
        }
        Err(_) => {
            // Timeout — kill the child process to prevent zombie/leaked processes
            let _ = child.kill().await;
            // Reap the child process to avoid zombie processes on Linux.
            // After kill(), we must wait() to collect the exit status.
            let _ = child.wait().await;
            HookResult {
                success: false,
                exit_code: None,
                output: format!(
                    "Hook timed out after {}s: {}",
                    hook.timeout_secs, hook.command
                ),
            }
        }
    }
}

/// Build environment variables for a PreToolUse hook.
pub fn pre_tool_env(
    tool_name: &str,
    tool_input: &serde_json::Value,
    session_id: &str,
) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert("DAEDALUS_TOOL_NAME".to_string(), tool_name.to_string());
    env.insert("DAEDALUS_TOOL_INPUT".to_string(), tool_input.to_string());
    env.insert("DAEDALUS_SESSION_ID".to_string(), session_id.to_string());
    env
}

/// Maximum size for DAEDALUS_TOOL_OUTPUT env var (8 KB).
/// Prevents oversized environment variables that could cause `sh -c` to fail.
const MAX_TOOL_OUTPUT_ENV_BYTES: usize = 8 * 1024;

/// Build environment variables for a PostToolUse hook.
pub fn post_tool_env(
    tool_name: &str,
    tool_input: &serde_json::Value,
    tool_output: &str,
    success: bool,
    session_id: &str,
) -> HashMap<String, String> {
    let mut env = pre_tool_env(tool_name, tool_input, session_id);
    // Truncate tool output to prevent oversized environment variables (UTF-8 safe)
    let output_value = if tool_output.len() > MAX_TOOL_OUTPUT_ENV_BYTES {
        let safe = truncate_utf8_raw(tool_output, MAX_TOOL_OUTPUT_ENV_BYTES);
        format!("{}...[truncated, {} bytes total]", safe, tool_output.len())
    } else {
        tool_output.to_string()
    };
    env.insert("DAEDALUS_TOOL_OUTPUT".to_string(), output_value);
    env.insert("DAEDALUS_TOOL_SUCCESS".to_string(), success.to_string());
    env
}

/// Truncate a string to at most `max_bytes` bytes at a valid UTF-8 boundary,
/// appending "...[truncated]" if truncation occurred.
fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let truncated = truncate_utf8_raw(s, max_bytes);
    format!("{}...[truncated]", truncated)
}

/// Truncate a string to at most `max_bytes` bytes at a valid UTF-8 boundary.
/// Returns the truncated slice (no suffix appended).
fn truncate_utf8_raw(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // Walk backwards from max_bytes to find a valid char boundary
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Build environment variables for a SessionStart hook.
pub fn session_env(session_id: &str) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert("DAEDALUS_SESSION_ID".to_string(), session_id.to_string());
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_execute_hook_success() {
        let hook = HookEntry {
            matcher: None,
            command: "echo hello".to_string(),
            timeout_secs: 5,
        };
        let result = execute_hook(&hook, &HashMap::new()).await;
        assert!(result.success);
        assert_eq!(result.exit_code, Some(0));
        assert!(result.output.contains("hello"));
    }

    #[tokio::test]
    async fn test_execute_hook_failure() {
        let hook = HookEntry {
            matcher: None,
            command: "exit 1".to_string(),
            timeout_secs: 5,
        };
        let result = execute_hook(&hook, &HashMap::new()).await;
        assert!(!result.success);
        assert_eq!(result.exit_code, Some(1));
    }

    #[tokio::test]
    async fn test_execute_hook_timeout() {
        let hook = HookEntry {
            matcher: None,
            command: "sleep 10".to_string(),
            timeout_secs: 1,
        };
        let result = execute_hook(&hook, &HashMap::new()).await;
        assert!(!result.success);
        assert!(result.output.contains("timed out"));
    }

    #[tokio::test]
    async fn test_execute_hook_with_env() {
        let hook = HookEntry {
            matcher: None,
            command: "echo $DAEDALUS_TOOL_NAME".to_string(),
            timeout_secs: 5,
        };
        let env = pre_tool_env("bash", &serde_json::json!({"command": "ls"}), "test-session");
        let result = execute_hook(&hook, &env).await;
        assert!(result.success);
        assert!(result.output.contains("bash"));
    }
}
