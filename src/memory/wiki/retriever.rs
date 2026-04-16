use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use anyhow::{Context, Result};

use crate::embedding::{cosine_similarity, Embedding};

use super::page::WikiPage;

/// Minimum keyword match score to consider a page relevant.
const KEYWORD_MATCH_THRESHOLD: f32 = 0.1;

/// Default link expansion score for pages discovered via wikilink traversal.
const LINK_EXPANSION_SCORE: f32 = 0.3;

/// Maximum number of seed pages used for wikilink expansion.
const MAX_SEED_PAGES: usize = 3;

/// Cached set of English stop words for keyword tokenization.
///
/// Using `LazyLock` to avoid rebuilding the `HashSet` on every call
/// to `tokenize()`. This is a static, immutable set.
static STOP_WORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "the", "a", "an", "is", "are", "was", "were", "be", "been",
        "being", "have", "has", "had", "do", "does", "did", "will",
        "would", "could", "should", "may", "might", "can", "shall",
        "to", "of", "in", "for", "on", "with", "at", "by", "from",
        "as", "into", "through", "during", "before", "after",
        "and", "but", "or", "nor", "not", "so", "yet",
        "it", "its", "this", "that", "these", "those",
        "i", "me", "my", "we", "our", "you", "your", "he", "she",
        "him", "her", "they", "them", "their", "what", "which",
        "who", "whom", "how", "when", "where", "why",
    ]
    .iter()
    .copied()
    .collect()
});

/// Wiki retriever — handles all retrieval strategies for wiki pages.
///
/// Separated from `WikiStore` to follow the Single Responsibility Principle:
/// - `WikiStore` handles storage, CRUD, persistence, and backlinks.
/// - `WikiRetriever` handles query processing, scoring, and ranking.
///
/// Supports two retrieval modes:
/// - **Embedding mode**: Cosine similarity search (requires embedding provider).
/// - **Keyword mode**: Keyword matching + wikilink traversal (zero dependencies).
pub struct WikiRetriever;

