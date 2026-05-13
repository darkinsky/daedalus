//! Built-in tool for executing bash commands.
//!
//! Provides a safe interface for the LLM to run shell commands on the host
//! system. Commands are executed via `/bin/bash -c` with configurable
//! working directory and timeout.

use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;

use super::BuiltinTool;

/// Default timeout for bash commands (in seconds).
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Maximum allowed timeout to prevent DoS via LLM-supplied values.
const MAX_TIMEOUT_SECS: u64 = 300;

/// Maximum output size in bytes to prevent unbounded memory usage.
const MAX_OUTPUT_BYTES: usize = 256 * 1024; // 256 KB

/// Shell commands that are considered safe for read-only (plan) mode.
///
/// Only the first word of the command (the program name) is checked.
/// This prevents write operations while allowing common read/analysis commands.
#[allow(dead_code)]
const READ_ONLY_ALLOWED_COMMANDS: &[&str] = &[
    "find", "wc", "grep", "rg", "cat", "head", "tail", "ls", "file", "stat",
    "sort", "uniq", "awk", "sed", "tr", "cut", "diff", "comm",
    "tree", "du", "df", "echo", "printf", "test", "expr",
    "cargo", "rustc", "git", "python", "node", "go",
];

/// Patterns in commands that indicate write/destructive operations,
/// blocked even if the base command is in the allow-list.
#[allow(dead_code)]
const READ_ONLY_BLOCKED_PATTERNS: &[&str] = &[
    " > ", " >> ", " | tee ", "rm ", "mv ", "cp ", "chmod ", "chown ",
    "curl ", "wget ", "apt ", "pip install", "cargo install",
    "mktemp", "mkdir ",
];

// ── Configuration ──

/// Bash tool configuration from YAML.
///
/// All fields are optional with sensible defaults. When not configured,
/// the tool behaves identically to the previous hardcoded behavior.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct BashConfig {
    /// Default timeout for commands in seconds (default: 30).
    pub default_timeout: u64,
    /// Maximum allowed timeout in seconds to prevent DoS (default: 300).
    pub max_timeout: u64,
    /// Maximum output size in bytes (default: 262144 = 256KB).
    pub max_output_bytes: usize,
}

impl Default for BashConfig {
    fn default() -> Self {
        Self {
            default_timeout: DEFAULT_TIMEOUT_SECS,
            max_timeout: MAX_TIMEOUT_SECS,
            max_output_bytes: MAX_OUTPUT_BYTES,
        }
    }
}

// ── bash ──

/// Execute a bash command and return its output.
pub struct BashTool {
    config: BashConfig,
}

impl BashTool {
    /// Create a new BashTool with the given configuration.
    pub fn new(config: BashConfig) -> Self {
        Self { config }
    }

    /// Execute a bash command with streaming output.
    ///
    /// Lines from stdout/stderr are sent to `on_output` in real time as they
    /// arrive. The full output is also collected and returned as a string.
    ///
    /// This is used by the tool pipeline to provide real-time bash output
    /// in the terminal while still collecting the full result for the LLM.
    pub async fn execute_streaming(
        &self,
        arguments: serde_json::Value,
        on_output: impl Fn(String) + Send + 'static,
    ) -> Result<String> {
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
            .unwrap_or(self.config.default_timeout)
            .min(self.config.max_timeout);

        tracing::info!(
            command = %command_str,
            working_dir = ?working_dir,
            timeout_secs = timeout_secs,
            "Executing bash command (streaming)"
        );

        let mut cmd = Command::new("/bin/bash");
        cmd.arg("-c").arg(&command_str);

        if let Some(ref dir) = working_dir {
            cmd.current_dir(dir);
        }

        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn()
            .with_context(|| format!("Failed to execute command: {}", command_str))?;

        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let mut stdout_reader = tokio::io::BufReader::new(stdout).lines();
        let mut stderr_reader = tokio::io::BufReader::new(stderr).lines();

        let mut output = String::new();
        let mut stderr_output = String::new();
        let max_bytes = self.config.max_output_bytes;
        let mut stdout_closed = false;
        let mut stderr_closed = false;

        let timeout = std::time::Duration::from_secs(timeout_secs);
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            // If both streams are closed, exit the loop
            if stdout_closed && stderr_closed {
                break;
            }

            tokio::select! {
                line = stdout_reader.next_line(), if !stdout_closed => {
                    match line {
                        Ok(Some(line)) => {
                            if output.len() + line.len() < max_bytes {
                                on_output(line.clone());
                                output.push_str(&line);
                                output.push('\n');
                            }
                        }
                        Ok(None) => {
                            stdout_closed = true; // Stop polling stdout, continue draining stderr
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, "Error reading stdout");
                            stdout_closed = true;
                        }
                    }
                }
                line = stderr_reader.next_line(), if !stderr_closed => {
                    match line {
                        Ok(Some(line)) => {
                            if stderr_output.len() + line.len() < max_bytes {
                                on_output(format!("[stderr] {}", line));
                                stderr_output.push_str(&line);
                                stderr_output.push('\n');
                            }
                        }
                        Ok(None) => {
                            stderr_closed = true; // Stop polling stderr
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, "Error reading stderr");
                            stderr_closed = true;
                        }
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    anyhow::bail!(
                        "Command timed out after {} seconds: {}",
                        timeout_secs,
                        command_str
                    );
                }
            }
        }

        // Wait for the process to finish
        let status = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            child.wait(),
        ).await
        .map_err(|_| anyhow::anyhow!("Timed out waiting for process exit"))?
        .with_context(|| "Failed to wait for child process")?;

        let exit_code = status.code().unwrap_or(-1);

        // Build result (same format as non-streaming execute)
        let mut result = output.trim_end().to_string();

        if !stderr_output.is_empty() {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str("[stderr]\n");
            result.push_str(stderr_output.trim_end());
        }

        if result.is_empty() {
            result.push_str("(no output)");
        }

        if exit_code != 0 {
            result.push_str(&format!("\n\n[exit code: {}]", exit_code));
        }

        Ok(result)
    }

    /// Check if a command is allowed in read-only mode.
    ///
    /// Returns `Ok(())` if allowed, or `Err` with an explanation if blocked.
    #[allow(dead_code)]
    pub fn validate_read_only(command: &str) -> Result<()> {
        let trimmed = command.trim();

        // Check for blocked destructive patterns
        for pattern in READ_ONLY_BLOCKED_PATTERNS {
            if trimmed.contains(pattern) {
                anyhow::bail!(
                    "Command blocked in read-only mode: contains '{}'. \
                     This subagent operates in plan/read-only mode and cannot \
                     perform write operations.",
                    pattern.trim()
                );
            }
        }

        // Check the base command (first word, or first word after env vars)
        let base_cmd = extract_base_command(trimmed);
        if READ_ONLY_ALLOWED_COMMANDS.iter().any(|&allowed| base_cmd == allowed) {
            return Ok(());
        }

        // Allow piped commands where the first command is allowed
        // (e.g., "find ... | wc -l" — find is allowed)
        if let Some(first_segment) = trimmed.split('|').next() {
            let first_cmd = extract_base_command(first_segment.trim());
            if READ_ONLY_ALLOWED_COMMANDS.iter().any(|&allowed| first_cmd == allowed) {
                return Ok(());
            }
        }

        anyhow::bail!(
            "Command '{}' is not in the read-only allow-list. \
             Allowed commands: {}",
            base_cmd,
            READ_ONLY_ALLOWED_COMMANDS.join(", ")
        )
    }
}

