//! Checkpoint manager — file snapshot management for undo/rollback support.
//!
//! Provides a global checkpoint stack that records file contents before
//! modifications. Each checkpoint captures the state of one or more files
//! before a write operation, enabling `/undo` to restore them.
//!
//! ## Design
//!
//! - **In-memory snapshots**: File contents are stored in memory (not git stash)
//!   to avoid git dependency and work in non-git directories.
//! - **Stack-based**: Checkpoints are pushed on write, popped on undo.
//! - **Bounded**: Maximum 50 checkpoints to prevent unbounded memory growth.
//! - **Thread-safe**: Uses `Mutex` for concurrent access from parallel tool calls.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use once_cell::sync::Lazy;

/// Maximum number of checkpoints to retain in memory.
const MAX_CHECKPOINTS: usize = 50;

/// Maximum total memory budget for checkpoint content (100 MB).
const MAX_TOTAL_BYTES: usize = 100 * 1024 * 1024;

/// A single file snapshot — the content of a file before modification.
#[derive(Debug, Clone)]
struct FileSnapshot {
    /// The file content before modification. `None` means the file didn't exist.
    content: Option<String>,
}

/// A checkpoint — a collection of file snapshots from a single operation.
#[derive(Debug, Clone)]
struct Checkpoint {
    /// Human-readable description (e.g., "edit_file src/main.rs").
    description: String,
    /// File path → snapshot (content before modification).
    snapshots: HashMap<PathBuf, FileSnapshot>,
    /// Timestamp of the checkpoint.
    #[allow(dead_code)]
    created_at: std::time::Instant,
}

/// Global checkpoint stack.
static CHECKPOINT_STACK: Lazy<Mutex<Vec<Checkpoint>>> = Lazy::new(|| Mutex::new(Vec::new()));

/// Record a file's current content before modification.
///
/// Call this **before** writing to a file. The snapshot is pushed onto the
/// checkpoint stack so that `/undo` can restore it later.
///
/// # Arguments
/// * `path` — The absolute path to the file being modified.
/// * `description` — Human-readable description (e.g., "edit_file src/main.rs").
pub async fn snapshot_before_write(path: &PathBuf, description: &str) {
    let content = match tokio::fs::read_to_string(path).await {
        Ok(c) => Some(c),
        Err(_) => None, // File doesn't exist yet (create operation)
    };

    let mut snapshots = HashMap::new();
    snapshots.insert(path.clone(), FileSnapshot { content });
    push_checkpoint(description, snapshots);
}

/// Record a file's content before modification, using already-loaded content.
///
/// This avoids a redundant disk read when the caller has already read the file
/// (e.g., `edit_file` and `multi_edit` read the file for validation first).
///
/// Pass `None` for `content` if the file does not yet exist (create operation).
pub fn snapshot_with_content(path: &PathBuf, content: Option<String>, description: &str) {
    let mut snapshots = HashMap::new();
    snapshots.insert(path.clone(), FileSnapshot { content });
    push_checkpoint(description, snapshots);
}

/// Record multiple files' current content before modification.
///
/// Used by `multi_edit` which modifies a single file but we keep this
/// generic for future use.
pub async fn snapshot_files_before_write(paths: &[PathBuf], description: &str) {
    let mut snapshots = HashMap::new();

    for path in paths {
        let content = match tokio::fs::read_to_string(path).await {
            Ok(c) => Some(c),
            Err(_) => None,
        };
        snapshots.insert(path.clone(), FileSnapshot { content });
    }

    push_checkpoint(description, snapshots);
}

/// Push a checkpoint onto the stack, enforcing count and memory limits.
fn push_checkpoint(description: &str, snapshots: HashMap<PathBuf, FileSnapshot>) {
    let checkpoint = Checkpoint {
        description: description.to_string(),
        snapshots,
        created_at: std::time::Instant::now(),
    };

    if let Ok(mut stack) = CHECKPOINT_STACK.lock() {
        stack.push(checkpoint);

        // Enforce maximum checkpoint count
        if stack.len() > MAX_CHECKPOINTS {
            stack.remove(0);
        }

        // Enforce memory budget: evict oldest checkpoints until under limit
        while stack.len() > 1 && estimate_total_bytes(&stack) > MAX_TOTAL_BYTES {
            stack.remove(0);
        }
    }
}

