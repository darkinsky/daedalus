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

    // Take stdout/stderr handles before waiting, so we retain ownership of `child`
    // for potential kill on timeout.
    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();

    let wait_result = tokio::time::timeout(timeout, child.wait()).await;

    match wait_result {
        Ok(Ok(status)) => {
            // Process exited within timeout — read captured output
            let stdout = if let Some(mut h) = stdout_handle {
                let mut buf = Vec::new();
                let _ = tokio::io::AsyncReadExt::read_to_end(&mut h, &mut buf).await;
                String::from_utf8_lossy(&buf).to_string()
            } else {
                String::new()
            };
            let stderr = if let Some(mut h) = stderr_handle {
                let mut buf = Vec::new();
                let _ = tokio::io::AsyncReadExt::read_to_end(&mut h, &mut buf).await;
                String::from_utf8_lossy(&buf).to_string()
            } else {
                String::new()
            };

            let combined = if stderr.is_empty() {
                stdout
            } else {
                format!("{}\n{}", stdout, stderr)
            };

            // Truncate output to 1KB
            let truncated = if combined.len() > 1024 {
                format!("{}...[truncated]", &combined[..1024])
            } else {
                combined
            };

            HookResult {
                success: status.success(),
                exit_code: status.code(),
                output: truncated,
            }
        }
        Ok(Err(e)) => {
            HookResult {
                success: false,
                exit_code: None,
                output: format!("Failed to execute hook: {}", e),
            }
        }
        Err(_) => {
            // Timeout — kill the child process to prevent zombie/leaked processes
            let _ = child.kill().await;
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
    // Truncate tool output to prevent oversized environment variables
    let output_value = if tool_output.len() > MAX_TOOL_OUTPUT_ENV_BYTES {
        format!("{}...[truncated, {} bytes total]", &tool_output[..MAX_TOOL_OUTPUT_ENV_BYTES], tool_output.len())
    } else {
        tool_output.to_string()
    };
    env.insert("DAEDALUS_TOOL_OUTPUT".to_string(), output_value);
    env.insert("DAEDALUS_TOOL_SUCCESS".to_string(), success.to_string());
    env
}

/// Build environment variables for a SessionStart hook.
#[allow(dead_code)]
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