/// Extract the base command name from a shell command string.
///
/// Handles common patterns like:
/// - `wc -l file` → `wc`
/// - `ENV=val command arg` → `command`
/// - `/usr/bin/grep pattern` → `grep`
#[allow(dead_code)]
fn extract_base_command(cmd: &str) -> &str {
    let trimmed = cmd.trim();

    // Skip leading environment variable assignments (FOO=bar command)
    let after_env = trimmed
        .split_whitespace()
        .find(|word| !word.contains('='))
        .unwrap_or(trimmed);

    // Take just the program name (strip path: /usr/bin/grep → grep)
    let base = after_env.rsplit('/').next().unwrap_or(after_env);

    // Strip everything after whitespace
    base.split_whitespace().next().unwrap_or(base)
}

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

    fn is_read_only(&self) -> bool {
        false
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
            .unwrap_or(self.config.default_timeout)
            .min(self.config.max_timeout);

        tracing::info!(
            command = %command_str,
            working_dir = ?working_dir,
            timeout_secs = timeout_secs,
            "Executing bash command"
        );

        // Build the command
        let mut cmd = Command::new("/bin/bash");
        cmd.arg("-c").arg(&command_str);

        // Set working directory if specified.
        // We set it and let Command::spawn return an error if it doesn't exist,
        // rather than using Path::exists() which has a TOCTOU race condition.
        if let Some(ref dir) = working_dir {
            cmd.current_dir(dir);
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
        let stdout = truncate_output(&output.stdout, self.config.max_output_bytes);
        let stderr = truncate_output(&output.stderr, self.config.max_output_bytes);

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
        let tool = BashTool::new(BashConfig::default());
        assert_eq!(tool.name(), "bash");
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["command"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::json!("command")));
    }

    #[test]
    fn test_to_openai_json() {
        let tool = BashTool::new(BashConfig::default());
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
        let tool = BashTool::new(BashConfig::default());
        let args = serde_json::json!({"command": "echo hello"});
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result, "hello");
    }

    #[tokio::test]
    async fn test_execute_with_stderr() {
        let tool = BashTool::new(BashConfig::default());
        let args = serde_json::json!({"command": "echo out && echo err >&2"});
        let result = tool.execute(args).await.unwrap();
        assert!(result.contains("out"));
        assert!(result.contains("[stderr]"));
        assert!(result.contains("err"));
    }

    #[tokio::test]
    async fn test_execute_nonzero_exit() {
        let tool = BashTool::new(BashConfig::default());
        let args = serde_json::json!({"command": "exit 42"});
        let result = tool.execute(args).await.unwrap();
        assert!(result.contains("[exit code: 42]"));
    }

    #[tokio::test]
    async fn test_execute_with_working_directory() {
        let tool = BashTool::new(BashConfig::default());
        let args = serde_json::json!({
            "command": "pwd",
            "working_directory": "/tmp"
        });
        let result = tool.execute(args).await.unwrap();
        assert!(result.contains("/tmp"));
    }

    #[tokio::test]
    async fn test_execute_missing_command() {
        let tool = BashTool::new(BashConfig::default());
        let args = serde_json::json!({});
        let result = tool.execute(args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_invalid_working_directory() {
        let tool = BashTool::new(BashConfig::default());
        let args = serde_json::json!({
            "command": "echo test",
            "working_directory": "/nonexistent/path/that/does/not/exist"
        });
        let result = tool.execute(args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_timeout() {
        let tool = BashTool::new(BashConfig::default());
        let args = serde_json::json!({
            "command": "sleep 10",
            "timeout_secs": 1
        });
        let result = tool.execute(args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }
}
