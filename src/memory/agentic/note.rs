use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

/// A single memory note in the A-MEM system.
///
/// Inspired by the Zettelkasten (slip-box) method and the A-MEM paper
/// (arxiv:2502.12110), each note is an atomic, self-contained unit of
/// knowledge with rich metadata and inter-note links.
///
/// ## Structure
///
/// Each note contains:
/// - **Raw content**: The original information that triggered this note.
/// - **LLM-generated metadata**: Keywords, tags, category, and contextual
///   description that capture the semantic essence of the content.
/// - **Embedding vector**: Dense vector representation for similarity search.
/// - **Links**: Bidirectional connections to semantically related notes,
///   forming an evolving knowledge graph.
/// - **Access tracking**: `retrieval_count` and `last_accessed` for
///   frequency-based prioritization (matching the paper's design).
/// - **Evolution history**: Audit trail of how the note has been refined.
///
/// ## Lifecycle
///
/// 1. **Construction**: Raw content → LLM extracts keywords/tags/context →
///    embedding model generates vector.
/// 2. **Linking**: Cosine similarity finds candidates → LLM validates and
///    establishes semantic links.
/// 3. **Evolution**: When new related notes are added, existing notes'
///    context, keywords, and tags may be updated to reflect higher-order
///    knowledge patterns.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct MemoryNote {
    /// Unique identifier for this note.
    pub id: Uuid,
    /// Timestamp when this note was created.
    pub created_at: DateTime<Local>,
    /// Timestamp of the last update (evolution) to this note.
    pub updated_at: DateTime<Local>,
    /// Timestamp of the last retrieval access.
    pub last_accessed: DateTime<Local>,
    /// The original raw content that this note captures.
    pub content: String,
    /// LLM-generated keywords that capture the key concepts.
    pub keywords: Vec<String>,
    /// LLM-generated tags for categorical classification.
    pub tags: Vec<String>,
    /// High-level category (e.g., "user_preference", "project_context",
    /// "technical_decision"). Defaults to "uncategorized".
    #[serde(default = "default_category")]
    pub category: String,
    /// LLM-generated contextual description that summarizes the note's
    /// significance and relationship to broader knowledge.
    pub context: String,
    /// Dense vector embedding of the note content for similarity search.
    pub embedding: Vec<f32>,
    /// IDs of linked (related) notes, forming a knowledge graph.
    ///
    /// Uses `HashSet` for O(1) deduplication. The number of links per note
    /// can grow as the knowledge graph evolves.
    pub linked_notes: HashSet<Uuid>,
    /// How many times this note has been retrieved in a search.
    /// Used for frequency-based prioritization in retrieval ranking.
    #[serde(default)]
    pub retrieval_count: u32,
    /// History of evolution events for this note (audit trail).
    /// Each entry is a brief description of what changed and when.
    #[serde(default)]
    pub evolution_history: Vec<String>,
}

fn default_category() -> String {
    "uncategorized".to_string()
}

#[allow(dead_code)]
impl MemoryNote {
    /// Create a new memory note with all fields populated.
    ///
    /// This is typically called by `AgenticMemoryStore::add_memory()` after
    /// the LLM has generated keywords/tags/context and the embedding model
    /// has produced the vector.
    pub fn new(
        content: String,
        keywords: Vec<String>,
        tags: Vec<String>,
        context: String,
        embedding: Vec<f32>,
    ) -> Self {
        let now = Local::now();
        Self {
            id: Uuid::new_v4(),
            created_at: now,
            updated_at: now,
            last_accessed: now,
            content,
            keywords,
            tags,
            category: "uncategorized".to_string(),
            context,
            embedding,
            linked_notes: HashSet::new(),
            retrieval_count: 0,
            evolution_history: Vec::new(),
        }
    }

    /// Create a new memory note with a specific category.
    pub fn with_category(
        content: String,
        keywords: Vec<String>,
        tags: Vec<String>,
        context: String,
        category: String,
        embedding: Vec<f32>,
    ) -> Self {
        let mut note = Self::new(content, keywords, tags, context, embedding);
        note.category = category;
        note
    }

    /// Add a bidirectional link to another note.
    ///
    /// Returns `true` if the link was newly added, `false` if it already existed.
    pub fn add_link(&mut self, other_id: Uuid) -> bool {
        self.linked_notes.insert(other_id)
    }

    /// Record a retrieval access (increment count and update timestamp).
    pub fn record_access(&mut self) {
        self.retrieval_count += 1;
        self.last_accessed = Local::now();
    }