/// Estimate total memory usage of all checkpoint snapshots.
fn estimate_total_bytes(stack: &[Checkpoint]) -> usize {
    stack.iter().map(|cp| {
        cp.snapshots.values().map(|s| {
            s.content.as_ref().map_or(0, |c| c.len())
        }).sum::<usize>()
    }).sum()
}

/// Undo the most recent file modification.
///
/// Pops the latest checkpoint from the stack and restores all files
/// to their pre-modification state.
///
/// Returns a human-readable description of what was undone, or an error
/// if there's nothing to undo.
pub async fn undo() -> anyhow::Result<String> {
    let checkpoint = {
        let mut stack = CHECKPOINT_STACK.lock()
            .map_err(|_| anyhow::anyhow!("Failed to acquire checkpoint lock"))?;
        stack.pop()
    };

    let checkpoint = checkpoint
        .ok_or_else(|| anyhow::anyhow!("Nothing to undo — no file modifications recorded in this session."))?;

    let mut restored_files = Vec::new();

    for (path, snapshot) in &checkpoint.snapshots {
        match &snapshot.content {
            Some(content) => {
                // Restore the file to its previous content
                tokio::fs::write(path, content).await
                    .map_err(|e| anyhow::anyhow!("Failed to restore {}: {}", path.display(), e))?;
                restored_files.push(format!("  restored: {}", path.display()));
            }
            None => {
                // File didn't exist before — remove it
                if path.exists() {
                    tokio::fs::remove_file(path).await
                        .map_err(|e| anyhow::anyhow!("Failed to remove {}: {}", path.display(), e))?;
                    restored_files.push(format!("  removed: {} (was newly created)", path.display()));
                }
            }
        }
    }

    let remaining = CHECKPOINT_STACK.lock()
        .map(|s| s.len())
        .unwrap_or(0);

    Ok(format!(
        "Undid: {}\n{}\n({} more undo(s) available)",
        checkpoint.description,
        restored_files.join("\n"),
        remaining,
    ))
}

/// Return the number of available undo operations.
pub fn undo_count() -> usize {
    CHECKPOINT_STACK.lock()
        .map(|s| s.len())
        .unwrap_or(0)
}

/// Return a summary of the checkpoint stack for display.
#[allow(dead_code)]
pub fn stack_summary() -> Vec<String> {
    CHECKPOINT_STACK.lock()
        .map(|stack| {
            stack.iter().rev().enumerate().map(|(i, cp)| {
                let file_count = cp.snapshots.len();
                format!("  {}. {} ({} file(s))", i + 1, cp.description, file_count)
            }).collect()
        })
        .unwrap_or_default()
}

/// Clear all checkpoints (called on /new session).
pub fn clear() {
    if let Ok(mut stack) = CHECKPOINT_STACK.lock() {
        stack.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_snapshot_and_undo() {
        // Clear the stack to avoid interference from parallel tests
        clear();

        let dir = std::env::temp_dir().join("daedalus_checkpoint_test");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let file = dir.join("test.txt");

        // Write initial content
        tokio::fs::write(&file, "original content").await.unwrap();

        // Snapshot before modification
        snapshot_before_write(&file, "edit_file test.txt").await;

        // Modify the file
        tokio::fs::write(&file, "modified content").await.unwrap();
        assert_eq!(
            tokio::fs::read_to_string(&file).await.unwrap(),
            "modified content"
        );

        // Undo
        let result = undo().await.unwrap();
        assert!(result.contains("edit_file test.txt"), "Unexpected undo result: {}", result);
        assert_eq!(
            tokio::fs::read_to_string(&file).await.unwrap(),
            "original content"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_undo_new_file() {
        // Clear the stack to avoid interference from parallel tests
        clear();

        let dir = std::env::temp_dir().join("daedalus_checkpoint_new_test");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let file = dir.join("new_file.txt");

        // Snapshot before creation (file doesn't exist)
        snapshot_before_write(&file, "write_file new_file.txt").await;

        // Create the file
        tokio::fs::write(&file, "new content").await.unwrap();
        assert!(file.exists());

        // Undo — should remove the file
        let result = undo().await.unwrap();
        assert!(result.contains("removed"));
        assert!(!file.exists());

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_undo_empty_stack() {
        // Clear the stack first
        clear();
        let result = undo().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Nothing to undo"));
    }

    #[test]
    fn test_undo_count() {
        clear();
        assert_eq!(undo_count(), 0);
    }
}
