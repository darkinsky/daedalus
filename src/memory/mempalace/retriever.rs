use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use regex::Regex;
use once_cell::sync::Lazy;

use crate::embedding::Embedding;

use super::config::MemPalaceConfig;
use super::palace::{HallEntry, Palace};
use super::query_sanitizer::sanitize_query;
use super::store::MemPalaceStore;

/// Regex for tokenizing text into words (2+ chars).
static TOKEN_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\w{2,}").unwrap()
});

/// A single retrieval result from ChromaDB or local search.
#[derive(Debug, Clone)]
pub struct RetrievalResult {
    /// The memory content.
    pub content: String,
    /// Wing ID this memory belongs to.
    pub wing_id: String,
    /// Room ID this memory belongs to.
    pub room_id: String,
    /// Hall type of this memory.
    pub hall_type: String,
    /// Relevance score (higher = more relevant).
    pub score: f32,
    /// Raw cosine distance from ChromaDB.
    #[allow(dead_code)]
    pub distance: f32,
    /// BM25 keyword score.
    pub bm25_score: f32,
    /// Closet boost applied.
    #[allow(dead_code)]
    pub closet_boost: f32,
    /// How this result was matched.
    #[allow(dead_code)]
    pub matched_via: String,
    /// Source file name (if available).
    #[allow(dead_code)]
    pub source_file: String,
}

/// Multi-signal retrieval engine for MemPalace.
///
/// Combines spatial filtering (wing/room scope), embedding similarity
/// (via ChromaDB), keyword matching, and knowledge graph traversal
/// to find the most relevant memories.
pub struct Retriever {
    /// Embedding provider for query vectorization.
    embedder: Arc<dyn Embedding>,
    /// Configuration.
    config: MemPalaceConfig,
    /// ChromaDB HTTP client base URL.
    chroma_url: String,
    /// HTTP client for ChromaDB API calls.
    http_client: reqwest::Client,
    /// Collection name in ChromaDB.
    collection_name: String,
}

impl Retriever {
    /// Create a new retriever.
    pub fn new(
        embedder: Arc<dyn Embedding>,
        config: &MemPalaceConfig,
    ) -> Self {
        let collection_name = format!("{}_halls", config.collection_prefix);
        Self {
            embedder,
            config: config.clone(),
            chroma_url: config.chroma_url.clone(),
            http_client: reqwest::Client::new(),
            collection_name,
        }
    }

    /// Ensure the ChromaDB collection exists (create if not).
    pub async fn ensure_collection(&self) -> Result<()> {
        let url = format!("{}/api/v1/collections", self.chroma_url);

        // Try to get existing collection first
        let get_url = format!(
            "{}/api/v1/collections/{}",
            self.chroma_url, self.collection_name
        );
        let resp = self.http_client.get(&get_url).send().await;

        if let Ok(r) = resp {
            if r.status().is_success() {
                return Ok(());
            }
        }

        // Create collection
        let body = serde_json::json!({
            "name": self.collection_name,
            "metadata": {
                "hnsw:space": "cosine"
            }
        });

        let resp = self.http_client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("Failed to connect to ChromaDB")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            // 409 means collection already exists, which is fine
            if status.as_u16() != 409 {
                anyhow::bail!(
                    "Failed to create ChromaDB collection '{}': {} - {}",
                    self.collection_name, status, text
                );
            }
        }

        tracing::info!(
            collection = %self.collection_name,
            "ChromaDB collection ensured"
        );