    /// Update the note's metadata during memory evolution.
    ///
    /// When a new related note is added, the LLM may re-analyze this note
    /// and produce updated keywords, tags, and context that reflect
    /// higher-order patterns. The change is recorded in evolution_history.
    pub fn evolve(&mut self, keywords: Vec<String>, tags: Vec<String>, context: String) {
        let history_entry = format!(
            "[{}] Evolved: keywords {} → {}, context updated",
            Local::now().format("%Y-%m-%d %H:%M"),
            self.keywords.join(","),
            keywords.join(","),
        );
        self.keywords = keywords;
        self.tags = tags;
        self.context = context;
        self.updated_at = Local::now();
        self.evolution_history.push(history_entry);
    }

    /// Format this note as a compact text representation for LLM prompts.
    ///
    /// Used when providing memory context to the LLM for note construction,
    /// link generation, or memory evolution.
    pub fn to_prompt_text(&self) -> String {
        let keywords_str = self.keywords.join(", ");
        let tags_str = self.tags.join(", ");
        let links_count = self.linked_notes.len();

        format!(
            "[Note {}]\nContent: {}\nKeywords: {}\nTags: {}\nCategory: {}\nContext: {}\nLinks: {} connected notes",
            &self.id.to_string()[..8],
            self.content,
            keywords_str,
            tags_str,
            self.category,
            self.context,
            links_count,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_note() {
        let note = MemoryNote::new(
            "User prefers Rust for systems programming".to_string(),
            vec!["rust".to_string(), "systems".to_string()],
            vec!["preference".to_string(), "language".to_string()],
            "User has expressed a strong preference for Rust".to_string(),
            vec![0.1, 0.2, 0.3],
        );

        assert_eq!(note.content, "User prefers Rust for systems programming");
        assert_eq!(note.keywords.len(), 2);
        assert_eq!(note.tags.len(), 2);
        assert!(note.linked_notes.is_empty());
        assert_eq!(note.embedding.len(), 3);
        assert_eq!(note.retrieval_count, 0);
        assert_eq!(note.category, "uncategorized");
        assert!(note.evolution_history.is_empty());
    }

    #[test]
    fn test_add_link() {
        let mut note = MemoryNote::new(
            "test".to_string(),
            vec![],
            vec![],
            "test context".to_string(),
            vec![],
        );
        let other_id = Uuid::new_v4();

        assert!(note.add_link(other_id));
        assert!(!note.add_link(other_id)); // duplicate
        assert_eq!(note.linked_notes.len(), 1);
    }

    #[test]
    fn test_record_access() {
        let mut note = MemoryNote::new(
            "test".to_string(), vec![], vec![], "ctx".to_string(), vec![],
        );
        assert_eq!(note.retrieval_count, 0);
        note.record_access();
        assert_eq!(note.retrieval_count, 1);
        note.record_access();
        assert_eq!(note.retrieval_count, 2);
    }

    #[test]
    fn test_evolve() {
        let mut note = MemoryNote::new(
            "test".to_string(),
            vec!["old".to_string()],
            vec!["old_tag".to_string()],
            "old context".to_string(),
            vec![],
        );
        let original_created = note.created_at;

        note.evolve(
            vec!["new".to_string(), "evolved".to_string()],
            vec!["new_tag".to_string()],
            "evolved context with new insights".to_string(),
        );

        assert_eq!(note.keywords, vec!["new", "evolved"]);
        assert_eq!(note.tags, vec!["new_tag"]);
        assert_eq!(note.context, "evolved context with new insights");
        assert_eq!(note.created_at, original_created);
        assert!(note.updated_at >= note.created_at);
        assert_eq!(note.evolution_history.len(), 1);
        assert!(note.evolution_history[0].contains("Evolved"));
    }

    #[test]
    fn test_to_prompt_text() {
        let note = MemoryNote::new(
            "User prefers Rust".to_string(),
            vec!["rust".to_string(), "preference".to_string()],
            vec!["lang".to_string()],
            "Strong Rust preference".to_string(),
            vec![0.1],
        );

        let text = note.to_prompt_text();
        assert!(text.contains("User prefers Rust"));
        assert!(text.contains("rust, preference"));
        assert!(text.contains("lang"));
        assert!(text.contains("Strong Rust preference"));
        assert!(text.contains("0 connected notes"));
        assert!(text.contains("Category:"));
    }

    #[test]
    fn test_with_category() {
        let note = MemoryNote::with_category(
            "test".to_string(),
            vec![], vec![], "ctx".to_string(),
            "user_preference".to_string(),
            vec![],
        );
        assert_eq!(note.category, "user_preference");
    }
}
