use std::collections::HashSet;

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

// ── Page type classification ──

/// Page type classification for categorical organization.
///
/// Each wiki page has a type that determines how it appears in the
/// index and how the compiler treats it during knowledge compilation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PageType {
    /// Master index page (one per wiki, auto-maintained).
    Index,
    /// Entity page (person, project, tool, API, etc.).
    Entity,
    /// Topic/concept page (design pattern, algorithm, etc.).
    Topic,
    /// Summary/overview page (aggregates multiple entities/topics).
    Summary,
    /// Log page (chronological record of changes).
    /// Reserved for future use.
    #[allow(dead_code)]
    Log,
}

impl std::fmt::Display for PageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Index => write!(f, "index"),
            Self::Entity => write!(f, "entity"),
            Self::Topic => write!(f, "topic"),
            Self::Summary => write!(f, "summary"),
            Self::Log => write!(f, "log"),
        }
    }
}

// ── YAML frontmatter ──

/// YAML frontmatter structure for a wiki page.
///
/// This is the structured metadata stored in the YAML frontmatter
/// of each `.md` file. Kept separate from `WikiPage` to cleanly
/// map between on-disk format and in-memory representation.
///
/// ## Example
///
/// ```yaml
/// ---
/// title: Rust Ownership Model
/// page_type: concept
/// tags:
///   - rust
///   - memory-safety
/// links:
///   - rust-borrowing
///   - project-daedalus
/// created_at: "2026-04-16T10:00:00+08:00"
/// updated_at: "2026-04-16T10:30:00+08:00"
/// revision_count: 3
/// ---
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageFrontmatter {
    /// Human-readable title.
    pub title: String,
    /// Page type for categorical organization.
    pub page_type: PageType,
    /// LLM-generated tags for categorical classification.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Wikilinks to other pages (by page id / slug).
    #[serde(default)]
    pub links: Vec<String>,
    /// When this page was first created.
    pub created_at: DateTime<Local>,
    /// When this page was last updated (compiled/evolved).
    pub updated_at: DateTime<Local>,
    /// Number of times this page has been updated (compile count).
    #[serde(default)]
    pub revision_count: u32,
}

// ── Wiki page (in-memory representation) ──

/// A single page in the LLM Wiki (in-memory representation).
///
/// Each page corresponds to a `.md` file on disk. The `id` is derived
/// from the filename (e.g., `rust-ownership.md` → id `rust-ownership`).
///
/// ## On-disk format
///
/// ```markdown
/// ---
/// title: Rust Ownership Model
/// page_type: topic
/// tags: [rust, memory-safety]
/// links: [rust-borrowing, project-daedalus]
/// created_at: "2026-04-16T10:00:00+08:00"
/// updated_at: "2026-04-16T10:30:00+08:00"
/// revision_count: 3
/// ---
///
/// # Rust Ownership Model
///
/// Rust's ownership system is a set of rules...
///
/// ## Related
/// - [[rust-borrowing]] — Borrowing rules
/// ```
#[derive(Debug, Clone)]
pub struct WikiPage {
    /// Unique page identifier (slug format, derived from filename).
    pub id: String,
    /// Structured metadata (serialized as YAML frontmatter).
    pub frontmatter: PageFrontmatter,
    /// The Markdown body content (everything after the frontmatter).
    pub body: String,
    /// Backlinks from other pages (computed at load time, not persisted).
    ///
    /// Restricted to `pub(super)` because backlinks are derived data managed
    /// exclusively by `WikiStore::rebuild_backlinks()`. External code should
    /// not modify them directly.
    pub(super) backlinks: HashSet<String>,
    /// Embedding vector for similarity-based retrieval (stored in _meta.json).
    ///
    /// Restricted to `pub(super)` because embeddings are machine data managed
    /// by `WikiCompiler` (write) and `WikiRetriever` (read). External code
    /// should not access them directly.
    pub(super) embedding: Vec<f32>,
}

impl WikiPage {
    /// Create a new wiki page with the given metadata and body.
    pub fn new(
        id: String,
        title: String,
        page_type: PageType,
        tags: Vec<String>,
        links: Vec<String>,
        body: String,
    ) -> Self {
        let now = Local::now();
        Self {
            id,
            frontmatter: PageFrontmatter {
                title,
                page_type,
                tags,
                links,
                created_at: now,
                updated_at: now,
                revision_count: 1,
            },
            body,
            backlinks: HashSet::new(),
            embedding: Vec::new(),
        }
    }

    /// Update the page body and metadata during a compile operation.
    ///
    /// Increments the revision count and updates the timestamp.
    pub fn update(&mut self, body: String, tags: Vec<String>, links: Vec<String>) {
        self.body = body;
        self.frontmatter.tags = tags;
        self.frontmatter.links = links;
        self.frontmatter.updated_at = Local::now();
        self.frontmatter.revision_count += 1;
    }

    /// Format this page as a compact text representation for LLM prompts.
    ///
    /// Used when providing wiki context to the LLM for compilation,
    /// query answering, or lint operations.
    pub fn to_prompt_text(&self) -> String {
        let tags_str = self.frontmatter.tags.join(", ");
        let links_str = self.frontmatter.links.join(", ");
        format!(
            "[Page: {}]\nTitle: {}\nType: {}\nTags: {}\nLinks: {}\n\n{}",
            self.id,
            self.frontmatter.title,
            self.frontmatter.page_type,
            tags_str,
            links_str,
            self.body,
        )
    }

    /// Return a short summary line for index generation.
    pub fn index_entry(&self) -> String {
        format!("- [[{}]] — {}", self.id, self.frontmatter.title)
    }
}

// ── Markdown serialization helpers ──