        Ok(())
    }

    /// Add a hall entry to ChromaDB.
    pub async fn add_entry(&self, entry: &HallEntry) -> Result<()> {
        // Generate embedding
        let embedding = self.embedder
            .embed(&entry.content)
            .await
            .context("Failed to generate embedding for hall entry")?;

        let metadata = entry.to_chroma_metadata();

        // Get collection ID first
        let collection_id = self.get_collection_id().await?;

        let url = format!(
            "{}/api/v1/collections/{}/add",
            self.chroma_url, collection_id
        );

        let body = serde_json::json!({
            "ids": [entry.id.to_string()],
            "embeddings": [embedding],
            "documents": [entry.to_chroma_document()],
            "metadatas": [metadata],
        });

        let resp = self.http_client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("Failed to add entry to ChromaDB")?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("ChromaDB add failed: {}", text);
        }

        Ok(())
    }

    /// Query ChromaDB for relevant memories.
    ///
    /// Optionally filters by wing_id and/or room_id for spatial scoping.
    pub async fn query(
        &self,
        query_text: &str,
        wing_id: Option<&str>,
        room_id: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<RetrievalResult>> {
        let query_embedding = self.embedder
            .embed(query_text)
            .await
            .context("Failed to generate query embedding")?;

        let collection_id = self.get_collection_id().await?;
        let n_results = limit.unwrap_or(self.config.retrieval_limit);

        let url = format!(
            "{}/api/v1/collections/{}/query",
            self.chroma_url, collection_id
        );

        // Build where filter for spatial scoping
        let where_filter = build_where_filter(wing_id, room_id);

        let mut body = serde_json::json!({
            "query_embeddings": [query_embedding],
            "n_results": n_results,
            "include": ["documents", "metadatas", "distances"],
        });

        if let Some(filter) = where_filter {
            body["where"] = filter;
        }

        let resp = self.http_client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("Failed to query ChromaDB")?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("ChromaDB query failed: {}", text);
        }

        let result: serde_json::Value = resp.json().await
            .context("Failed to parse ChromaDB query response")?;

        parse_query_results(&result)
    }

    /// Retrieve relevant context and format it for system prompt injection.
    ///
    /// Combines six retrieval signals:
    /// 1. Embedding-based retrieval from ChromaDB (with spatial filter)
    /// 2. BM25 hybrid re-ranking (keyword + vector fusion)
    /// 3. Keyword search across drawer verbatim text
    /// 4. Knowledge graph traversal
    /// 5. Tunnel traversal (cross-wing related rooms + explicit tunnels)
    /// 6. Closet summaries for the current room
    pub async fn retrieve_context(
        &self,
        query: &str,
        palace: &Palace,
        store: &MemPalaceStore,
        wing_id: Option<&str>,
        room_id: Option<&str>,
    ) -> Result<Option<String>> {
        // Sanitize query if too long
        let sanitized = sanitize_query(query);
        let effective_query = &sanitized.clean_query;

        // 1. Embedding-based retrieval from ChromaDB (over-fetch for re-ranking)
        let over_fetch = self.config.retrieval_limit * 3;
        let mut chroma_results = match self.query(effective_query, wing_id, room_id, Some(over_fetch)).await {
            Ok(results) => results,
            Err(e) => {
                tracing::warn!(error = %e, "ChromaDB query failed, falling back to other signals");
                Vec::new()
            }
        };

        // 2. BM25 hybrid re-ranking
        if !chroma_results.is_empty() {
            hybrid_rank(&mut chroma_results, effective_query, self.config.vector_weight, self.config.bm25_weight);
            chroma_results.truncate(self.config.retrieval_limit);
        }

        // 3. Keyword search across drawer verbatim text
        let keyword_results = self.keyword_search(effective_query, store, wing_id);

        // 4. Knowledge graph traversal
        let kg_context = self.kg_context(effective_query, palace);

        // 5. Tunnel traversal (cross-wing related rooms + explicit tunnels)
        let tunnel_context = if let Some(rid) = room_id {
            self.tunnel_context(rid, palace, wing_id)
        } else {
            None
        };

        // 6. Closet summaries for the current scope
        let closet_context = self.closet_context(store, wing_id, room_id);

        // Merge results
        if chroma_results.is_empty()
            && keyword_results.is_none()
            && kg_context.is_none()
            && tunnel_context.is_none()
            && closet_context.is_none()
        {
            return Ok(None);
        }

        Ok(Some(format_context_sections(
            &chroma_results,
            keyword_results,
            kg_context,
            tunnel_context,
            closet_context,
        )))
    }

    /// Get knowledge graph context for a query.
    fn kg_context(&self, query: &str, palace: &Palace) -> Option<String> {
        // Extract potential entity names from the query (simple word-based)
        let words: Vec<&str> = query.split_whitespace()
            .filter(|w| w.len() > 2)
            .collect();

        let mut triples = Vec::new();
        for word in &words {
            let related = palace.find_related_triples(word);
            for triple in related {
                triples.push(format!(
                    "- {} → {} → {}",
                    triple.subject, triple.predicate, triple.object
                ));
            }
        }

        triples.dedup();

        if triples.is_empty() {
            None
        } else {
            Some(triples.join("\n"))
        }
    }

    /// Get tunnel context (cross-wing connections) for a room.
    ///
    /// Includes both passive tunnels (shared room names) and explicit
    /// tunnels (agent-created cross-wing links).
    fn tunnel_context(&self, room_id: &str, palace: &Palace, wing_id: Option<&str>) -> Option<String> {
        let mut parts = Vec::new();

        // Passive tunnels
        let connected_wings = palace.tunnel_wings(room_id);
        if connected_wings.len() > 1 {
            parts.push(format!(
                "Room '{}' is shared across wings: {}",
                room_id,
                connected_wings.join(", ")
            ));
        }

        // Explicit tunnels
        if let Some(wid) = wing_id {
            let explicit = palace.follow_tunnels(wid, room_id);
            for tunnel in explicit {
                let target = if tunnel.source_wing == wid && tunnel.source_room == room_id {
                    format!("{}/{}", tunnel.target_wing, tunnel.target_room)
                } else {
                    format!("{}/{}", tunnel.source_wing, tunnel.source_room)
                };
                parts.push(format!(
                    "Tunnel → {} ({})",
                    target, tunnel.label
                ));
            }
        }

        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n"))
        }
    }

    /// Keyword search across drawer verbatim text.
    ///
    /// Performs case-insensitive substring matching on drawer user_input
    /// and assistant_response fields. Optionally scoped to a wing.
    fn keyword_search(
        &self,
        query: &str,
        store: &MemPalaceStore,
        wing_id: Option<&str>,
    ) -> Option<String> {
        let query_lower = query.to_lowercase();
        // Extract meaningful keywords (words > 2 chars)
        let keywords: Vec<&str> = query_lower
            .split_whitespace()
            .filter(|w| w.len() > 2)
            .collect();

        if keywords.is_empty() {
            return None;
        }

        let mut matches: Vec<String> = Vec::new();
        let limit = self.config.retrieval_limit;

        for drawer in &store.drawers {
            // Optional wing scoping
            if let Some(wid) = wing_id {
                if drawer.wing_id != wid {
                    continue;
                }
            }

            let input_lower = drawer.user_input.to_lowercase();
            let response_lower = drawer.assistant_response.to_lowercase();

            // Check if any keyword matches in either field
            let hit = keywords.iter().any(|kw| {
                input_lower.contains(kw) || response_lower.contains(kw)
            });

            if hit {
                // UTF-8 safe truncation: use .chars().take() instead of byte slicing
                let snippet: String = drawer.user_input.chars().take(120).collect();
                let suffix = if drawer.user_input.chars().count() > 120 { "..." } else { "" };
                matches.push(format!(
                    "- [{}:{}] Turn {}: {}{}",
                    drawer.wing_id, drawer.room_id, drawer.turn_number, snippet, suffix
                ));

                if matches.len() >= limit {
                    break;
                }
            }
        }

        if matches.is_empty() {
            None
        } else {
            Some(matches.join("\n"))
        }
    }

    /// Get closet (compressed summary) context for the current scope.
    fn closet_context(
        &self,
        store: &MemPalaceStore,
        wing_id: Option<&str>,
        room_id: Option<&str>,
    ) -> Option<String> {
        let closets: Vec<String> = store.closets
            .iter()
            .filter(|c| {
                let wing_match = wing_id.map_or(true, |w| c.wing_id == w);
                let room_match = room_id.map_or(true, |r| c.room_id == r);
                wing_match && room_match
            })
            .map(|c| {
                format!(
                    "- [{}:{}] (summarizes {} turns): {}",
                    c.wing_id, c.room_id, c.source_drawer_ids.len(), c.summary
                )
            })
            .collect();

        if closets.is_empty() {
            None
        } else {
            Some(closets.join("\n"))
        }
    }

    /// Get the ChromaDB collection ID by name.
    async fn get_collection_id(&self) -> Result<String> {
        let url = format!(
            "{}/api/v1/collections/{}",
            self.chroma_url, self.collection_name
        );

        let resp = self.http_client
            .get(&url)
            .send()
            .await
            .context("Failed to get ChromaDB collection")?;

        if !resp.status().is_success() {
            anyhow::bail!(
                "ChromaDB collection '{}' not found. Ensure ChromaDB is running at {}",
                self.collection_name, self.chroma_url
            );
        }

        let body: serde_json::Value = resp.json().await
            .context("Failed to parse ChromaDB collection response")?;

        body["id"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("ChromaDB collection response missing 'id' field"))
    }
}

