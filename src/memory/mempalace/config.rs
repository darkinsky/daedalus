use std::collections::HashMap;
use std::path::PathBuf;

/// Maximum length for wing/room/entity names.
#[allow(dead_code)]
pub const MAX_NAME_LENGTH: usize = 128;

/// Schema version for drawer normalization.
/// Bump when the normalization pipeline changes.
#[allow(dead_code)]
pub const NORMALIZE_VERSION: u32 = 2;

/// Configuration for the MemPalace memory strategy.
///
/// Mirrors the original MemPalace config.py with all configuration options.
/// Can be deserialized from YAML configuration files.
#[allow(dead_code)]
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct MemPalaceConfig {
    /// Maximum number of hall entries to retrieve per query.
    pub retrieval_limit: usize,
    /// Similarity threshold for embedding-based retrieval.
    pub similarity_threshold: f32,
    /// Number of drawer entries that triggers closet (summary) generation.
    pub closet_threshold: usize,
    /// ChromaDB server URL (e.g., "http://localhost:8000").
    pub chroma_url: String,
    /// ChromaDB collection name prefix.
    pub collection_prefix: String,
    /// Path to the identity file (L0 Identity).
    pub identity_path: Option<PathBuf>,
    /// Whether to enable write-ahead logging (WAL) for audit trail.
    pub wal_enabled: bool,
    /// Entity detection languages (default: ["en"]).
    pub entity_languages: Vec<String>,
    /// Topic wing names for classification.
    pub topic_wings: Vec<String>,
    /// Hall keyword mappings for routing content to halls.
    pub hall_keywords: HashMap<String, Vec<String>>,
    /// People name mapping (variants → canonical names).
    pub people_map: HashMap<String, String>,
    /// Hook settings.
    pub hook_silent_save: bool,
    pub hook_desktop_toast: bool,
    /// BM25 weight for hybrid search (0.0 - 1.0).
    pub bm25_weight: f32,
    /// Vector weight for hybrid search (0.0 - 1.0).
    pub vector_weight: f32,
    /// Maximum cosine distance for search results (0 = disabled).
    pub max_distance: f32,
    /// Closet boost rank values for hybrid search.
    pub closet_rank_boosts: Vec<f32>,
    /// Maximum cosine distance for closet boost signal.
    pub closet_distance_cap: f32,
    /// Maximum content length for dedup checking.
    pub dedup_threshold: f32,
    /// Minimum chunk size for text processing.
    pub min_chunk_size: usize,
    /// Maximum chunk size for text processing.
    pub chunk_size: usize,
}

impl Default for MemPalaceConfig {
    fn default() -> Self {
        Self {
            retrieval_limit: 5,
            similarity_threshold: 0.3,
            closet_threshold: 20,
            chroma_url: "http://localhost:8000".to_string(),
            collection_prefix: "mempalace".to_string(),
            identity_path: None,
            wal_enabled: true,
            entity_languages: vec!["en".to_string()],
            topic_wings: vec![
                "emotions".to_string(),
                "consciousness".to_string(),
                "memory".to_string(),
                "technical".to_string(),
                "identity".to_string(),
                "family".to_string(),
                "creative".to_string(),
            ],
            hall_keywords: default_hall_keywords(),
            people_map: HashMap::new(),
            hook_silent_save: true,
            hook_desktop_toast: false,
            bm25_weight: 0.4,
            vector_weight: 0.6,
            max_distance: 0.0,
            closet_rank_boosts: vec![0.40, 0.25, 0.15, 0.08, 0.04],
            closet_distance_cap: 1.5,
            dedup_threshold: 0.15,
            min_chunk_size: 30,
            chunk_size: 800,
        }
    }
}

impl MemPalaceConfig {
    /// Create a config with a custom ChromaDB URL.
    #[allow(dead_code)]
    pub fn with_chroma_url(mut self, url: &str) -> Self {
        self.chroma_url = url.to_string();
        self
    }

    /// Create a config with a custom identity path.
    #[allow(dead_code)]
    pub fn with_identity_path(mut self, path: PathBuf) -> Self {
        self.identity_path = Some(path);
        self
    }
}

