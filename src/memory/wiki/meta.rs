use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Machine-only metadata stored in `_meta.json`.
///
/// Contains data that is essential for the wiki engine but not
/// human-readable or suitable for Markdown files:
/// - Embedding vectors (dense float arrays)
/// - Lint state (turn counter)
/// - Schema version for forward compatibility
///
/// This file is NOT meant to be edited by users. It is managed
/// exclusively by the wiki engine.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WikiMeta {
    /// Embedding vectors indexed by page ID.
    ///
    /// Each page's embedding is stored here rather than in the `.md` file
    /// because embedding vectors are pure machine data (large float arrays)
    /// that would clutter the human-readable Markdown.
    #[serde(default)]
    pub embeddings: HashMap<String, Vec<f32>>,

    /// Number of conversation turns since the last lint operation.
    ///
    /// Used to trigger periodic lint checks (every N turns).
    #[serde(default)]
    pub last_lint_turn: usize,

    /// Schema version for forward compatibility.
    ///
    /// Allows future migrations if the wiki format changes.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
}

fn default_schema_version() -> u32 {
    1
}

impl WikiMeta {
    /// Create a new empty metadata instance.
    pub fn new() -> Self {
        Self {
            embeddings: HashMap::new(),
            last_lint_turn: 0,
            schema_version: default_schema_version(),
        }
    }

    /// Remove embeddings for page IDs that no longer exist.
    ///
    /// Called during load to clean up orphaned entries after
    /// users manually delete `.md` files.
    pub fn cleanup_orphaned_embeddings(&mut self, existing_page_ids: &[&str]) {
        let valid_ids: std::collections::HashSet<&str> =
            existing_page_ids.iter().copied().collect();
        self.embeddings.retain(|id, _| valid_ids.contains(id.as_str()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_meta() {
        let meta = WikiMeta::new();
        assert!(meta.embeddings.is_empty());
        assert_eq!(meta.last_lint_turn, 0);
        assert_eq!(meta.schema_version, 1);
    }

    #[test]
    fn test_cleanup_orphaned_embeddings() {
        let mut meta = WikiMeta::new();
        meta.embeddings.insert("page-a".to_string(), vec![0.1]);
        meta.embeddings.insert("page-b".to_string(), vec![0.2]);
        meta.embeddings.insert("page-c".to_string(), vec![0.3]);

        // Only page-a and page-c still exist
        meta.cleanup_orphaned_embeddings(&["page-a", "page-c"]);

        assert!(meta.embeddings.contains_key("page-a"));
        assert!(!meta.embeddings.contains_key("page-b"));
        assert!(meta.embeddings.contains_key("page-c"));
    }

    #[test]
    fn test_serde_roundtrip() {
        let mut meta = WikiMeta::new();
        meta.embeddings.insert("test".to_string(), vec![0.1, 0.2, 0.3]);
        meta.last_lint_turn = 42;

        let json = serde_json::to_string(&meta).unwrap();
        let parsed: WikiMeta = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.embeddings.len(), 1);
        assert_eq!(parsed.last_lint_turn, 42);
        assert_eq!(parsed.schema_version, 1);
    }
}
