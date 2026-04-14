use std::path::Path;

use anyhow::{Context, Result};

/// Trait for memory components that can be persisted to disk.
///
/// Implementations should handle the case where the file doesn't exist
/// (returning a default value) and should use `atomic_write` for crash-safe writes.
pub trait MemoryPersistence: Sized {
    /// Save the current state to the given path.
    ///
    /// Creates parent directories if they don't exist.
    /// Implementations should use `atomic_write` for crash safety.
    fn save(&self, path: &Path) -> Result<()>;

    /// Load state from the given path.
    ///
    /// Returns a default instance if the file doesn't exist.
    fn load(path: &Path) -> Result<Self>;
}

/// Write data to a file atomically using the write-to-temp-then-rename pattern.
///
/// This ensures that the target file is never left in a partially-written state
/// if the process crashes mid-write. The sequence is:
/// 1. Write data to `<path>.tmp`
/// 2. Rename `<path>.tmp` → `<path>` (atomic on most filesystems)
///
/// Creates parent directories if they don't exist.
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
    }
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, data)
        .with_context(|| format!("Failed to write temp file: {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, path)
        .with_context(|| format!("Failed to rename {} → {}", tmp_path.display(), path.display()))?;
    Ok(())
}
