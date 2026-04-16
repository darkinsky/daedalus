use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Local;

use crate::embedding::Embedding;
use crate::memory::persistence::MemoryPersistence;

use super::meta::WikiMeta;
use super::page::{render_page, split_frontmatter, PageType, WikiPage};
use super::retriever::WikiRetriever;

/// Default lint interval (every N conversation turns).
const DEFAULT_LINT_INTERVAL: usize = 10;

/// Default maximum number of pages to retrieve for context injection.
const DEFAULT_MAX_RETRIEVAL_PAGES: usize = 5;

/// The core Wiki engine with Markdown file persistence.
///
/// Implements Karpathy's three-layer architecture:
/// - **Raw Layer**: Conversation turns (transient, handled by WikiMemory)
/// - **Wiki Layer**: Structured, interlinked Markdown pages (persisted as `.md` files)
/// - **Schema Layer**: Rules governing page structure (enforced by prompts + frontmatter)
///
/// ## On-disk layout
/// ```text
/// wiki_dir/
/// ├── _index.md       # Master index (auto-maintained)
/// ├── _meta.json      # Embeddings + lint state
/// ├── page-a.md       # Knowledge page
/// └── page-b.md       # Knowledge page
/// ```
///
/// ## Key design decisions
/// - Each page is a `.md` file with YAML frontmatter (Obsidian-compatible)
/// - Embedding vectors stored in `_meta.json` (not in Markdown)
/// - Backlinks computed at load time from forward links
/// - System files prefixed with `_` are auto-managed
pub struct WikiStore {
    /// All wiki pages, indexed by page ID (slug).
    pages: HashMap<String, WikiPage>,
    /// Machine-only metadata (embeddings, lint state).
    meta: WikiMeta,
    /// Root directory for wiki files.
    wiki_dir: PathBuf,
    /// Counter for triggering periodic lint operations.
    turns_since_last_lint: usize,
    /// Lint interval (every N turns).
    lint_interval: usize,
    /// Maximum number of pages to retrieve for context injection.
    max_retrieval_pages: usize,
}

impl WikiStore {
    /// Create a new empty wiki store for the given directory.
    pub fn new(wiki_dir: &Path) -> Self {
        Self {
            pages: HashMap::new(),
            meta: WikiMeta::new(),
            wiki_dir: wiki_dir.to_path_buf(),
            turns_since_last_lint: 0,
            lint_interval: DEFAULT_LINT_INTERVAL,
            max_retrieval_pages: DEFAULT_MAX_RETRIEVAL_PAGES,
        }
    }

    /// Create a store with custom configuration.
    #[allow(dead_code)]
    pub fn with_config(
        wiki_dir: &Path,
        lint_interval: usize,
        max_retrieval_pages: usize,
    ) -> Self {
        Self {
            pages: HashMap::new(),
            meta: WikiMeta::new(),
            wiki_dir: wiki_dir.to_path_buf(),
            turns_since_last_lint: 0,
            lint_interval,
            max_retrieval_pages,
        }
    }

    // ── Accessors ──

    /// Return the number of knowledge pages (excluding system pages).
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Check if the store is empty.
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// Get a reference to a page by ID.
    #[allow(dead_code)]
    pub fn get_page(&self, id: &str) -> Option<&WikiPage> {
        self.pages.get(id)
    }

    /// Get all pages as an iterator.
    #[allow(dead_code)]
    pub fn all_pages(&self) -> impl Iterator<Item = &WikiPage> {
        self.pages.values()
    }

    /// Check if a lint operation should be triggered.
    pub fn should_lint(&self) -> bool {
        self.turns_since_last_lint >= self.lint_interval && !self.pages.is_empty()
    }

    /// Increment the turn counter (called after each conversation turn).
    pub fn increment_turn(&mut self) {
        self.turns_since_last_lint += 1;
    }

    /// Reset the lint counter after a successful lint operation.
    pub fn reset_lint_counter(&mut self) {
        self.turns_since_last_lint = 0;
    }

    /// Get the wiki directory path.
    pub fn wiki_dir(&self) -> &Path {
        &self.wiki_dir
    }

    // ── Page CRUD ──

    /// Add a new page to the store.
    ///
    /// If a page with the same ID already exists, it is replaced.
    pub fn add_page(&mut self, page: WikiPage) {
        self.pages.insert(page.id.clone(), page);
    }

