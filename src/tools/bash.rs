//! Built-in tool for executing bash commands.
//!
//! Provides a safe interface for the LLM to run shell commands on the host
//! system. Commands are executed via `/bin/bash -c` with configurable
//! working directory and timeout.

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::path::Path;
use tokio::process::Command;

use super::BuiltinTool;

/// Default timeout for bash commands (in seconds).
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Maximum allowed timeout to prevent DoS via LLM-supplied values.
const MAX_TIMEOUT_SECS: u64 = 300;

/// Maximum output size in bytes to prevent unbounded memory usage.
const MAX_OUTPUT_BYTES: usize = 256 * 1024; // 256 KB

// ── bash ──

/// Execute a bash command and return its output.
pub struct BashTool;

#[async_trait]
impl BuiltinTool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Execute a bash command on the host system and return its output (stdout and stderr). \
         Use for running shell commands, scripts, build tools, git operations, etc. \
         Commands are executed via /bin/bash -c with an optional working directory and timeout."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The bash command to execute. This is passed to /bin/bash -c."
                },
                "working_directory": {
                    "type": "string",
                    "description": "The working directory for the command. Defaults to the current working directory if not specified."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Timeout in seconds. The command will be killed if it exceeds this duration. Defaults to 30 seconds."
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String> {
        let command_str = arguments
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: 'command'"))?;

        let working_dir = arguments
            .get("working_directory")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let timeout_secs = arguments
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .min(MAX_TIMEOUT_SECS);

        tracing::info!(
            command = %command_str,
            working_dir = ?working_dir,
            timeout_secs = timeout_secs,
            "Executing bash command"
        );

        // Build the command
        let mut cmd = Command::new("/bin/bash");
        cmd.arg("-c").arg(&command_str);

        // Set working directory if specified
        if let Some(ref dir) = working_dir {
            let dir_path = Path::new(dir);
            if !dir_path.exists() {
                anyhow::bail!("Working directory does not exist: {}", dir);
            }
            cmd.current_dir(dir_path);
        }

        // Prevent the child from inheriting stdin (avoid blocking on interactive commands)
        cmd.stdin(std::process::Stdio::null());

        // Execute with timeout
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            cmd.output(),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "Command timed out after {} seconds: {}",
                timeout_secs,
                command_str
            )
        })?
        .with_context(|| format!("Failed to execute command: {}", command_str))?;

        // Build result
        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = truncate_output(&output.stdout, MAX_OUTPUT_BYTES);
        let stderr = truncate_output(&output.stderr, MAX_OUTPUT_BYTES);

        let mut result = String::new();

        if !stdout.is_empty() {
            result.push_str(&stdout);
        }

        if !stderr.is_empty() {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str("[stderr]\n");
            result.push_str(&stderr);
        }

        if result.is_empty() {
            result.push_str("(no output)");
        }

        // Append exit code if non-zero
        if exit_code != 0 {
            result.push_str(&format!("\n\n[exit code: {}]", exit_code));
        }

        Ok(result)
    }
}

/// Truncate command output to a maximum byte size, converting to a lossy UTF-8 string.
fn truncate_output(bytes: &[u8], max_bytes: usize) -> String {
    if bytes.is_empty() {
        return String::new();
    }

    if bytes.len() <= max_bytes {
        String::from_utf8_lossy(bytes).trim_end().to_string()
    } else {
        let truncated = String::from_utf8_lossy(&bytes[..max_bytes]);
        format!(
            "{}\n\n... (output truncated, {} bytes total)",
            truncated.trim_end(),
            bytes.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bash_tool_schema() {
        let tool = BashTool;
        assert_eq!(tool.name(), "bash");
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["command"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::json!("command")));
    }

    #[test]
    fn test_to_openai_json() {
        let tool = BashTool;
        let json = tool.to_openai_json();
        assert_eq!(json["type"], "function");
        assert_eq!(json["function"]["name"], "bash");
        assert!(json["function"]["description"].as_str().unwrap().len() > 0);
    }

    #[test]
    fn test_truncate_output_empty() {
        assert_eq!(truncate_output(b"", 100), "");
    }

    #[test]
    fn test_truncate_output_within_limit() {
        assert_eq!(truncate_output(b"hello world", 100), "hello world");
    }

    #[test]
    fn test_truncate_output_exceeds_limit() {
        let result = truncate_output(b"hello world", 5);
        assert!(result.contains("hello"));
        assert!(result.contains("truncated"));
        assert!(result.contains("11 bytes total"));
    }

    #[tokio::test]
    async fn test_execute_echo() {
        let tool = BashTool;
        let args = serde_json::json!({"command": "echo hello"});
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result, "hello");
    }

    #[tokio::test]
    async fn test_execute_with_stderr() {
        let tool = BashTool;
        let args = serde_json::json!({"command": "echo out && echo err >&2"});
        let result = tool.execute(args).await.unwrap();
        assert!(result.contains("out"));
        assert!(result.contains("[stderr]"));
        assert!(result.contains("err"));
    }

    #[tokio::test]
    async fn test_execute_nonzero_exit() {
        let tool = BashTool;
        let args = serde_json::json!({"command": "exit 42"});
        let result = tool.execute(args).await.unwrap();
        assert!(result.contains("[exit code: 42]"));
    }

    #[tokio::test]
    async fn test_execute_with_working_directory() {
        let tool = BashTool;
        let args = serde_json::json!({
            "command": "pwd",
            "working_directory": "/tmp"
        });
        let result = tool.execute(args).await.unwrap();
        assert!(result.contains("/tmp"));
    }

    #[tokio::test]
    async fn test_execute_missing_command() {
        let tool = BashTool;
        let args = serde_json::json!({});
        let result = tool.execute(args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_invalid_working_directory() {
        let tool = BashTool;
        let args = serde_json::json!({
            "command": "echo test",
            "working_directory": "/nonexistent/path/that/does/not/exist"
        });
        let result = tool.execute(args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_timeout() {
        let tool = BashTool;
        let args = serde_json::json!({
            "command": "sleep 10",
            "timeout_secs": 1
        });
        let result = tool.execute(args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }
}
