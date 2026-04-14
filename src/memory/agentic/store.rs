use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use uuid::Uuid;

use crate::embedding::{cosine_similarity, Embedding};
use crate::llm::LlmApi;
use crate::memory::persistence::MemoryPersistence;

use super::note::MemoryNote;
use super::prompts::{
    evolution_prompt, link_validation_prompt, metadata_extraction_prompt,
    EVOLUTION_SYSTEM_PROMPT, LINK_VALIDATION_SYSTEM_PROMPT, METADATA_SYSTEM_PROMPT,
};

/// Default maximum number of candidate notes to retrieve for link generation.
const DEFAULT_MAX_LINK_CANDIDATES: usize = 5;

/// Default similarity threshold for considering a note as a link candidate.
const DEFAULT_SIMILARITY_THRESHOLD: f32 = 0.5;

/// Default number of notes to retrieve for context-aware retrieval.
const DEFAULT_RETRIEVAL_LIMIT: usize = 5;

// ── Parsing helpers ──

/// Strip a prefix from a string in a case-insensitive manner, returning
/// the remainder from the *original* (non-lowercased) string.
///
/// This avoids the subtle `offset = line.len() - rest.len()` trick that
/// relies on ASCII case-conversion preserving byte length.
fn strip_prefix_case_insensitive<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    if line.len() >= prefix.len()
        && line[..prefix.len()].eq_ignore_ascii_case(prefix)
    {
        Some(&line[prefix.len()..])
    } else {
        None
    }
}