/// Split a Markdown file content into YAML frontmatter and body.
///
/// Expects the content to start with `---`, followed by YAML, then `---`,
/// then the Markdown body.
pub fn split_frontmatter(content: &str) -> anyhow::Result<(PageFrontmatter, String)> {
    let content = content.trim_start();
    if !content.starts_with("---") {
        anyhow::bail!("Missing YAML frontmatter opening delimiter '---'");
    }
    let after_first = &content[3..];
    let end = after_first
        .find("\n---")
        .ok_or_else(|| anyhow::anyhow!("Missing closing frontmatter delimiter '---'"))?;
    let yaml_str = &after_first[..end];
    let body = after_first[end + 4..].trim_start_matches('\n').to_string();
    let frontmatter: PageFrontmatter = serde_yaml::from_str(yaml_str)
        .map_err(|e| anyhow::anyhow!("Failed to parse YAML frontmatter: {}", e))?;
    Ok((frontmatter, body))
}

/// Render a `WikiPage` to a Markdown string with YAML frontmatter.
pub fn render_page(page: &WikiPage) -> anyhow::Result<String> {
    let yaml = serde_yaml::to_string(&page.frontmatter)
        .map_err(|e| anyhow::anyhow!("Failed to serialize YAML frontmatter: {}", e))?;
    Ok(format!("---\n{}---\n\n{}", yaml, page.body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_page() {
        let page = WikiPage::new(
            "rust-ownership".to_string(),
            "Rust Ownership Model".to_string(),
            PageType::Topic,
            vec!["rust".to_string(), "memory-safety".to_string()],
            vec!["rust-borrowing".to_string()],
            "# Rust Ownership\n\nContent here.".to_string(),
        );

        assert_eq!(page.id, "rust-ownership");
        assert_eq!(page.frontmatter.title, "Rust Ownership Model");
        assert_eq!(page.frontmatter.page_type, PageType::Topic);
        assert_eq!(page.frontmatter.tags.len(), 2);
        assert_eq!(page.frontmatter.links.len(), 1);
        assert_eq!(page.frontmatter.revision_count, 1);
        assert!(page.backlinks.is_empty());
        assert!(page.embedding.is_empty());
    }

    #[test]
    fn test_update_page() {
        let mut page = WikiPage::new(
            "test".to_string(),
            "Test".to_string(),
            PageType::Entity,
            vec![],
            vec![],
            "old body".to_string(),
        );
        let original_created = page.frontmatter.created_at;

        page.update(
            "new body".to_string(),
            vec!["new-tag".to_string()],
            vec!["linked-page".to_string()],
        );

        assert_eq!(page.body, "new body");
        assert_eq!(page.frontmatter.tags, vec!["new-tag"]);
        assert_eq!(page.frontmatter.links, vec!["linked-page"]);
        assert_eq!(page.frontmatter.revision_count, 2);
        assert_eq!(page.frontmatter.created_at, original_created);
        assert!(page.frontmatter.updated_at >= original_created);
    }

    #[test]
    fn test_split_frontmatter() {
        let content = "---\ntitle: Test Page\npage_type: entity\ntags: []\nlinks: []\ncreated_at: \"2026-04-16T10:00:00+08:00\"\nupdated_at: \"2026-04-16T10:00:00+08:00\"\nrevision_count: 1\n---\n\n# Test Page\n\nBody content here.";
        let (fm, body) = split_frontmatter(content).unwrap();
        assert_eq!(fm.title, "Test Page");
        assert_eq!(fm.page_type, PageType::Entity);
        assert!(body.contains("Body content here."));
    }

    #[test]
    fn test_split_frontmatter_missing_opening() {
        let content = "No frontmatter here.";
        assert!(split_frontmatter(content).is_err());
    }

    #[test]
    fn test_split_frontmatter_missing_closing() {
        let content = "---\ntitle: Test\n";
        assert!(split_frontmatter(content).is_err());
    }

    #[test]
    fn test_render_page_roundtrip() {
        let page = WikiPage::new(
            "test-page".to_string(),
            "Test Page".to_string(),
            PageType::Entity,
            vec!["tag1".to_string()],
            vec!["other-page".to_string()],
            "# Test Page\n\nSome content.".to_string(),
        );

        let rendered = render_page(&page).unwrap();
        assert!(rendered.starts_with("---\n"));
        assert!(rendered.contains("title: Test Page"));
        assert!(rendered.contains("# Test Page"));
        assert!(rendered.contains("Some content."));

        // Parse it back
        let (fm, body) = split_frontmatter(&rendered).unwrap();
        assert_eq!(fm.title, "Test Page");
        assert_eq!(fm.page_type, PageType::Entity);
        assert!(body.contains("Some content."));
    }

    #[test]
    fn test_to_prompt_text() {
        let page = WikiPage::new(
            "rust-ownership".to_string(),
            "Rust Ownership".to_string(),
            PageType::Topic,
            vec!["rust".to_string()],
            vec!["rust-borrowing".to_string()],
            "# Rust Ownership\n\nContent.".to_string(),
        );

        let text = page.to_prompt_text();
        assert!(text.contains("[Page: rust-ownership]"));
        assert!(text.contains("Title: Rust Ownership"));
        assert!(text.contains("Tags: rust"));
        assert!(text.contains("Links: rust-borrowing"));
        assert!(text.contains("# Rust Ownership"));
    }

    #[test]
    fn test_index_entry() {
        let page = WikiPage::new(
            "my-page".to_string(),
            "My Page Title".to_string(),
            PageType::Entity,
            vec![],
            vec![],
            "body".to_string(),
        );
        assert_eq!(page.index_entry(), "- [[my-page]] — My Page Title");
    }
}
