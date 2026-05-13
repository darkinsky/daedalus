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

/// Global checkpoint stack (undo operations).
static CHECKPOINT_STACK: Lazy<Mutex<Vec<Checkpoint>>> = Lazy::new(|| Mutex::new(Vec::new()));

/// Separate redo stack — stores reverse checkpoints after undo.
/// This prevents redo entries from polluting the undo stack.
static REDO_STACK: Lazy<Mutex<Vec<Checkpoint>>> = Lazy::new(|| Mutex::new(Vec::new()));

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
/// to their pre-modification state. Before restoring, a reverse checkpoint
/// is created so the user can "redo" (undo the undo) if needed.
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

    // ── Create a reverse checkpoint (for redo support) ──
    // Snapshot the *current* state of all files before restoring, so the user
    // can undo the undo.
    let mut reverse_snapshots = HashMap::new();
    for (path, _) in &checkpoint.snapshots {
        let current_content = match tokio::fs::read_to_string(path).await {
            Ok(c) => Some(c),
            Err(_) => None, // File doesn't exist currently
        };
        reverse_snapshots.insert(path.clone(), FileSnapshot { content: current_content });
    }

    // ── Restore files atomically ──
    // Track which paths were successfully restored for rollback on failure.
    let mut restored_paths: Vec<PathBuf> = Vec::new();
    let mut restored_messages: Vec<String> = Vec::new();
    let mut restore_error: Option<String> = None;

    for (path, snapshot) in &checkpoint.snapshots {
        let result = match &snapshot.content {
            Some(content) => {
                tokio::fs::write(path, content).await
                    .map(|_| format!("  restored: {}", path.display()))
                    .map_err(|e| format!("Failed to restore {}: {}", path.display(), e))
            }
            None => {
                // File didn't exist before — remove it
                if path.exists() {
                    tokio::fs::remove_file(path).await
                        .map(|_| format!("  removed: {} (was newly created)", path.display()))
                        .map_err(|e| format!("Failed to remove {}: {}", path.display(), e))
                } else {
                    Ok(format!("  skipped: {} (already absent)", path.display()))
                }
            }
        };

        match result {
            Ok(msg) => {
                restored_paths.push(path.clone());
                restored_messages.push(msg);
            }
            Err(e) => {
                restore_error = Some(e);
                break;
            }
        }
    }

    // If a restore failed, roll back already-restored files using their paths directly
    if let Some(ref err_msg) = restore_error {
        tracing::warn!("Undo partially failed, rolling back: {}", err_msg);
        for path in &restored_paths {
            if let Some(rev_snapshot) = reverse_snapshots.get(path) {
                match &rev_snapshot.content {
                    Some(content) => {
                        let _ = tokio::fs::write(path, content).await;
                    }
                    None => {
                        if path.exists() {
                            let _ = tokio::fs::remove_file(path).await;
                        }
                    }
                }
            }
        }
        // Push the original checkpoint back since undo failed
        push_checkpoint(&checkpoint.description, checkpoint.snapshots.clone());
        return Err(anyhow::anyhow!("Undo failed (rolled back): {}", err_msg));
    }

    // Push the reverse checkpoint to the REDO stack (not the undo stack)
    push_redo_checkpoint(
        &format!("redo: {}", checkpoint.description),
        reverse_snapshots,
    );

    let remaining = CHECKPOINT_STACK.lock()
        .map(|s| s.len())
        .unwrap_or(0);

    Ok(format!(
        "Undid: {}\n{}\n({} more undo(s) available)",
        checkpoint.description,
        restored_messages.join("\n"),
        remaining,
    ))
}

/// Push a checkpoint onto the redo stack.
fn push_redo_checkpoint(description: &str, snapshots: HashMap<PathBuf, FileSnapshot>) {
    let checkpoint = Checkpoint {
        description: description.to_string(),
        snapshots,
        created_at: std::time::Instant::now(),
    };

    if let Ok(mut stack) = REDO_STACK.lock() {
        stack.push(checkpoint);
        // Keep redo stack bounded
        if stack.len() > MAX_CHECKPOINTS {
            stack.remove(0);
        }
    }
}

/// Return the number of available undo operations.
#[allow(dead_code)]
pub fn undo_count() -> usize {
    CHECKPOINT_STACK.lock()
        .map(|s| s.len())
        .unwrap_or(0)
}

/// Return the number of available redo operations.
#[allow(dead_code)]
pub fn redo_count() -> usize {
    REDO_STACK.lock()
        .map(|s| s.len())
        .unwrap_or(0)
}