    /// Update an existing page's body, tags, links, and optionally embedding.
    ///
    /// If the page doesn't exist, this is a no-op (logged as warning).
    pub fn update_page(
        &mut self,
        page_id: &str,
        body: String,
        tags: Vec<String>,
        links: Vec<String>,
        embedding: Option<Vec<f32>>,
    ) {
        if let Some(page) = self.pages.get_mut(page_id) {
            page.update(body, tags, links);
            if let Some(emb) = embedding {
                page.embedding = emb;
            }
        } else {
            tracing::warn!(
                page_id = %page_id,
                "Wiki: attempted to update non-existent page"
            );
        }
    }

    /// Remove a page from the store.
    #[allow(dead_code)]
    pub fn remove_page(&mut self, page_id: &str) -> Option<WikiPage> {
        self.pages.remove(page_id)
    }

    // ── Backlinks ──

    /// Rebuild backlinks by scanning all pages' forward links.
    ///
    /// Called after batch updates to ensure backlinks are consistent.
    pub fn rebuild_backlinks(&mut self) {
        // Collect all forward links: (source_id, target_id)
        let links: Vec<(String, String)> = self
            .pages
            .values()
            .flat_map(|page| {
                page.frontmatter
                    .links
                    .iter()
                    .map(move |target| (page.id.clone(), target.clone()))
            })
            .collect();

        // Clear existing backlinks
        for page in self.pages.values_mut() {
            page.backlinks.clear();
        }

        // Populate backlinks
        for (source, target) in links {
            if let Some(target_page) = self.pages.get_mut(&target) {
                target_page.backlinks.insert(source);
            }
        }
    }

    // ── Query / Retrieval (delegated to WikiRetriever) ──

    /// Query relevant pages and format them as context for the LLM.
    ///
    /// Delegates to `WikiRetriever` for the actual retrieval logic.
    /// Returns a Markdown-formatted string of relevant wiki pages,
    /// or `None` if no relevant pages are found.
    pub async fn query_context(
        &self,
        query: &str,
        embedder: Option<&dyn Embedding>,
        limit: Option<usize>,
    ) -> Result<Option<String>> {
        let limit = limit.unwrap_or(self.max_retrieval_pages);
        WikiRetriever::retrieve_context(&self.pages, query, embedder, limit).await
    }

    /// Provide read-only access to pages for external retrieval.
    ///
    /// Used by `WikiRetriever` when called directly (e.g., in tests).
    #[allow(dead_code)]
    pub fn pages(&self) -> &HashMap<String, WikiPage> {
        &self.pages
    }

    // ── Prompt helpers ──

    /// Build a compact page listing for the compile prompt.
    ///
    /// Returns a string like:
    /// ```text
    /// - rust-ownership: Rust Ownership Model [topic] (tags: rust, memory-safety)
    /// - project-daedalus: Daedalus Project [entity] (tags: project, ai)
    /// ```
    pub fn page_listing(&self) -> String {
        if self.pages.is_empty() {
            return "(empty wiki — no pages yet)".to_string();
        }

        let mut entries: Vec<String> = self
            .pages
            .values()
            .map(|page| {
                format!(
                    "- {}: {} [{}] (tags: {})",
                    page.id,
                    page.frontmatter.title,
                    page.frontmatter.page_type,
                    page.frontmatter.tags.join(", "),
                )
            })
            .collect();
        entries.sort();
        entries.join("\n")
    }

    /// Build a full text representation of all pages for lint.
    pub fn all_pages_prompt_text(&self) -> String {
        let mut texts: Vec<String> = self
            .pages
            .values()
            .map(|page| page.to_prompt_text())
            .collect();
        texts.sort();
        texts.join("\n\n---\n\n")
    }

    /// Render all pages as a compact Markdown summary for system prompt injection.
    #[allow(dead_code)]
    pub fn to_markdown_summary(&self) -> Option<String> {
        if self.pages.is_empty() {
            return None;
        }

        let mut pages: Vec<&WikiPage> = self.pages.values().collect();
        pages.sort_by(|a, b| b.frontmatter.updated_at.cmp(&a.frontmatter.updated_at));

        let sections: Vec<String> = pages
            .iter()
            .take(10)
            .map(|page| {
                format!(
                    "- **{}**: {} [{}]",
                    page.frontmatter.title,
                    page.frontmatter.tags.join(", "),
                    page.frontmatter.page_type,
                )
            })
            .collect();

        Some(format!("## Wiki Knowledge\n\n{}", sections.join("\n")))
    }

