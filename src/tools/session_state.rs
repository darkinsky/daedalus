//! Session-scoped mutable state for built-in tools.
//!
//! Provides a centralized `reset_session_state()` function that clears all
//! session-scoped global state when a new session starts. This ensures:
//!
//! 1. No stale state leaks across sessions
//! 2. All resets happen in one place (single point of control)
//! 3. Future migration to dependency-injected state is straightforward
//!
//! ## Current global state managed here:
//!
//! | State | File | Purpose |
//! |-------|------|---------|
//! | `MODIFIED_FILES` | `edit_file.rs` | Tracks dirty files for context |
//! | `MODIFIED_FILES` | `multi_edit.rs` | Same (duplicate set) |
//! | `CHECKPOINT_STACK` | `checkpoint.rs` | Undo stack |
//! | `REDO_STACK` | `checkpoint.rs` | Redo stack |
//! | `EDITING_FILES` | `fs_utils.rs` | Concurrent edit guard |
//!
//! ## Usage
//!
//! ```ignore
//! // Called when a new session starts:
//! crate::tools::session_state::reset_session_state();
//! ```

use super::checkpoint;
use super::edit_file;
use super::fs_utils::EDITING_FILES;

/// Reset all session-scoped tool state.
///
/// Must be called when starting a new session to prevent stale state
/// from leaking across sessions. This is the **single point of control**
/// for all tool-level session resets.
///
/// Currently resets:
/// - Checkpoint stacks (undo/redo history)
/// - Modified files tracking
/// - Concurrent edit guards (in case of stale locks)
pub fn reset_session_state() {
    // 1. Clear undo/redo history
    checkpoint::clear();

    // 2. Clear modified files tracking
    edit_file::clear_modified_files();

    // 3. Clear concurrent edit guards (defensive — should already be empty)
    if let Ok(mut set) = EDITING_FILES.lock() {
        set.clear();
    }

    tracing::debug!("Session tool state reset");
}
