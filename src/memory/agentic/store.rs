use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use uuid::Uuid;

use crate::embedding::{cosine_similarity, Embedding};
use crate::llm::LlmApi;
use crate::memory::persistence::MemoryPersistence;

use super::note::MemoryNote;
use super::prompts;

/// Default maximum number of candidate notes to retrieve for link generation.
const DEFAULT_MAX_LINK_CANDIDATES: usize = 5;

/// Default similarity threshold for considering a note as a link candidate.
const DEFAULT_SIMILARITY_THRESHOLD: f32 = 0.5;

/// Default number of notes to retrieve for context-aware retrieval.
const DEFAULT_RETRIEVAL_LIMIT: usize = 5;

// ── Parsing helpers ──

/// Strip a prefix from a string in a case-insensitive manner, returning
/// the remainder from the *original* (non-lowercased) string.
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
/// (arxiv:2502.12110, NeurIPS 2025):
///
/// 1. **Note Construction**: New information → LLM extracts structured
///    metadata (keywords, tags, category, context) → embedding model generates vector.
/// 2. **Process Memory** (unified linking + evolution): Cosine similarity
///    retrieves candidates → single LLM call decides links + whether to evolve
///    + how to update neighbor metadata.
/// 3. **Retrieval**: `search_agentic()` combines vector similarity with
///    graph traversal along `links` for richer context.
///
/// The store maintains an in-memory collection of `MemoryNote`s indexed by
/// UUID, with embedding vectors for similarity search.
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
    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }

    /// Get a reference to a note by ID.
    pub fn get_note(&self, id: &Uuid) -> Option<&MemoryNote> {
        self.notes.get(id)
    }

    /// Get all notes as a slice-like iterator.
    pub fn all_notes(&self) -> impl Iterator<Item = &MemoryNote> {
        self.notes.values()
    }

    // ══════════════════════════════════════════════════════════════
    // ── Phase 1: Note Construction ──
    // ══════════════════════════════════════════════════════════════

    /// Add a new memory from raw content.
    ///
    /// This is the main entry point for the A-MEM lifecycle:
    /// 1. LLM extracts keywords, tags, category, and context from the content.
    /// 2. Embedding model generates a vector for the content.
    /// 3. A new `MemoryNote` is created and stored.
    /// 4. Unified `process_memory` handles linking + selective evolution in one LLM call.
    ///
    /// Returns the ID of the newly created note.
    pub async fn add_memory(
        &mut self,
        content: &str,
        llm: &dyn LlmApi,
        embedder: &dyn Embedding,
    ) -> Result<Uuid> {
        // Phase 1: Note Construction
        let (keywords, tags, category, context) = self
            .extract_metadata(content, llm)
            .await
            .context("Failed to extract metadata for new memory")?;

        let embedding = embedder
            .embed(content)
            .await
            .context("Failed to generate embedding for new memory")?;

        let note = MemoryNote::with_category(
            content.to_string(),
            keywords,
            tags,
            context,
            category,
            embedding,
        );
        let note_id = note.id;

        tracing::debug!(
            note_id = %note_id,
            keywords = ?note.keywords,
            tags = ?note.tags,
            category = %note.category,
            "Created new memory note"
        );

        self.notes.insert(note_id, note);

        // Phase 2+3: Unified process_memory (linking + selective evolution)
        if let Err(e) = self.process_memory(note_id, llm).await {
            tracing::warn!(
                note_id = %note_id,
                error = %e,
                "process_memory failed, note stored without links/evolution"
            );
        }

        Ok(note_id)
    }

    /// Extract structured metadata from raw content using the LLM.
    ///
    /// Returns (keywords, tags, category, context_description).
    async fn extract_metadata(
        &self,
        content: &str,
        llm: &dyn LlmApi,
    ) -> Result<(Vec<String>, Vec<String>, String, String)> {
        let messages = vec![
            crate::llm::ChatMessage::system(prompts::METADATA_SYSTEM_PROMPT),
            crate::llm::ChatMessage::user(prompts::metadata_extraction_prompt(content)),
        ];

        let response = llm.chat(&messages, None).await?;
        Self::parse_metadata_response(&response.content)
    }

    /// Parse the LLM's metadata extraction response.
    ///
    /// Now also extracts CATEGORY field.
    pub fn parse_metadata_response(response: &str) -> Result<(Vec<String>, Vec<String>, String, String)> {
        let mut keywords = Vec::new();
        let mut tags = Vec::new();
        let mut category = "uncategorized".to_string();
        let mut context = String::new();

        for line in response.lines() {
            let line = line.trim().trim_start_matches('*').trim_start_matches('#').trim();

            if let Some(rest) = strip_prefix_case_insensitive(line, "keywords:") {
                keywords = parse_comma_separated(rest);
            } else if let Some(rest) = strip_prefix_case_insensitive(line, "tags:") {
                tags = parse_comma_separated(rest);
            } else if let Some(rest) = strip_prefix_case_insensitive(line, "category:") {
                category = rest.trim_start_matches('*').trim().to_lowercase();
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

        Ok((keywords, tags, category, context))
    }

    // ══════════════════════════════════════════════════════════════
    // ── Phase 2+3: Unified Process Memory (Link + Evolve) ──
    // ══════════════════════════════════════════════════════════════

    /// Unified process_memory — combines linking and selective evolution
    /// into a single LLM call.
    ///
    /// Aligned with the paper's `process_memory()` design:
    /// 1. Find similar notes by cosine similarity
    /// 2. Single LLM call decides: which to link + whether to evolve + how
    /// 3. Apply decisions (create links, update neighbor metadata)
    async fn process_memory(
        &mut self,
        note_id: Uuid,
        llm: &dyn LlmApi,
    ) -> Result<()> {
        let candidates = self.find_similar_notes(note_id);

        if candidates.is_empty() {
            return Ok(());
        }

        // Build prompt with new note + candidates
        let note_text = self.notes.get(&note_id)
            .map(|n| n.to_prompt_text())
            .unwrap_or_default();

        let candidate_texts: Vec<(usize, Uuid, String)> = candidates
            .iter()
            .enumerate()
            .filter_map(|(i, &(id, _sim))| {
                self.notes.get(&id).map(|n| (i + 1, id, n.to_prompt_text()))
            })
            .collect();

        if candidate_texts.is_empty() {
            return Ok(());
        }

        let candidates_formatted: String = candidate_texts
            .iter()
            .map(|(num, id, text)| format!("Candidate {} (ID: {}):\n{}", num, &id.to_string()[..8], text))
            .collect::<Vec<_>>()
            .join("\n\n");

        // Single LLM call for linking + evolution decisions
        let messages = vec![
            crate::llm::ChatMessage::system(prompts::PROCESS_MEMORY_SYSTEM_PROMPT),
            crate::llm::ChatMessage::user(prompts::process_memory_prompt(&note_text, &candidates_formatted)),
        ];

        let response = llm.chat(&messages, None).await?;
        let response_text = response.content.trim();

        // Parse the unified response
        let (linked_nums, should_evolve, evolutions) = Self::parse_process_memory_response(response_text);

        // Apply linking decisions
        let mut linked_ids = Vec::new();
        for num in &linked_nums {
            if let Some(&(_, id, _)) = candidate_texts.iter().find(|(n, _, _)| n == num) {
                if let Some(note) = self.notes.get_mut(&note_id) {
                    note.add_link(id);
                }
                if let Some(linked_note) = self.notes.get_mut(&id) {
                    linked_note.add_link(note_id);
                }
                linked_ids.push(id);
            }
        }

        tracing::debug!(
            note_id = %note_id,
            links_created = linked_ids.len(),
            should_evolve = should_evolve,
            "process_memory: linking complete"
        );

        // Apply selective evolution (only if LLM decided it's needed)
        if should_evolve {
            for (candidate_num, new_tags, new_context) in &evolutions {
                if let Some(&(_, id, _)) = candidate_texts.iter().find(|(n, _, _)| n == candidate_num) {
                    if linked_ids.contains(&id) {
                        if let Some(note) = self.notes.get_mut(&id) {
                            // Keep existing keywords, only update tags + context
                            let keywords = note.keywords.clone();
                            note.evolve(keywords, new_tags.clone(), new_context.clone());
                            tracing::debug!(
                                note_id = %id,
                                "Memory note evolved via process_memory"
                            );
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Parse the unified process_memory LLM response.
    ///
    /// Returns (linked_candidate_numbers, should_evolve, evolution_updates).
    fn parse_process_memory_response(response: &str) -> (Vec<usize>, bool, Vec<(usize, Vec<String>, String)>) {
        let mut linked_nums: Vec<usize> = Vec::new();
        let mut should_evolve = false;
        let mut evolutions: Vec<(usize, Vec<String>, String)> = Vec::new();

        // Temporary state for parsing evolution blocks
        let mut current_tags: HashMap<usize, Vec<String>> = HashMap::new();
        let mut current_contexts: HashMap<usize, String> = HashMap::new();

        for line in response.lines() {
            let line = line.trim();
            if line.is_empty() { continue; }

            if let Some(rest) = strip_prefix_case_insensitive(line, "link:") {
                let rest = rest.trim();
                if !rest.eq_ignore_ascii_case("none") {
                    linked_nums = rest.split(',')
                        .filter_map(|s| s.trim().parse::<usize>().ok())
                        .collect();
                }
            } else if let Some(rest) = strip_prefix_case_insensitive(line, "should_evolve:") {
                should_evolve = rest.trim().eq_ignore_ascii_case("true");
            } else if let Some(rest) = strip_prefix_case_insensitive(line, "candidate_") {
                // Parse CANDIDATE_N_TAGS or CANDIDATE_N_CONTEXT
                if let Some((num_and_field, value)) = rest.split_once(':') {
                    let parts: Vec<&str> = num_and_field.split('_').collect();
                    if parts.len() >= 2 {
                        if let Ok(num) = parts[0].parse::<usize>() {
                            let field = parts[1..].join("_").to_lowercase();
                            if field == "tags" {
                                current_tags.insert(num, parse_comma_separated(value));
                            } else if field == "context" {
                                current_contexts.insert(num, value.trim().to_string());
                            }
                        }
                    }
                }
            }
        }

        // Assemble evolution entries
        for num in &linked_nums {
            let tags = current_tags.remove(num).unwrap_or_default();
            let context = current_contexts.remove(num).unwrap_or_default();
            if !tags.is_empty() || !context.is_empty() {
                evolutions.push((*num, tags, context));
            }
        }

        (linked_nums, should_evolve, evolutions)
    }

    /// Find notes most similar to the given note by cosine similarity.
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

    // ══════════════════════════════════════════════════════════════
    // ── Retrieval: search_agentic (graph-expanded) ──
    // ══════════════════════════════════════════════════════════════

    /// Agentic search — vector similarity + graph link expansion.
    ///
    /// This is the paper's `search_agentic()`:
    /// 1. Find top-k notes by cosine similarity (direct matches)
    /// 2. Expand along `links` to include neighbor notes
    /// 3. Mark each result as `is_neighbor` (true = reached via link)
    /// 4. Update `retrieval_count` and `last_accessed` on accessed notes
    ///
    /// Returns (note_id, similarity_score, is_neighbor) tuples.
    pub async fn search_agentic(
        &mut self,
        query: &str,
        embedder: &dyn Embedding,
        limit: Option<usize>,
    ) -> Result<Vec<(Uuid, f32, bool)>> {
        if self.notes.is_empty() {
            return Ok(Vec::new());
        }

        let query_embedding = embedder
            .embed(query)
            .await
            .context("Failed to generate query embedding")?;

        let limit = limit.unwrap_or(self.retrieval_limit);

        // Step 1: Direct similarity search
        let mut direct_results: Vec<(Uuid, f32)> = self
            .notes
            .values()
            .filter(|note| !note.embedding.is_empty())
            .map(|note| {
                let sim = cosine_similarity(&query_embedding, &note.embedding);
                (note.id, sim)
            })
            .collect();

        direct_results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        direct_results.truncate(limit);

        // Step 2: Expand along links (add neighbors)
        let mut seen: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
        let mut results: Vec<(Uuid, f32, bool)> = Vec::new();

        for (id, score) in &direct_results {
            seen.insert(*id);
            results.push((*id, *score, false)); // is_neighbor = false
        }

        // Expand neighbors from direct matches
        for (id, parent_score) in &direct_results {
            if let Some(note) = self.notes.get(id) {
                for &neighbor_id in &note.linked_notes {
                    if !seen.contains(&neighbor_id) {
                        seen.insert(neighbor_id);
                        // Neighbor score = parent_score * 0.8 (decay factor)
                        let neighbor_score = parent_score * 0.8;
                        results.push((neighbor_id, neighbor_score, true));
                    }
                }
            }
        }

        // Sort by score descending, truncate to limit
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);

        // Step 3: Update access tracking on retrieved notes
        for (id, _, _) in &results {
            if let Some(note) = self.notes.get_mut(id) {
                note.record_access();
            }
        }

        Ok(results)
    }

    /// Basic retrieve (cosine similarity only, no graph expansion).
    ///
    /// Kept for backward compatibility and simple use cases.
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

    /// Retrieve relevant memories using agentic search and format as context.
    ///
    /// Uses `search_agentic()` (graph-expanded retrieval) and wraps the
    /// results with injection preamble/epilogue for the system prompt.
    ///
    /// Returns `None` if no relevant memories are found.
    pub async fn retrieve_context(
        &mut self,
        query: &str,
        embedder: &dyn Embedding,
        limit: Option<usize>,
    ) -> Result<Option<String>> {
        let results = self.search_agentic(query, embedder, limit).await?;

        if results.is_empty() {
            return Ok(None);
        }

        let sections: Vec<String> = results
            .iter()
            .filter_map(|(id, score, is_neighbor)| {
                let note = self.notes.get(id)?;
                let source = if *is_neighbor { " (via link)" } else { "" };
                Some(format!(
                    "### Memory {} (relevance: {:.2}{})\n{}\n**Keywords**: {}\n**Tags**: {}\n**Category**: {}",
                    &id.to_string()[..8],
                    score,
                    source,
                    note.content,
                    note.keywords.join(", "),
                    note.tags.join(", "),
                    note.category,
                ))
            })
            .collect();

        Ok(Some(format!(
            "{}\n\n## Retrieved Memories\n\n{}\n\n{}",
            prompts::MEMORY_INJECTION_PREAMBLE,
            sections.join("\n\n"),
            prompts::MEMORY_INJECTION_EPILOGUE,
        )))
    }

    /// Render all notes as a compact Markdown summary for system prompt injection.
    #[allow(dead_code)]
    pub fn to_markdown(&self) -> Option<String> {
        if self.notes.is_empty() {
            return None;
        }

        let mut notes: Vec<&MemoryNote> = self.notes.values().collect();
        notes.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        let sections: Vec<String> = notes
            .iter()
            .take(10)
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
        let notes: Vec<&MemoryNote> = self.notes.values().collect();
        let json = serde_json::to_string_pretty(&notes)
            .context("Failed to serialize AgenticMemoryStore")?;
        crate::memory::persistence::atomic_write(path, json.as_bytes())
            .with_context(|| format!("Failed to write AgenticMemoryStore to: {}", path.display()))?;
        tracing::debug!(
            path = %path.display(),
            notes = notes.len(),
            "AgenticMemoryStore saved (atomic)"
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
        let response = "KEYWORDS: rust, memory, systems\nTAGS: language, preference\nCATEGORY: user_preference\nCONTEXT: User prefers Rust for systems programming.";
        let (keywords, tags, category, context) = AgenticMemoryStore::parse_metadata_response(response).unwrap();

        assert_eq!(keywords, vec!["rust", "memory", "systems"]);
        assert_eq!(tags, vec!["language", "preference"]);
        assert_eq!(category, "user_preference");
        assert_eq!(context, "User prefers Rust for systems programming.");
    }

    #[test]
    fn test_parse_metadata_response_no_category() {
        let response = "KEYWORDS: rust, memory\nTAGS: lang\nCONTEXT: Some context";
        let (keywords, tags, category, context) = AgenticMemoryStore::parse_metadata_response(response).unwrap();
        assert_eq!(keywords, vec!["rust", "memory"]);
        assert_eq!(tags, vec!["lang"]);
        assert_eq!(category, "uncategorized");
        assert_eq!(context, "Some context");
    }

    #[test]
    fn test_parse_metadata_response_with_whitespace() {
        let response = "  KEYWORDS:  rust , memory  \n  TAGS: lang \n  CONTEXT:  Some context  ";
        let (_keywords, _tags, _category, context) = AgenticMemoryStore::parse_metadata_response(response).unwrap();
        assert_eq!(context, "Some context");
    }

    #[test]
    fn test_parse_metadata_response_case_insensitive() {
        let response = "Keywords: rust, memory\nTags: lang\nContext: Some context";
        let (keywords, tags, _category, context) = AgenticMemoryStore::parse_metadata_response(response).unwrap();
        assert_eq!(keywords, vec!["rust", "memory"]);
        assert_eq!(tags, vec!["lang"]);
        assert_eq!(context, "Some context");
    }

    #[test]
    fn test_parse_metadata_response_markdown_bold() {
        let response = "**KEYWORDS:** rust, memory\n**TAGS:** lang\n**CONTEXT:** Some context";
        let (keywords, tags, _category, context) = AgenticMemoryStore::parse_metadata_response(response).unwrap();
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
    fn test_parse_process_memory_response() {
        let response = "LINK: 1, 3\nSHOULD_EVOLVE: true\nCANDIDATE_1_TAGS: rust, safety\nCANDIDATE_1_CONTEXT: Updated Rust safety context\nCANDIDATE_3_TAGS: systems\nCANDIDATE_3_CONTEXT: Systems programming context";
        let (linked, should_evolve, evolutions) = AgenticMemoryStore::parse_process_memory_response(response);
        assert_eq!(linked, vec![1, 3]);
        assert!(should_evolve);
        assert_eq!(evolutions.len(), 2);
        assert_eq!(evolutions[0].0, 1);
        assert!(evolutions[0].1.contains(&"rust".to_string()));
        assert!(evolutions[0].2.contains("Rust safety"));
    }

    #[test]
    fn test_parse_process_memory_response_no_evolve() {
        let response = "LINK: 2\nSHOULD_EVOLVE: false";
        let (linked, should_evolve, evolutions) = AgenticMemoryStore::parse_process_memory_response(response);
        assert_eq!(linked, vec![2]);
        assert!(!should_evolve);
        assert!(evolutions.is_empty());
    }

    #[test]
    fn test_parse_process_memory_response_none() {
        let response = "LINK: NONE\nSHOULD_EVOLVE: false";
        let (linked, should_evolve, _evolutions) = AgenticMemoryStore::parse_process_memory_response(response);
        assert!(linked.is_empty());
        assert!(!should_evolve);
    }

    #[test]
    fn test_find_similar_notes() {
        let mut store = AgenticMemoryStore::with_config(0.5, 3, 5);

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
            vec![0.9, 0.1, 0.0],
        );
        let note3 = MemoryNote::new(
            "Python scripting".to_string(),
            vec!["python".to_string()],
            vec!["lang".to_string()],
            "About Python".to_string(),
            vec![0.0, 0.0, 1.0],
        );

        let id1 = note1.id;
        let id2 = note2.id;
        store.notes.insert(id1, note1);
        store.notes.insert(id2, note2);
        store.notes.insert(note3.id, note3);

        let similar = store.find_similar_notes(id1);
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
