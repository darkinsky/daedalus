//! Isolation and lifecycle hook support for subagent execution.
//!
//! This module handles two orthogonal concerns that are separated from
//! the core execution loop in `runner.rs`:
//!
//! - **Git worktree isolation**: Creates temporary worktrees so subagents
//!   can operate on an isolated copy of the codebase.
//! - **Lifecycle hooks**: Runs shell commands before/after subagent execution
//!   (e.g., for setup/teardown scripts).

use anyhow::Result;

/// Run a lifecycle hook shell command.
///
/// The hook receives input via stdin (task description for onStart,
/// result content for onComplete).
pub async fn run_lifecycle_hook(
    hook_name: &str,
    agent_name: &str,
    command: &str,
    stdin_input: &str,
) -> Result<()> {
    tracing::info!(
        hook = hook_name,
        agent = agent_name,
        command = command,
        "Running lifecycle hook"
    );

    let mut child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!(
            "Failed to run {} hook for subagent '{}': {}",
            hook_name, agent_name, e
        ))?;

    // Write input to stdin
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(stdin_input.as_bytes()).await;
        drop(stdin);
    }

    let output = child.wait_with_output().await.map_err(|e| {
        anyhow::anyhow!(
            "Failed to wait for {} hook for subagent '{}': {}",
            hook_name, agent_name, e
        )
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            hook = hook_name,
            agent = agent_name,
            exit_code = ?output.status.code(),
            stderr = %stderr,
            "Lifecycle hook exited with non-zero status"
        );
    } else {
        tracing::info!(
            hook = hook_name,
            agent = agent_name,
            "Lifecycle hook completed successfully"
        );
    }

    Ok(())
}

/// Set up a git worktree for isolated subagent execution.
///
/// Creates a temporary worktree branch and returns a guard that
/// cleans up the worktree when dropped.
pub fn setup_worktree(agent_name: &str) -> Result<WorktreeGuard> {
    let worktree_dir = std::env::temp_dir()
        .join(format!("daedalus-worktree-{}-{}", agent_name, std::process::id()));

    let branch_name = format!("daedalus/subagent/{}", agent_name);

    tracing::info!(
        agent = agent_name,
        worktree = %worktree_dir.display(),
        branch = %branch_name,
        "Setting up git worktree for isolated execution"
    );

    // Try creating a new worktree with a new branch
    let output = std::process::Command::new("git")
        .args(["worktree", "add", "-b", &branch_name])
        .arg(&worktree_dir)
        .arg("HEAD")
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to create git worktree: {}", e))?;

    if output.status.success() {
        return finish_worktree_setup(agent_name, worktree_dir, branch_name);
    }

    // If the branch already exists, retry without -b
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.contains("already exists") {
        anyhow::bail!("Failed to create git worktree: {}", stderr);
    }

    let output = std::process::Command::new("git")
        .args(["worktree", "add"])
        .arg(&worktree_dir)
        .arg(&branch_name)
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to create git worktree: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to create git worktree: {}", stderr);
    }

    finish_worktree_setup(agent_name, worktree_dir, branch_name)
}

/// Log worktree creation and return the cleanup guard.
fn finish_worktree_setup(
    agent_name: &str,
    worktree_dir: std::path::PathBuf,
    branch_name: String,
) -> Result<WorktreeGuard> {
    tracing::info!(
        agent = agent_name,
        worktree = %worktree_dir.display(),
        "Git worktree created for isolated execution"
    );

    Ok(WorktreeGuard {
        worktree_dir,
        branch_name,
    })
}

/// RAII guard that cleans up a git worktree when dropped.
///
/// Automatically removes the worktree directory and deletes the
/// temporary branch on drop, ensuring no leftover state.
pub struct WorktreeGuard {
    worktree_dir: std::path::PathBuf,
    branch_name: String,
}

impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        tracing::info!(
            worktree = %self.worktree_dir.display(),
            branch = %self.branch_name,
            "Cleaning up git worktree"
        );

        // Remove the worktree
        let _ = std::process::Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&self.worktree_dir)
            .output();

        // Delete the temporary branch
        let _ = std::process::Command::new("git")
            .args(["branch", "-D"])
            .arg(&self.branch_name)
            .output();
    }
}