impl WikiRetriever {
    /// Unified retrieval: uses embedding if available, falls back to keywords.
    ///
    /// This is the main entry point for retrieval. It automatically selects
    /// the best available retrieval method:
    /// - **With embedding**: Cosine similarity search (higher quality)
    /// - **Without embedding**: Keyword matching + wikilink traversal (zero dependencies)
    pub async fn retrieve<'a>(
        pages: &'a HashMap<String, WikiPage>,
        query: &str,
        embedder: Option<&dyn Embedding>,
        limit: usize,
    ) -> Result<Vec<(&'a WikiPage, f32)>> {
        match embedder {
            Some(emb) => Self::retrieve_by_embedding(pages, query, emb, limit).await,
            None => Ok(Self::retrieve_by_keywords(pages, query, limit)),
        }
    }

    /// Retrieve relevant pages and format them as context for the LLM.
    ///
    /// Returns a Markdown-formatted string of relevant wiki pages,
    /// or `None` if no relevant pages are found.
    pub async fn retrieve_context(
        pages: &HashMap<String, WikiPage>,
        query: &str,
        embedder: Option<&dyn Embedding>,
        limit: usize,
    ) -> Result<Option<String>> {
        let results = Self::retrieve(pages, query, embedder, limit).await?;

        if results.is_empty() {
            return Ok(None);
        }

        let sections: Vec<String> = results
            .iter()
            .enumerate()
            .map(|(i, (page, score))| {
                format!(
                    "### Wiki Page {} (relevance: {:.2})\n**{}** [{}]\n\n{}",
                    i + 1,
                    score,
                    page.frontmatter.title,
                    page.frontmatter.tags.join(", "),
                    page.body,
                )
            })
            .collect();

        Ok(Some(format!(
            "## Relevant Wiki Knowledge\n\n{}",
            sections.join("\n\n---\n\n")
        )))
    }

    // ── Embedding-based retrieval ──

    /// Retrieve the most relevant wiki pages using embedding similarity.
    ///
    /// Uses embedding cosine similarity to find the top-K most relevant
    /// pages. Requires an embedding provider.
    async fn retrieve_by_embedding<'a>(
        pages: &'a HashMap<String, WikiPage>,
        query: &str,
        embedder: &dyn Embedding,
        limit: usize,
    ) -> Result<Vec<(&'a WikiPage, f32)>> {
        if pages.is_empty() {
            return Ok(Vec::new());
        }

        let query_embedding = embedder
            .embed(query)
            .await
            .context("Failed to generate query embedding for wiki retrieval")?;

        let mut results: Vec<(&WikiPage, f32)> = pages
            .values()
            .filter(|page| !page.embedding.is_empty())
            .map(|page| {
                let sim = cosine_similarity(&query_embedding, &page.embedding);
                (page, sim)
            })
            .collect();

        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);

        Ok(results)
    }

    // ── Keyword-based retrieval ──

    /// Retrieve the most relevant wiki pages using keyword matching + wikilink traversal.
    ///
    /// This is the **fallback retrieval** method used when no embedding provider
    /// is configured. It works in three phases:
    /// 1. **Score**: Tokenize query and score each page by keyword overlap.
    /// 2. **Expand**: Traverse wikilinks from top matches to discover related pages.
    /// 3. **Merge**: Combine, deduplicate, and rank all results.
    pub fn retrieve_by_keywords<'a>(
        pages: &'a HashMap<String, WikiPage>,
        query: &str,
        limit: usize,
    ) -> Vec<(&'a WikiPage, f32)> {
        if pages.is_empty() {
            return Vec::new();
        }

        let query_tokens = Self::tokenize(query);
        if query_tokens.is_empty() {
            return Vec::new();
        }

        // Phase 1: Score each page by keyword overlap
        let mut candidates = Self::score_pages(pages, &query_tokens);

        // Phase 2: Expand via wikilink traversal from top matches
        let seed_limit = limit.min(MAX_SEED_PAGES);
        let seeds: Vec<String> = candidates
            .iter()
            .take(seed_limit)
            .map(|(page, _)| page.id.clone())
            .collect();
        let expanded = Self::expand_via_wikilinks(pages, &seeds);

        // Add expanded pages with a reduced score
        for expanded_id in expanded {
            if let Some(page) = pages.get(&expanded_id) {
                candidates.push((page, LINK_EXPANSION_SCORE));
            }
        }

        // Phase 3: Merge, deduplicate, and truncate
        Self::deduplicate_and_truncate(candidates, limit)
    }

    /// Score each page by keyword overlap with query tokens.
    ///
    /// Returns pages sorted by score (descending), filtered by threshold.
    fn score_pages<'a>(
        pages: &'a HashMap<String, WikiPage>,
        query_tokens: &[String],
    ) -> Vec<(&'a WikiPage, f32)> {
        let mut scored: Vec<(&WikiPage, f32)> = pages
            .values()
            .map(|page| {
                let score = Self::keyword_score(page, query_tokens);
                (page, score)
            })
            .filter(|(_, score)| *score > KEYWORD_MATCH_THRESHOLD)
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored
    }

    /// Expand seed pages via 1-hop wikilink traversal (forward + backward links).
    ///
    /// Returns page IDs discovered through link traversal that are NOT
    /// already in the seed set.
    fn expand_via_wikilinks(
        pages: &HashMap<String, WikiPage>,
        seeds: &[String],
    ) -> Vec<String> {
        let mut seen: HashSet<String> = seeds.iter().cloned().collect();
        let mut expanded = Vec::new();

        for seed_id in seeds {
            if let Some(seed_page) = pages.get(seed_id) {
                // Forward links
                for link in &seed_page.frontmatter.links {
                    if !seen.contains(link) && pages.contains_key(link) {
                        seen.insert(link.clone());
                        expanded.push(link.clone());
                    }
                }
                // Backlinks
                for backlink in &seed_page.backlinks {
                    if !seen.contains(backlink) {
                        seen.insert(backlink.clone());
                        expanded.push(backlink.clone());
                    }
                }
            }
        }

        expanded
    }

    /// Deduplicate scored results by page ID (keep highest score) and truncate.
    fn deduplicate_and_truncate<'a>(
        mut scored: Vec<(&'a WikiPage, f32)>,
        limit: usize,
    ) -> Vec<(&'a WikiPage, f32)> {
        // Re-sort after adding expanded pages
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Deduplicate by page ID (keep highest score, which comes first after sort)
        let mut seen_ids: HashSet<&str> = HashSet::new();
        let mut deduped: Vec<(&WikiPage, f32)> = Vec::new();
        for (page, score) in &scored {
            if seen_ids.insert(&page.id) {
                deduped.push((*page, *score));
            }
        }

        deduped.truncate(limit);
        deduped
    }

    // ── Keyword matching helpers ──

    /// Tokenize a string into lowercase keywords.
    ///
    /// Splits on whitespace and punctuation, filters out short tokens
    /// and common stop words. Uses a cached stop word set for efficiency.
    pub fn tokenize(text: &str) -> Vec<String> {
        text.to_lowercase()
            .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
            .filter(|token| token.len() >= 2 && !STOP_WORDS.contains(token))
            .map(|s| s.to_string())
            .collect()
    }

    /// Keyword relevance weight for each page field.
    ///
    /// Higher weights mean stronger relevance signal. The weights are:
    /// - Title: 3.0 (strongest — title is the most descriptive field)
    /// - Page ID: 2.0 (slug often mirrors the title)
    /// - Tags: 2.0 (curated classification labels)
    /// - Body: 1.0 (weakest — body is large, matches are less specific)
    const FIELD_WEIGHTS: [(fn(&WikiPage) -> String, f32); 3] = [
        (|p| p.frontmatter.title.to_lowercase(), 3.0),
        (|p| p.id.to_lowercase(), 2.0),
        (|p| p.body.to_lowercase(), 1.0),
    ];

    /// Score a page's relevance to a set of query keywords.
    ///
    /// Uses a weight-table driven approach: each page field is checked
    /// for keyword presence with a different weight. Tags are handled
    /// separately because they require per-tag matching.
    fn keyword_score(page: &WikiPage, query_tokens: &[String]) -> f32 {
        if query_tokens.is_empty() {
            return 0.0;
        }

        let tags_lower: Vec<String> = page.frontmatter.tags.iter()
            .map(|t| t.to_lowercase())
            .collect();

        let mut total_score: f32 = 0.0;

        for token in query_tokens {
            // Score against each weighted field
            for (field_fn, weight) in &Self::FIELD_WEIGHTS {
                let field_text = field_fn(page);
                if field_text.contains(token.as_str()) {
                    total_score += weight;
                }
            }
            // Tags scored separately (per-tag matching, weight 2.0)
            if tags_lower.iter().any(|tag| tag.contains(token.as_str())) {
                total_score += 2.0;
            }
        }

        // Normalize by number of query tokens to get a 0..~8 range
        total_score / query_tokens.len() as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::page::PageType;

    fn make_page(id: &str, title: &str, tags: Vec<&str>, links: Vec<&str>, body: &str) -> WikiPage {
        WikiPage::new(
            id.to_string(),
            title.to_string(),
            PageType::Topic,
            tags.into_iter().map(|s| s.to_string()).collect(),
            links.into_iter().map(|s| s.to_string()).collect(),
            body.to_string(),
        )
    }

    fn make_pages(pages: Vec<WikiPage>) -> HashMap<String, WikiPage> {
        pages.into_iter().map(|p| (p.id.clone(), p)).collect()
    }

    #[test]
    fn test_tokenize() {
        let tokens = WikiRetriever::tokenize("How does Rust handle memory ownership?");
        assert!(tokens.contains(&"rust".to_string()));
        assert!(tokens.contains(&"handle".to_string()));
        assert!(tokens.contains(&"memory".to_string()));
        assert!(tokens.contains(&"ownership".to_string()));
        // Stop words should be filtered
        assert!(!tokens.contains(&"how".to_string()));
        assert!(!tokens.contains(&"does".to_string()));
    }

    #[test]
    fn test_tokenize_empty() {
        let tokens = WikiRetriever::tokenize("the a an is");
        assert!(tokens.is_empty());
    }

    #[test]
    fn test_keyword_retrieval_basic() {
        let pages = make_pages(vec![
            make_page(
                "rust-ownership",
                "Rust Ownership Model",
                vec!["rust", "memory-safety"],
                vec![],
                "# Rust Ownership\n\nRust uses an ownership system for memory management.",
            ),
            make_page(
                "python-gc",
                "Python Garbage Collection",
                vec!["python", "gc"],
                vec![],
                "# Python GC\n\nPython uses reference counting and garbage collection.",
            ),
        ]);

        let results = WikiRetriever::retrieve_by_keywords(&pages, "rust ownership memory", 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].0.id, "rust-ownership");
    }

    #[test]
    fn test_keyword_retrieval_with_wikilinks() {
        let mut pages = make_pages(vec![
            make_page(
                "rust-ownership",
                "Rust Ownership",
                vec!["rust"],
                vec!["rust-borrowing"],
                "# Rust Ownership\n\nContent about ownership.",
            ),
            make_page(
                "rust-borrowing",
                "Rust Borrowing",
                vec!["rust"],
                vec![],
                "# Rust Borrowing\n\nContent about borrowing.",
            ),
            make_page(
                "unrelated-page",
                "Unrelated Page",
                vec!["other"],
                vec![],
                "# Unrelated\n\nNothing about rust here.",
            ),
        ]);

        // Rebuild backlinks manually for test
        let links: Vec<(String, String)> = pages
            .values()
            .flat_map(|p| p.frontmatter.links.iter().map(move |t| (p.id.clone(), t.clone())))
            .collect();
        for page in pages.values_mut() {
            page.backlinks.clear();
        }
        for (source, target) in links {
            if let Some(tp) = pages.get_mut(&target) {
                tp.backlinks.insert(source);
            }
        }

        let results = WikiRetriever::retrieve_by_keywords(&pages, "ownership", 5);
        let result_ids: Vec<&str> = results.iter().map(|(p, _)| p.id.as_str()).collect();
        assert!(result_ids.contains(&"rust-ownership"));
        assert!(result_ids.contains(&"rust-borrowing")); // via wikilink
    }

    #[test]
    fn test_keyword_retrieval_empty() {
        let pages: HashMap<String, WikiPage> = HashMap::new();
        let results = WikiRetriever::retrieve_by_keywords(&pages, "anything", 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_score_pages() {
        let pages = make_pages(vec![
            make_page("rust", "Rust Language", vec!["rust"], vec![], "Rust content"),
            make_page("python", "Python Language", vec!["python"], vec![], "Python content"),
        ]);

        let tokens = WikiRetriever::tokenize("rust language");
        let scored = WikiRetriever::score_pages(&pages, &tokens);
        assert!(!scored.is_empty());
        assert_eq!(scored[0].0.id, "rust");
    }

    #[test]
    fn test_expand_via_wikilinks() {
        let mut pages = make_pages(vec![
            make_page("a", "Page A", vec![], vec!["b"], "body"),
            make_page("b", "Page B", vec![], vec![], "body"),
            make_page("c", "Page C", vec![], vec![], "body"),
        ]);
        // Set backlink: c -> a
        pages.get_mut("a").unwrap().backlinks.insert("c".to_string());

        let expanded = WikiRetriever::expand_via_wikilinks(&pages, &["a".to_string()]);
        assert!(expanded.contains(&"b".to_string())); // forward link
        assert!(expanded.contains(&"c".to_string())); // backlink
    }

    #[test]
    fn test_deduplicate_and_truncate() {
        let pages = make_pages(vec![
            make_page("a", "A", vec![], vec![], "body"),
            make_page("b", "B", vec![], vec![], "body"),
        ]);
        let page_a = pages.get("a").unwrap();
        let page_b = pages.get("b").unwrap();

        // Duplicate entries for page_a with different scores
        let scored = vec![(page_a, 0.9), (page_b, 0.5), (page_a, 0.3)];
        let result = WikiRetriever::deduplicate_and_truncate(scored, 10);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0.id, "a");
        assert_eq!(result[0].1, 0.9); // highest score kept
    }

    #[test]
    fn test_stop_words_cached() {
        // Access STOP_WORDS twice to verify LazyLock works
        assert!(STOP_WORDS.contains("the"));
        assert!(STOP_WORDS.contains("and"));
        assert!(!STOP_WORDS.contains("rust"));
    }
}