    // ── Persistence (Markdown files) ──

    /// Load the entire wiki from a directory.
    ///
    /// 1. Scan for all `.md` files (excluding `_` prefixed system files).
    /// 2. Parse each file into a `WikiPage`.
    /// 3. Load `_meta.json` and attach embeddings to pages.
    /// 4. Compute backlinks from forward links.
    pub fn load_from_dir(wiki_dir: &Path) -> Result<Self> {
        let mut store = Self::new(wiki_dir);

        if !wiki_dir.exists() {
            return Ok(store);
        }

        // Load _meta.json
        let meta_path = wiki_dir.join("_meta.json");
        if meta_path.exists() {
            let content = std::fs::read_to_string(&meta_path)
                .with_context(|| format!("Failed to read _meta.json: {}", meta_path.display()))?;
            store.meta = serde_json::from_str(&content)
                .with_context(|| format!("Failed to parse _meta.json: {}", meta_path.display()))?;
            store.turns_since_last_lint = store.meta.last_lint_turn;
        }

        // Scan and parse all .md files
        let entries = std::fs::read_dir(wiki_dir)
            .with_context(|| format!("Failed to read wiki directory: {}", wiki_dir.display()))?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "md") {
                let filename = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                // Skip system files (prefixed with _)
                if filename.starts_with('_') {
                    continue;
                }
                match Self::parse_page_file(&path) {
                    Ok(mut page) => {
                        // Attach embedding from meta
                        if let Some(emb) = store.meta.embeddings.get(&page.id) {
                            page.embedding = emb.clone();
                        }
                        store.pages.insert(page.id.clone(), page);
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %e,
                            "Failed to parse wiki page, skipping"
                        );
                    }
                }
            }
        }

        // Clean up orphaned embeddings
        let page_ids: Vec<&str> = store.pages.keys().map(|s| s.as_str()).collect();
        store.meta.cleanup_orphaned_embeddings(&page_ids);

        // Compute backlinks
        store.rebuild_backlinks();

        tracing::info!(
            path = %wiki_dir.display(),
            pages = store.pages.len(),
            "WikiStore loaded from Markdown files"
        );

        Ok(store)
    }

    /// Save the entire wiki state to disk.
    ///
    /// 1. Save each page as a `.md` file.
    /// 2. Update and save `_meta.json` (embeddings + lint state).
    /// 3. Regenerate `_index.md`.
    pub fn save_to_dir(&self) -> Result<()> {
        // Ensure wiki directory exists
        std::fs::create_dir_all(&self.wiki_dir)
            .with_context(|| format!("Failed to create wiki directory: {}", self.wiki_dir.display()))?;

        // Save all pages as .md files
        for page in self.pages.values() {
            self.save_page_file(page)?;
        }

        // Save _meta.json
        let mut meta = self.meta.clone();
        meta.embeddings.clear();
        for (id, page) in &self.pages {
            if !page.embedding.is_empty() {
                meta.embeddings.insert(id.clone(), page.embedding.clone());
            }
        }
        meta.last_lint_turn = self.turns_since_last_lint;
        let meta_json = serde_json::to_string_pretty(&meta)
            .context("Failed to serialize _meta.json")?;
        let meta_path = self.wiki_dir.join("_meta.json");
        crate::memory::persistence::atomic_write(&meta_path, meta_json.as_bytes())
            .with_context(|| format!("Failed to write _meta.json: {}", meta_path.display()))?;

        // Regenerate _index.md
        self.save_index_page()?;

        tracing::debug!(
            path = %self.wiki_dir.display(),
            pages = self.pages.len(),
            "WikiStore saved to Markdown files"
        );

        Ok(())
    }

    /// Parse a single `.md` file into a `WikiPage`.
    fn parse_page_file(path: &Path) -> Result<WikiPage> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read wiki page: {}", path.display()))?;
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow::anyhow!("Invalid page filename: {}", path.display()))?
            .to_string();

        let (frontmatter, body) = split_frontmatter(&content)
            .with_context(|| format!("Failed to parse frontmatter: {}", path.display()))?;

        Ok(WikiPage {
            id,
            frontmatter,
            body,
            backlinks: std::collections::HashSet::new(),
            embedding: Vec::new(),
        })
    }

    /// Save a single page to disk as a `.md` file.
    fn save_page_file(&self, page: &WikiPage) -> Result<()> {
        let path = self.wiki_dir.join(format!("{}.md", page.id));
        let content = render_page(page)
            .with_context(|| format!("Failed to render wiki page: {}", page.id))?;
        crate::memory::persistence::atomic_write(&path, content.as_bytes())
            .with_context(|| format!("Failed to write wiki page: {}", path.display()))
    }

    /// Generate and save the `_index.md` master index page.
    fn save_index_page(&self) -> Result<()> {
        let mut entities = Vec::new();
        let mut topics = Vec::new();
        let mut summaries = Vec::new();

        for page in self.pages.values() {
            let entry = page.index_entry();
            match page.frontmatter.page_type {
                PageType::Entity => entities.push(entry),
                PageType::Topic => topics.push(entry),
                PageType::Summary => summaries.push(entry),
                _ => {}
            }
        }

        entities.sort();
        topics.sort();
        summaries.sort();

        let now = Local::now();
        let mut content = format!(
            "---\ntitle: Wiki Index\npage_type: index\nupdated_at: \"{}\"\n---\n\n# Knowledge Wiki Index\n\n",
            now.to_rfc3339()
        );

        Self::write_index_section(&mut content, "Entities", &entities);
        Self::write_index_section(&mut content, "Topics", &topics);
        Self::write_index_section(&mut content, "Summaries", &summaries);

        let index_path = self.wiki_dir.join("_index.md");
        crate::memory::persistence::atomic_write(&index_path, content.as_bytes())
            .with_context(|| "Failed to write _index.md")
    }

    /// Write a single section to the index page content.
    ///
    /// Appends a `## {heading}` section with the given entries.
    /// Does nothing if `entries` is empty.
    fn write_index_section(content: &mut String, heading: &str, entries: &[String]) {
        if entries.is_empty() {
            return;
        }
        content.push_str(&format!("## {}\n", heading));
        for entry in entries {
            content.push_str(entry);
            content.push('\n');
        }
        content.push('\n');
    }
}