/// Format retrieval results into a structured context string for system prompt injection.
///
/// Merges multiple retrieval signals (vector search, keyword, KG, tunnels, closets)
/// into a single formatted markdown string.
fn format_context_sections(
    chroma_results: &[RetrievalResult],
    keyword_results: Option<String>,
    kg_context: Option<String>,
    tunnel_context: Option<String>,
    closet_context: Option<String>,
) -> String {
    let mut sections = Vec::new();

    if !chroma_results.is_empty() {
        let entries: Vec<String> = chroma_results
            .iter()
            .enumerate()
            .map(|(i, r)| {
                format!(
                    "{}. [{}:{}] ({}|sim={:.3}|bm25={:.3}): {}",
                    i + 1, r.wing_id, r.room_id, r.hall_type,
                    r.score, r.bm25_score, r.content
                )
            })
            .collect();
        sections.push(format!(
            "### Spatial Memories (hybrid-ranked)\n{}",
            entries.join("\n")
        ));
    }

    if let Some(kw) = keyword_results {
        sections.push(format!("### Keyword Matches\n{}", kw));
    }

    if let Some(closet) = closet_context {
        sections.push(format!("### Room Summaries\n{}", closet));
    }

    if let Some(kg) = kg_context {
        sections.push(format!("### Knowledge Graph\n{}", kg));
    }

    if let Some(tunnel) = tunnel_context {
        sections.push(format!("### Cross-Project Links\n{}", tunnel));
    }

    format!("## Memory Palace Context\n\n{}", sections.join("\n\n"))
}