/// Redo the most recently undone file modification.
///
/// Pops the most recent entry from the redo stack, snapshots the current
/// file state (pushing it onto the undo stack), then restores the redo
/// checkpoint's file contents.
///
/// Returns a human-readable description of what was redone, or an error
/// if the redo stack is empty.
#[allow(dead_code)]
pub async fn redo() -> anyhow::Result<String> {
    let checkpoint = {
        let mut stack = REDO_STACK.lock()
            .map_err(|_| anyhow::anyhow!("Failed to acquire redo lock"))?;
        stack.pop()
    };

    let checkpoint = checkpoint
        .ok_or_else(|| anyhow::anyhow!("Nothing to redo — no undone modifications to restore."))?;

    // Snapshot the current state before restoring, so the user can undo the redo.
    let mut reverse_snapshots = HashMap::new();
    for (path, _) in &checkpoint.snapshots {
        let current_content = match tokio::fs::read_to_string(path).await {
            Ok(c) => Some(c),
            Err(_) => None,
        };
        reverse_snapshots.insert(path.clone(), FileSnapshot { content: current_content });
    }

    // Restore files from the redo checkpoint
    let mut restored_messages: Vec<String> = Vec::new();
    for (path, snapshot) in &checkpoint.snapshots {
        let result = match &snapshot.content {
            Some(content) => {
                tokio::fs::write(path, content).await
                    .map(|_| format!("  restored: {}", path.display()))
                    .map_err(|e| format!("Failed to restore {}: {}", path.display(), e))
            }
            None => {
                if path.exists() {
                    tokio::fs::remove_file(path).await
                        .map(|_| format!("  removed: {}", path.display()))
                        .map_err(|e| format!("Failed to remove {}: {}", path.display(), e))
                } else {
                    Ok(format!("  skipped: {} (already absent)", path.display()))
                }
            }
        };

        match result {
            Ok(msg) => restored_messages.push(msg),
            Err(e) => return Err(anyhow::anyhow!("Redo failed: {}", e)),
        }
    }

    // Push the reverse snapshot onto the UNDO stack (so user can undo the redo)
    push_checkpoint(
        &format!("undo of: {}", checkpoint.description),
        reverse_snapshots,
    );

    let remaining = REDO_STACK.lock()
        .map(|s| s.len())
        .unwrap_or(0);

    Ok(format!(
        "Redid: {}\n{}\n({} more redo(s) available)",
        checkpoint.description,
        restored_messages.join("\n"),
        remaining,
    ))
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
    if let Ok(mut stack) = REDO_STACK.lock() {
        stack.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shared mutex to serialize tests that use the global CHECKPOINT_STACK.
    /// Without this, parallel test execution causes race conditions where
    /// one test's `clear()` + `snapshot` + `undo` sequence is interleaved
    /// with another test's operations on the same global stack.
    ///
    /// Uses `std::sync::Mutex` (not `tokio::sync::Mutex`) because each
    /// `#[tokio::test]` may run on a different tokio runtime instance.
    /// We use `unwrap_or_else(|e| e.into_inner())` to recover from poison
    /// (a previous test panicked while holding the lock).
    static TEST_MUTEX: Lazy<std::sync::Mutex<()>> = Lazy::new(|| std::sync::Mutex::new(()));

    /// Acquire the test serialization lock, recovering from poison.
    fn lock_tests() -> std::sync::MutexGuard<'static, ()> {
        TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[tokio::test]
    async fn test_snapshot_and_undo() {
        let _guard = lock_tests();
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

        // Undo — may pop a different checkpoint if other tests pushed to the
        // global stack concurrently. Keep popping until we find ours or the
        // stack is empty.
        let mut found = false;
        for _ in 0..10 {
            match undo().await {
                Ok(result) => {
                    if result.contains("edit_file test.txt") {
                        found = true;
                        break;
                    }
                    // Not our checkpoint — continue popping
                }
                Err(_) => break, // Stack empty
            }
        }
        assert!(found, "Our checkpoint was not found in the undo stack");
        assert_eq!(
            tokio::fs::read_to_string(&file).await.unwrap(),
            "original content"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_undo_new_file() {
        let _guard = lock_tests();
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

        // Undo — keep popping until we find our checkpoint
        let mut found = false;
        for _ in 0..10 {
            match undo().await {
                Ok(result) => {
                    if result.contains("write_file new_file.txt") {
                        found = true;
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        assert!(found, "Our checkpoint was not found in the undo stack");
        assert!(!file.exists());

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_undo_empty_stack() {
        let _guard = lock_tests();
        // Clear the stack first
        clear();
        let result = undo().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Nothing to undo"));
    }

    #[tokio::test]
    async fn test_undo_count() {
        let _guard = lock_tests();
        clear();
        assert_eq!(undo_count(), 0);
    }
}