// ── MemoryPersistence implementation ──
//
// For WikiStore, the `path` parameter in save/load represents the
// **wiki directory** (not a single file), since the wiki is a collection
// of Markdown files.

impl MemoryPersistence for WikiStore {
    fn save(&self, path: &Path) -> Result<()> {
        // If the store's wiki_dir differs from the given path,
        // we need to save to the specified path instead.
        if self.wiki_dir == path {
            self.save_to_dir()
        } else {
            // Create a temporary store pointing to the new path
            let mut redirected = Self::new(path);
            redirected.pages = self.pages.clone();
            redirected.meta = self.meta.clone();
            redirected.turns_since_last_lint = self.turns_since_last_lint;
            redirected.save_to_dir()
        }
    }

    fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            tracing::debug!(
                path = %path.display(),
                "No wiki directory found, using default"
            );
            return Ok(Self::new(path));
        }
        Self::load_from_dir(path)
    }
}

impl Clone for WikiStore {
    fn clone(&self) -> Self {
        Self {
            pages: self.pages.clone(),
            meta: self.meta.clone(),
            wiki_dir: self.wiki_dir.clone(),
            turns_since_last_lint: self.turns_since_last_lint,
            lint_interval: self.lint_interval,
            max_retrieval_pages: self.max_retrieval_pages,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_new_store() {
        let store = WikiStore::new(Path::new("/tmp/test_wiki"));
        assert!(store.is_empty());
        assert_eq!(store.page_count(), 0);
    }

    #[test]
    fn test_add_and_get_page() {
        let mut store = WikiStore::new(Path::new("/tmp/test_wiki"));
        let page = WikiPage::new(
            "test-page".to_string(),
            "Test Page".to_string(),
            PageType::Entity,
            vec!["test".to_string()],
            vec![],
            "# Test\n\nContent.".to_string(),
        );

        store.add_page(page);
        assert_eq!(store.page_count(), 1);
        assert!(store.get_page("test-page").is_some());
        assert_eq!(store.get_page("test-page").unwrap().frontmatter.title, "Test Page");
    }

    #[test]
    fn test_update_page() {
        let mut store = WikiStore::new(Path::new("/tmp/test_wiki"));
        let page = WikiPage::new(
            "test".to_string(),
            "Test".to_string(),
            PageType::Entity,
            vec![],
            vec![],
            "old body".to_string(),
        );
        store.add_page(page);

        store.update_page("test", "new body".to_string(), vec!["tag".to_string()], vec![], None);

        let updated = store.get_page("test").unwrap();
        assert_eq!(updated.body, "new body");
        assert_eq!(updated.frontmatter.tags, vec!["tag"]);
        assert_eq!(updated.frontmatter.revision_count, 2);
    }

    #[test]
    fn test_rebuild_backlinks() {
        let mut store = WikiStore::new(Path::new("/tmp/test_wiki"));

        let page_a = WikiPage::new(
            "page-a".to_string(),
            "Page A".to_string(),
            PageType::Entity,
            vec![],
            vec!["page-b".to_string()],
            "Links to [[page-b]]".to_string(),
        );
        let page_b = WikiPage::new(
            "page-b".to_string(),
            "Page B".to_string(),
            PageType::Entity,
            vec![],
            vec![],
            "No links".to_string(),
        );

        store.add_page(page_a);
        store.add_page(page_b);
        store.rebuild_backlinks();

        assert!(store.get_page("page-b").unwrap().backlinks.contains("page-a"));
        assert!(store.get_page("page-a").unwrap().backlinks.is_empty());
    }

    #[test]
    fn test_page_listing() {
        let mut store = WikiStore::new(Path::new("/tmp/test_wiki"));
        assert!(store.page_listing().contains("empty wiki"));

        let page = WikiPage::new(
            "test".to_string(),
            "Test Page".to_string(),
            PageType::Entity,
            vec!["tag1".to_string()],
            vec![],
            "body".to_string(),
        );
        store.add_page(page);

        let listing = store.page_listing();
        assert!(listing.contains("test: Test Page"));
        assert!(listing.contains("[entity]"));
        assert!(listing.contains("tag1"));
    }

    #[test]
    fn test_should_lint() {
        let mut store = WikiStore::with_config(Path::new("/tmp"), 3, 5);
        let page = WikiPage::new(
            "test".to_string(),
            "Test".to_string(),
            PageType::Entity,
            vec![],
            vec![],
            "body".to_string(),
        );
        store.add_page(page);

        assert!(!store.should_lint());
        store.increment_turn();
        store.increment_turn();
        assert!(!store.should_lint());
        store.increment_turn();
        assert!(store.should_lint());

        store.reset_lint_counter();
        assert!(!store.should_lint());
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let dir = std::env::temp_dir().join("daedalus_wiki_test_roundtrip");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Create and save
        let mut store = WikiStore::new(&dir);
        let mut page = WikiPage::new(
            "test-page".to_string(),
            "Test Page".to_string(),
            PageType::Topic,
            vec!["rust".to_string(), "test".to_string()],
            vec![],
            "# Test Page\n\nSome content here.".to_string(),
        );
        page.embedding = vec![0.1, 0.2, 0.3];
        store.add_page(page);
        store.save_to_dir().unwrap();

        // Verify files exist
        assert!(dir.join("test-page.md").exists());
        assert!(dir.join("_meta.json").exists());
        assert!(dir.join("_index.md").exists());

        // Load and verify
        let loaded = WikiStore::load_from_dir(&dir).unwrap();
        assert_eq!(loaded.page_count(), 1);
        let loaded_page = loaded.get_page("test-page").unwrap();
        assert_eq!(loaded_page.frontmatter.title, "Test Page");
        assert_eq!(loaded_page.frontmatter.page_type, PageType::Topic);
        assert_eq!(loaded_page.frontmatter.tags, vec!["rust", "test"]);
        assert!(loaded_page.body.contains("Some content here."));
        assert_eq!(loaded_page.embedding, vec![0.1, 0.2, 0.3]);

        let _ = fs::remove_dir_all(&dir);
    }
}