/// Parse a comma-separated string into a `Vec<String>`, trimming whitespace
/// and stripping leading Markdown bold markers (`*`).
fn parse_comma_separated(s: &str) -> Vec<String> {
    s.trim_start_matches('*')
        .split(',')
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

/// Agentic Memory Store — the core A-MEM engine.
///
/// Implements the three-phase memory lifecycle from the A-MEM paper
/// (arxiv:2502.12110):
///
/// 1. **Note Construction**: New information → LLM extracts structured
///    metadata (keywords, tags, context) → embedding model generates vector.
/// 2. **Link Generation**: Cosine similarity retrieves candidate notes →
///    LLM analyzes semantic relationships → bidirectional links established.
/// 3. **Memory Evolution**: Related notes' metadata is updated by the LLM
///    to reflect higher-order knowledge patterns.
///
/// The store maintains an in-memory collection of `MemoryNote`s indexed by
/// UUID, with embedding vectors for fast similarity search.
#[allow(dead_code)]
pub struct AgenticMemoryStore {
    /// All memory notes, indexed by their unique ID.
    notes: HashMap<Uuid, MemoryNote>,
    /// Similarity threshold for link candidate selection.
    similarity_threshold: f32,
    /// Maximum number of top candidates to consider for linking.
    max_link_candidates: usize,
    /// Default number of notes to return from retrieval.
    retrieval_limit: usize,
}

#[allow(dead_code)]
impl AgenticMemoryStore {
    /// Create a new empty agentic memory store with default settings.
    pub fn new() -> Self {
        Self {
            notes: HashMap::new(),
            similarity_threshold: DEFAULT_SIMILARITY_THRESHOLD,
            max_link_candidates: DEFAULT_MAX_LINK_CANDIDATES,
            retrieval_limit: DEFAULT_RETRIEVAL_LIMIT,
        }
    }

    /// Create a store with custom thresholds.
    #[allow(dead_code)]
    pub fn with_config(
        similarity_threshold: f32,
        max_link_candidates: usize,
        retrieval_limit: usize,
    ) -> Self {
        Self {
            notes: HashMap::new(),
            similarity_threshold,
            max_link_candidates,
            retrieval_limit,
        }
    }

    /// Return the number of notes in the store.
    pub fn len(&self) -> usize {
        self.notes.len()
    }

    /// Check if the store is empty.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }

    /// Get a reference to a note by ID.
    #[allow(dead_code)]
    pub fn get_note(&self, id: &Uuid) -> Option<&MemoryNote> {
        self.notes.get(id)
    }

    /// Get all notes as a slice-like iterator.
    #[allow(dead_code)]
    pub fn all_notes(&self) -> impl Iterator<Item = &MemoryNote> {
        self.notes.values()
    }

    // ── Phase 1: Note Construction ──

    /// Add a new memory from raw content.
    ///
    /// This is the main entry point for the A-MEM lifecycle:
    /// 1. LLM extracts keywords, tags, and context from the content.
    /// 2. Embedding model generates a vector for the content.
    /// 3. A new `MemoryNote` is created and stored.
    /// 4. Link generation is performed against existing notes.
    /// 5. Memory evolution updates related notes' metadata.
    ///
    /// Returns the ID of the newly created note.
    pub async fn add_memory(
        &mut self,
        content: &str,
        llm: &dyn LlmApi,
        embedder: &dyn Embedding,
    ) -> Result<Uuid> {
        // Phase 1: Note Construction
        let (keywords, tags, context) = self
            .extract_metadata(content, llm)
            .await
            .context("Failed to extract metadata for new memory")?;

        let embedding = embedder
            .embed(content)
            .await
            .context("Failed to generate embedding for new memory")?;

        let note = MemoryNote::new(
            content.to_string(),
            keywords,
            tags,
            context,
            embedding,
        );
        let note_id = note.id;

        tracing::debug!(
            note_id = %note_id,
            keywords = ?note.keywords,
            tags = ?note.tags,
            "Created new memory note"
        );

        self.notes.insert(note_id, note);

        // Phase 2: Link Generation
        if let Err(e) = self.generate_links(note_id, llm).await {
            tracing::warn!(
                note_id = %note_id,
                error = %e,
                "Link generation failed, note stored without links"
            );
        }

        // Phase 3: Memory Evolution
        if let Err(e) = self.evolve_related_notes(note_id, llm).await {
            tracing::warn!(
                note_id = %note_id,
                error = %e,
                "Memory evolution failed, related notes not updated"
            );
        }

        Ok(note_id)
    }

    /// Extract structured metadata from raw content using the LLM.
    ///
    /// Returns (keywords, tags, context_description).
    async fn extract_metadata(
        &self,
        content: &str,
        llm: &dyn LlmApi,
    ) -> Result<(Vec<String>, Vec<String>, String)> {
        let messages = vec![
            crate::llm::ChatMessage::system(METADATA_SYSTEM_PROMPT),
            crate::llm::ChatMessage::user(metadata_extraction_prompt(content)),
        ];

        let response = llm.chat(&messages, None).await?;
        Self::parse_metadata_response(&response.content)
    }

    /// Parse the LLM's metadata extraction response.
    ///
    /// Supports case-insensitive prefix matching and common Markdown
    /// formatting variants (e.g., `**KEYWORDS:**`, `Keywords:`).
    fn parse_metadata_response(response: &str) -> Result<(Vec<String>, Vec<String>, String)> {
        let mut keywords = Vec::new();
        let mut tags = Vec::new();
        let mut context = String::new();

        for line in response.lines() {
            // Strip leading whitespace and common Markdown formatting
            let line = line.trim().trim_start_matches('*').trim_start_matches('#').trim();

            if let Some(rest) = strip_prefix_case_insensitive(line, "keywords:") {
                keywords = parse_comma_separated(rest);
            } else if let Some(rest) = strip_prefix_case_insensitive(line, "tags:") {
                tags = parse_comma_separated(rest);
            } else if let Some(rest) = strip_prefix_case_insensitive(line, "context:") {
                context = rest.trim_start_matches('*').trim().to_string();
            }
        }

        if keywords.is_empty() && tags.is_empty() && context.is_empty() {
            anyhow::bail!(
                "Failed to parse metadata from LLM response: {}",
                response
            );
        }

        Ok((keywords, tags, context))
    }

    // ── Phase 2: Link Generation ──

    /// Find candidate notes by cosine similarity and establish links.
    ///
    /// 1. Compute cosine similarity between the new note and all existing notes.
    /// 2. Select top-K candidates above the similarity threshold.
    /// 3. Ask the LLM to validate which candidates are truly related.
    /// 4. Establish bidirectional links.
    async fn generate_links(
        &mut self,
        note_id: Uuid,
        llm: &dyn LlmApi,
    ) -> Result<()> {
        let candidates = self.find_similar_notes(note_id);

        if candidates.is_empty() {
            return Ok(());
        }

        // Collect candidate info for the LLM prompt
        let note_text = self.notes.get(&note_id)
            .map(|n| n.to_prompt_text())
            .unwrap_or_default();

        let candidate_texts: Vec<(Uuid, String)> = candidates
            .iter()
            .filter_map(|&(id, _sim)| {
                self.notes.get(&id).map(|n| (id, n.to_prompt_text()))
            })
            .collect();

        if candidate_texts.is_empty() {
            return Ok(());
        }

        // Ask LLM to validate links
        let validated_ids = self
            .validate_links(&note_text, &candidate_texts, llm)
            .await?;

        // Establish bidirectional links
        for linked_id in &validated_ids {
            if let Some(note) = self.notes.get_mut(&note_id) {
                note.add_link(*linked_id);
            }
            if let Some(linked_note) = self.notes.get_mut(linked_id) {
                linked_note.add_link(note_id);
            }
        }

        tracing::debug!(
            note_id = %note_id,
            links_created = validated_ids.len(),
            "Link generation complete"
        );

        Ok(())
    }

    /// Find notes most similar to the given note by cosine similarity.
    ///
    /// Returns a list of (note_id, similarity_score) pairs, sorted by
    /// descending similarity, limited to `max_link_candidates` entries above
    /// the `similarity_threshold`.
    fn find_similar_notes(&self, note_id: Uuid) -> Vec<(Uuid, f32)> {
        let target_embedding = match self.notes.get(&note_id) {
            Some(note) if !note.embedding.is_empty() => &note.embedding,
            _ => return Vec::new(),
        };

        let mut similarities: Vec<(Uuid, f32)> = self
            .notes
            .iter()
            .filter(|(id, _)| **id != note_id)
            .filter(|(_, note)| !note.embedding.is_empty())
            .map(|(id, note)| {
                let sim = cosine_similarity(target_embedding, &note.embedding);
                (*id, sim)
            })
            .filter(|&(_, sim)| sim >= self.similarity_threshold)
            .collect();

        similarities.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        similarities.truncate(self.max_link_candidates);
        similarities
    }

    /// Ask the LLM to validate which candidate notes should be linked.
    async fn validate_links(
        &self,
        note_text: &str,
        candidates: &[(Uuid, String)],
        llm: &dyn LlmApi,
    ) -> Result<Vec<Uuid>> {
        let candidates_text: String = candidates
            .iter()
            .enumerate()
            .map(|(i, (id, text))| format!("Candidate {}: (ID: {})\n{}", i + 1, &id.to_string()[..8], text))
            .collect::<Vec<_>>()
            .join("\n\n");

        let messages = vec![
            crate::llm::ChatMessage::system(LINK_VALIDATION_SYSTEM_PROMPT),
            crate::llm::ChatMessage::user(link_validation_prompt(note_text, &candidates_text)),
        ];

        let response = llm.chat(&messages, None).await?;
        let response_text = response.content.trim();

        if response_text.eq_ignore_ascii_case("NONE") {
            return Ok(Vec::new());
        }

        // Parse candidate numbers from response
        let linked_ids: Vec<Uuid> = response_text
            .split(',')
            .filter_map(|s| {
                let num: usize = s.trim().parse().ok()?;
                if num >= 1 && num <= candidates.len() {
                    Some(candidates[num - 1].0)
                } else {
                    None
                }
            })
            .collect();

        Ok(linked_ids)
    }

    // ── Phase 3: Memory Evolution ──

    /// Trigger evolution of notes linked to the newly added note.
    ///
    /// For each linked note, the LLM re-analyzes its metadata in light of
    /// the new note, potentially updating keywords, tags, and context to
    /// reflect higher-order knowledge patterns.
    async fn evolve_related_notes(
        &mut self,
        note_id: Uuid,
        llm: &dyn LlmApi,
    ) -> Result<()> {
        // Collect linked note IDs (clone to avoid borrow issues)
        let linked_ids: Vec<Uuid> = self
            .notes
            .get(&note_id)
            .map(|n| n.linked_notes.iter().copied().collect())
            .unwrap_or_default();

        if linked_ids.is_empty() {
            return Ok(());
        }

        let new_note_text = self
            .notes
            .get(&note_id)
            .map(|n| n.to_prompt_text())
            .unwrap_or_default();

        for linked_id in &linked_ids {
            let existing_text = match self.notes.get(linked_id) {
                Some(note) => note.to_prompt_text(),
                None => continue,
            };

            match self
                .evolve_single_note(&existing_text, &new_note_text, llm)
                .await
            {
                Ok((keywords, tags, context)) => {
                    if let Some(note) = self.notes.get_mut(linked_id) {
                        note.evolve(keywords, tags, context);
                        tracing::debug!(
                            note_id = %linked_id,
                            "Memory note evolved"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        note_id = %linked_id,
                        error = %e,
                        "Failed to evolve note, keeping original metadata"
                    );
                }
            }
        }

        Ok(())
    }

    /// Ask the LLM to produce updated metadata for an existing note,
    /// given a newly linked note as additional context.
    async fn evolve_single_note(
        &self,
        existing_note_text: &str,
        new_note_text: &str,
        llm: &dyn LlmApi,
    ) -> Result<(Vec<String>, Vec<String>, String)> {
        let messages = vec![
            crate::llm::ChatMessage::system(EVOLUTION_SYSTEM_PROMPT),
            crate::llm::ChatMessage::user(evolution_prompt(existing_note_text, new_note_text)),
        ];

        let response = llm.chat(&messages, None).await?;
        Self::parse_metadata_response(&response.content)
    }

    // ── Context-Aware Retrieval ──

    /// Retrieve the most relevant memory notes for a given query.
    ///
    /// Uses embedding cosine similarity to find the top-K most relevant
    /// notes, then returns them sorted by relevance.
    pub async fn retrieve(
        &self,
        query: &str,
        embedder: &dyn Embedding,
        limit: Option<usize>,
    ) -> Result<Vec<(&MemoryNote, f32)>> {
        if self.notes.is_empty() {
            return Ok(Vec::new());
        }

        let query_embedding = embedder
            .embed(query)
            .await
            .context("Failed to generate query embedding")?;

        let limit = limit.unwrap_or(self.retrieval_limit);

        let mut results: Vec<(&MemoryNote, f32)> = self
            .notes
            .values()
            .filter(|note| !note.embedding.is_empty())
            .map(|note| {
                let sim = cosine_similarity(&query_embedding, &note.embedding);
                (note, sim)
            })
            .collect();

        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);

        Ok(results)
    }

    /// Retrieve relevant memories and format them as context for the LLM.
    ///
    /// This is the primary interface for injecting agentic memory into
    /// conversation context. Returns a Markdown-formatted string of
    /// relevant memories, or `None` if no relevant memories are found.
    pub async fn retrieve_context(
        &self,
        query: &str,
        embedder: &dyn Embedding,
        limit: Option<usize>,
    ) -> Result<Option<String>> {
        let results = self.retrieve(query, embedder, limit).await?;

        if results.is_empty() {
            return Ok(None);
        }

        let sections: Vec<String> = results
            .iter()
            .enumerate()
            .map(|(i, (note, score))| {
                format!(
                    "### Memory {} (relevance: {:.2})\n{}\n**Keywords**: {}\n**Tags**: {}",
                    i + 1,
                    score,
                    note.content,
                    note.keywords.join(", "),
                    note.tags.join(", "),
                )
            })
            .collect();

        Ok(Some(format!(
            "## Relevant Memories\n\n{}",
            sections.join("\n\n")
        )))
    }

    /// Render all notes as a compact Markdown summary for system prompt injection.
    ///
    /// Unlike `retrieve_context` which is query-based, this returns ALL
    /// notes (useful for small memory stores or full context injection).
    #[allow(dead_code)]
    pub fn to_markdown(&self) -> Option<String> {
        if self.notes.is_empty() {
            return None;
        }

        let mut notes: Vec<&MemoryNote> = self.notes.values().collect();
        notes.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        let sections: Vec<String> = notes
            .iter()
            .take(10) // Limit to most recent 10 for token budget
            .map(|note| {
                format!(
                    "- **{}**: {} [{}]",
                    note.keywords.join(", "),
                    note.context,
                    note.tags.join(", "),
                )
            })
            .collect();

        Some(format!(
            "## Agentic Memory\n\n{}",
            sections.join("\n")
        ))
    }
}

impl Default for AgenticMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryPersistence for AgenticMemoryStore {
    fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }
        let notes: Vec<&MemoryNote> = self.notes.values().collect();
        let json = serde_json::to_string_pretty(&notes)
            .context("Failed to serialize AgenticMemoryStore")?;
        std::fs::write(path, &json)
            .with_context(|| format!("Failed to write AgenticMemoryStore to: {}", path.display()))?;
        tracing::debug!(
            path = %path.display(),
            notes = notes.len(),
            "AgenticMemoryStore saved"
        );
        Ok(())
    }

    fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            tracing::debug!(path = %path.display(), "No AgenticMemoryStore file found, using default");
            return Ok(Self::new());
        }
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read AgenticMemoryStore from: {}", path.display()))?;
        let notes: Vec<MemoryNote> = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse AgenticMemoryStore from: {}", path.display()))?;
        let mut store = Self::new();
        for note in notes {
            store.notes.insert(note.id, note);
        }
        tracing::info!(
            path = %path.display(),
            notes = store.notes.len(),
            "AgenticMemoryStore loaded from disk"
        );
        Ok(store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_store() {
        let store = AgenticMemoryStore::new();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn test_parse_metadata_response() {
        let response = "KEYWORDS: rust, memory, systems\nTAGS: language, preference\nCONTEXT: User prefers Rust for systems programming.";
        let (keywords, tags, context) = AgenticMemoryStore::parse_metadata_response(response).unwrap();

        assert_eq!(keywords, vec!["rust", "memory", "systems"]);
        assert_eq!(tags, vec!["language", "preference"]);
        assert_eq!(context, "User prefers Rust for systems programming.");
    }

    #[test]
    fn test_parse_metadata_response_with_whitespace() {
        let response = "  KEYWORDS:  rust , memory  \n  TAGS: lang \n  CONTEXT:  Some context  ";
        let (keywords, tags, context) = AgenticMemoryStore::parse_metadata_response(response).unwrap();

        assert_eq!(keywords, vec!["rust", "memory"]);
        assert_eq!(tags, vec!["lang"]);
        assert_eq!(context, "Some context");
    }

    #[test]
    fn test_parse_metadata_response_case_insensitive() {
        let response = "Keywords: rust, memory\nTags: lang\nContext: Some context";
        let (keywords, tags, context) = AgenticMemoryStore::parse_metadata_response(response).unwrap();
        assert_eq!(keywords, vec!["rust", "memory"]);
        assert_eq!(tags, vec!["lang"]);
        assert_eq!(context, "Some context");
    }

    #[test]
    fn test_parse_metadata_response_markdown_bold() {
        let response = "**KEYWORDS:** rust, memory\n**TAGS:** lang\n**CONTEXT:** Some context";
        let (keywords, tags, context) = AgenticMemoryStore::parse_metadata_response(response).unwrap();
        assert_eq!(keywords, vec!["rust", "memory"]);
        assert_eq!(tags, vec!["lang"]);
        assert_eq!(context, "Some context");
    }

    #[test]
    fn test_parse_metadata_response_empty() {
        let response = "Some random text without the expected format";
        let result = AgenticMemoryStore::parse_metadata_response(response);
        assert!(result.is_err());
    }

    #[test]
    fn test_find_similar_notes() {
        let mut store = AgenticMemoryStore::with_config(0.5, 3, 5);

        // Add notes with known embeddings
        let note1 = MemoryNote::new(
            "Rust programming".to_string(),
            vec!["rust".to_string()],
            vec!["lang".to_string()],
            "About Rust".to_string(),
            vec![1.0, 0.0, 0.0],
        );
        let note2 = MemoryNote::new(
            "Rust memory safety".to_string(),
            vec!["rust".to_string(), "safety".to_string()],
            vec!["lang".to_string()],
            "About Rust safety".to_string(),
            vec![0.9, 0.1, 0.0], // Similar to note1
        );
        let note3 = MemoryNote::new(
            "Python scripting".to_string(),
            vec!["python".to_string()],
            vec!["lang".to_string()],
            "About Python".to_string(),
            vec![0.0, 0.0, 1.0], // Orthogonal to note1
        );

        let id1 = note1.id;
        let id2 = note2.id;
        store.notes.insert(id1, note1);
        store.notes.insert(id2, note2);
        store.notes.insert(note3.id, note3);

        let similar = store.find_similar_notes(id1);
        // note2 should be similar (cosine ~0.994), note3 should not (cosine ~0)
        assert_eq!(similar.len(), 1);
        assert_eq!(similar[0].0, id2);
        assert!(similar[0].1 > 0.9);
    }

    #[test]
    fn test_to_markdown_empty() {
        let store = AgenticMemoryStore::new();
        assert!(store.to_markdown().is_none());
    }

    #[test]
    fn test_to_markdown_with_notes() {
        let mut store = AgenticMemoryStore::new();
        let note = MemoryNote::new(
            "User prefers Rust".to_string(),
            vec!["rust".to_string()],
            vec!["preference".to_string()],
            "Strong Rust preference".to_string(),
            vec![0.1],
        );
        store.notes.insert(note.id, note);

        let md = store.to_markdown().unwrap();
        assert!(md.contains("Agentic Memory"));
        assert!(md.contains("rust"));
        assert!(md.contains("Strong Rust preference"));
    }
}