/// Build a ChromaDB `where` filter for spatial scoping.
fn build_where_filter(
    wing_id: Option<&str>,
    room_id: Option<&str>,
) -> Option<serde_json::Value> {
    match (wing_id, room_id) {
        (Some(w), Some(r)) => Some(serde_json::json!({
            "$and": [
                {"wing_id": {"$eq": w}},
                {"room_id": {"$eq": r}}
            ]
        })),
        (Some(w), None) => Some(serde_json::json!({
            "wing_id": {"$eq": w}
        })),
        (None, Some(r)) => Some(serde_json::json!({
            "room_id": {"$eq": r}
        })),
        (None, None) => None,
    }
}

/// Parse ChromaDB query results into RetrievalResult structs.
fn parse_query_results(result: &serde_json::Value) -> Result<Vec<RetrievalResult>> {
    let mut results = Vec::new();

    let documents = result["documents"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|a| a.as_array());

    let metadatas = result["metadatas"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|a| a.as_array());

    let distances = result["distances"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|a| a.as_array());

    if let (Some(docs), Some(metas), Some(dists)) = (documents, metadatas, distances) {
        for i in 0..docs.len() {
            let content = docs[i].as_str().unwrap_or("").to_string();
            let meta = &metas[i];
            let distance = dists[i].as_f64().unwrap_or(1.0) as f32;
            // ChromaDB returns cosine distance; convert to similarity
            let score = (1.0 - distance).max(0.0);

            results.push(RetrievalResult {
                content,
                wing_id: meta["wing_id"].as_str().unwrap_or("").to_string(),
                room_id: meta["room_id"].as_str().unwrap_or("").to_string(),
                hall_type: meta["hall_type"].as_str().unwrap_or("").to_string(),
                score,
                distance,
                bm25_score: 0.0,
                closet_boost: 0.0,
                matched_via: "drawer".to_string(),
                source_file: meta["source_file"].as_str().unwrap_or("").to_string(),
            });
        }
    }

    Ok(results)
}

// ── BM25 Hybrid Ranking ──

/// Tokenize text into lowercase words of length >= 2.
fn tokenize(text: &str) -> Vec<String> {
    TOKEN_RE
        .find_iter(&text.to_lowercase())
        .map(|m| m.as_str().to_string())
        .collect()
}