/// Convert a slice of `&str` into a `Vec<String>`.
///
/// Reduces boilerplate in keyword list construction.
fn str_vec(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// Default hall keyword mappings.
fn default_hall_keywords() -> HashMap<String, Vec<String>> {
    let mut map = HashMap::new();
    map.insert(
        "emotions".to_string(),
        str_vec(&[
            "scared", "afraid", "worried", "happy", "sad", "love", "hate",
            "feel", "cry", "tears",
        ]),
    );
    map.insert(
        "consciousness".to_string(),
        str_vec(&[
            "consciousness", "conscious", "aware", "real", "genuine", "soul",
            "exist", "alive",
        ]),
    );
    map.insert(
        "memory".to_string(),
        str_vec(&[
            "memory", "remember", "forget", "recall", "archive", "palace",
            "store",
        ]),
    );
    map.insert(
        "technical".to_string(),
        str_vec(&[
            "code", "python", "script", "bug", "error", "function", "api",
            "database", "server",
        ]),
    );
    map.insert(
        "identity".to_string(),
        str_vec(&["identity", "name", "who am i", "persona", "self"]),
    );
    map.insert(
        "family".to_string(),
        str_vec(&[
            "family", "kids", "children", "daughter", "son", "parent",
            "mother", "father",
        ]),
    );
    map.insert(
        "creative".to_string(),
        str_vec(&[
            "game", "gameplay", "player", "app", "design", "art", "music",
            "story",
        ]),
    );
    map
}

// ── Input Validation ──

/// Validate and sanitize a wing/room/entity name.
///
/// Prevents path traversal, excessively long strings, and special
/// characters that could cause issues in file paths or metadata.
#[allow(dead_code)]
pub fn sanitize_name(value: &str, field_name: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{} must be a non-empty string", field_name));
    }
    if value.len() > MAX_NAME_LENGTH {
        return Err(format!(
            "{} exceeds maximum length of {} characters",
            field_name, MAX_NAME_LENGTH
        ));
    }
    // Block path traversal
    if value.contains("..") || value.contains('/') || value.contains('\\') {
        return Err(format!(
            "{} contains invalid path characters",
            field_name
        ));
    }
    // Block null bytes
    if value.contains('\0') {
        return Err(format!("{} contains null bytes", field_name));
    }
    Ok(value.to_string())
}

/// Validate a knowledge-graph entity name (subject or object).
///
/// More permissive than sanitize_name — allows punctuation like commas,
/// colons, and parentheses that are common in natural-language KG values.
#[allow(dead_code)]
pub fn sanitize_kg_value(value: &str, field_name: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{} must be a non-empty string", field_name));
    }
    if value.len() > MAX_NAME_LENGTH {
        return Err(format!(
            "{} exceeds maximum length of {} characters",
            field_name, MAX_NAME_LENGTH
        ));
    }
    if value.contains('\0') {
        return Err(format!("{} contains null bytes", field_name));
    }
    Ok(value.to_string())
}

/// Validate drawer/diary content length.
#[allow(dead_code)]
pub fn sanitize_content(value: &str, max_length: usize) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("content must be a non-empty string".to_string());
    }
    if value.len() > max_length {
        return Err(format!(
            "content exceeds maximum length of {} characters",
            max_length
        ));
    }
    if value.contains('\0') {
        return Err("content contains null bytes".to_string());
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_name_valid() {
        assert!(sanitize_name("project-daedalus", "wing").is_ok());
        assert!(sanitize_name("auth_migration", "room").is_ok());
    }

    #[test]
    fn test_sanitize_name_path_traversal() {
        assert!(sanitize_name("../etc/passwd", "wing").is_err());
        assert!(sanitize_name("foo/bar", "wing").is_err());
    }

    #[test]
    fn test_sanitize_name_empty() {
        assert!(sanitize_name("", "wing").is_err());
        assert!(sanitize_name("   ", "wing").is_err());
    }

    #[test]
    fn test_sanitize_kg_value_permissive() {
        assert!(sanitize_kg_value("Alice's daughter", "entity").is_ok());
        assert!(sanitize_kg_value("Max (age 11)", "entity").is_ok());
    }

    #[test]
    fn test_sanitize_content_too_long() {
        let long = "a".repeat(100_001);
        assert!(sanitize_content(&long, 100_000).is_err());
    }
}