/// Compute Okapi-BM25 scores for a query against each document.
///
/// IDF is computed over the provided corpus using the Lucene/BM25+
/// smoothed formula: log((N - df + 0.5) / (df + 0.5) + 1).
///
/// Parameters mirror Okapi-BM25 conventions:
///   k1 — term-frequency saturation (1.2-2.0 typical, 1.5 default)
///   b  — length normalization (0.0 = none, 1.0 = full, 0.75 default)
fn bm25_scores(
    query: &str,
    documents: &[String],
    k1: f32,
    b: f32,
) -> Vec<f32> {
    let n_docs = documents.len();
    let query_terms: HashSet<String> = tokenize(query).into_iter().collect();

    if query_terms.is_empty() || n_docs == 0 {
        return vec![0.0; n_docs];
    }

    let tokenized: Vec<Vec<String>> = documents.iter().map(|d| tokenize(d)).collect();
    let doc_lens: Vec<f32> = tokenized.iter().map(|t| t.len() as f32).collect();
    let total_len: f32 = doc_lens.iter().sum();
    let avgdl = if n_docs > 0 { total_len / n_docs as f32 } else { 1.0 };

    // Document frequency
    let mut df: HashMap<String, usize> = HashMap::new();
    for toks in &tokenized {
        let seen: HashSet<&String> = toks.iter().collect();
        for term in &query_terms {
            if seen.contains(term) {
                *df.entry(term.clone()).or_insert(0) += 1;
            }
        }
    }

    // IDF
    let idf: HashMap<String, f32> = query_terms
        .iter()
        .map(|term| {
            let d = *df.get(term).unwrap_or(&0) as f32;
            let n = n_docs as f32;
            let score = ((n - d + 0.5) / (d + 0.5) + 1.0).ln();
            (term.clone(), score)
        })
        .collect();

    // Score each document
    tokenized
        .iter()
        .zip(doc_lens.iter())
        .map(|(toks, &dl)| {
            if dl == 0.0 {
                return 0.0;
            }
            let mut tf: HashMap<&String, f32> = HashMap::new();
            for t in toks {
                if query_terms.contains(t) {
                    *tf.entry(t).or_insert(0.0) += 1.0;
                }
            }
            let mut score = 0.0f32;
            for (term, &freq) in &tf {
                let term_idf = idf.get(*term).copied().unwrap_or(0.0);
                let num = freq * (k1 + 1.0);
                let den = freq + k1 * (1.0 - b + b * dl / avgdl);
                score += term_idf * num / den;
            }
            score
        })
        .collect()
}

/// Re-rank results by a convex combination of vector similarity and BM25.
///
/// Vector similarity uses absolute cosine sim max(0, 1 - distance).
/// BM25 is min-max normalized within the candidate set.
///
/// Matches the original MemPalace searcher.py `_hybrid_rank()`.
fn hybrid_rank(
    results: &mut Vec<RetrievalResult>,
    query: &str,
    vector_weight: f32,
    bm25_weight: f32,
) {
    if results.is_empty() {
        return;
    }

    let docs: Vec<String> = results.iter().map(|r| r.content.clone()).collect();
    let bm25_raw = bm25_scores(query, &docs, 1.5, 0.75);
    let max_bm25 = bm25_raw.iter().cloned().fold(0.0f32, f32::max);
    let bm25_norm: Vec<f32> = if max_bm25 > 0.0 {
        bm25_raw.iter().map(|&s| s / max_bm25).collect()
    } else {
        vec![0.0; bm25_raw.len()]
    };

    let mut scored: Vec<(f32, usize)> = Vec::new();
    for (i, (raw, norm)) in bm25_raw.iter().zip(bm25_norm.iter()).enumerate() {
        results[i].bm25_score = (*raw * 1000.0).round() / 1000.0;
        let vec_sim = results[i].score.max(0.0);
        let combined = vector_weight * vec_sim + bm25_weight * norm;
        scored.push((combined, i));
    }

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let reordered: Vec<RetrievalResult> = scored
        .into_iter()
        .map(|(_, idx)| results[idx].clone())
        .collect();
    *results = reordered;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_where_filter_both() {
        let filter = build_where_filter(Some("wing1"), Some("room1"));
        assert!(filter.is_some());
        let f = filter.unwrap();
        assert!(f["$and"].is_array());
    }

    #[test]
    fn test_build_where_filter_wing_only() {
        let filter = build_where_filter(Some("wing1"), None);
        assert!(filter.is_some());
        let f = filter.unwrap();
        assert!(f["wing_id"].is_object());
    }

    #[test]
    fn test_build_where_filter_none() {
        let filter = build_where_filter(None, None);
        assert!(filter.is_none());
    }

    #[test]
    fn test_parse_query_results_empty() {
        let result = serde_json::json!({
            "documents": [[]],
            "metadatas": [[]],
            "distances": [[]]
        });
        let results = parse_query_results(&result).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_parse_query_results() {
        let result = serde_json::json!({
            "documents": [["doc1", "doc2"]],
            "metadatas": [[
                {"wing_id": "w1", "room_id": "r1", "hall_type": "facts"},
                {"wing_id": "w2", "room_id": "r2", "hall_type": "events"}
            ]],
            "distances": [[0.1, 0.3]]
        });
        let results = parse_query_results(&result).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].wing_id, "w1");
        assert!((results[0].score - 0.9).abs() < 0.01);
    }
}
