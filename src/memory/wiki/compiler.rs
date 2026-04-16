use anyhow::{Context, Result};

use crate::embedding::Embedding;
use crate::llm::{ChatMessage, LlmApi};

use super::page::{PageType, WikiPage};
use super::prompts::{build_compile_prompt, build_lint_prompt, COMPILE_SYSTEM_PROMPT, LINT_SYSTEM_PROMPT};
use super::store::WikiStore;

/// Wiki compiler — orchestrates the Ingest/Compile/Lint workflows.
///
/// The compiler is the "brain" of the wiki memory strategy. It uses
/// the LLM to:
/// 1. **Compile**: Analyze conversation turns and produce wiki updates.
/// 2. **Lint**: Periodically check wiki consistency and fix issues.
///
/// Ingest and Compile are merged into a single LLM call for efficiency
/// (the LLM both extracts knowledge and decides how to update the wiki
/// in one pass).
pub struct WikiCompiler;

/// A single wiki update instruction parsed from the LLM's compile output.
#[derive(Debug)]
pub struct CompileAction {
    /// Whether to create a new page or update an existing one.
    pub action: ActionType,
    /// Page ID (slug format).
    pub page_id: String,
    /// Human-readable title.
    pub title: String,
    /// Page type classification.
    pub page_type: PageType,
    /// Tags for the page.
    pub tags: Vec<String>,
    /// Links to other pages.
    pub links: Vec<String>,
    /// Markdown body content.
    pub body: String,
}

/// Action type for a compile instruction.
#[derive(Debug, PartialEq)]
pub enum ActionType {
    Create,
    Update,
    #[allow(dead_code)]
    Skip,
}

impl WikiCompiler {
    /// Run the compile workflow: analyze a conversation turn and update the wiki.
    ///
    /// This is the main entry point called from `WikiMemory::reflect_on_turn()`.
    /// It combines Ingest + Compile into a single LLM call:
    /// 1. Build a page listing of the current wiki state.
    /// 2. Ask the LLM to analyze the conversation and produce update instructions.
    /// 3. Parse the LLM output and apply changes to the store.
    /// 4. Generate embeddings for new/updated pages (if embedding provider is available).
    ///
    /// The `embedder` parameter is optional. When `None`, pages are created/updated
    /// without embedding vectors — retrieval will fall back to keyword matching.
    pub async fn compile(
        store: &mut WikiStore,
        user_input: &str,
        assistant_response: &str,
        llm: &dyn LlmApi,
        embedder: Option<&dyn Embedding>,
    ) -> Result<()> {
        // Build page listing for the LLM
        let page_listing = store.page_listing();

        // Ask the LLM to compile
        let prompt = build_compile_prompt(&page_listing, user_input, assistant_response);
        let messages = vec![
            ChatMessage::system(COMPILE_SYSTEM_PROMPT),
            ChatMessage::user(prompt),
        ];

        let response = llm.chat(&messages, None).await
            .context("Wiki compile LLM call failed")?;

        // Parse the LLM's response into actions
        let actions = Self::parse_compile_response(&response.content);

        if actions.is_empty() {
            tracing::debug!("Wiki compiler: no updates needed for this turn");
            return Ok(());
        }

        // Apply parsed actions to the store
        Self::apply_actions(store, actions, embedder).await
    }

    /// Apply a list of compile actions to the wiki store.
    ///
    /// Handles page creation and updates, including optional embedding
    /// generation. Rebuilds backlinks after all changes are applied.
    async fn apply_actions(
        store: &mut WikiStore,
        actions: Vec<CompileAction>,
        embedder: Option<&dyn Embedding>,
    ) -> Result<()> {
        for action in actions {
            match action.action {
                ActionType::Skip => continue,
                ActionType::Create => {
                    let mut page = WikiPage::new(
                        action.page_id.clone(),
                        action.title,
                        action.page_type,
                        action.tags,
                        action.links,
                        action.body,
                    );

                    // Generate embedding for the new page (if provider available)
                    if let Some(embedder) = embedder {
                        match embedder.embed(&page.to_prompt_text()).await {
                            Ok(emb) => page.embedding = emb,
                            Err(e) => tracing::warn!(
                                page_id = %action.page_id,
                                error = %e,
                                "Failed to generate embedding for new wiki page"
                            ),
                        }
                    }

                    store.add_page(page);
                    tracing::debug!(page_id = %action.page_id, "Wiki: created new page");
                }
                ActionType::Update => {
                    // Generate new embedding for the updated content (if provider available)
                    let embedding = if let Some(embedder) = embedder {
                        match embedder.embed(&action.body).await {
                            Ok(emb) => Some(emb),
                            Err(e) => {
                                tracing::warn!(
                                    page_id = %action.page_id,
                                    error = %e,
                                    "Failed to generate embedding for updated wiki page"
                                );
                                None
                            }
                        }
                    } else {
                        None
                    };

                    store.update_page(
                        &action.page_id,
                        action.body,
                        action.tags,
                        action.links,
                        embedding,
                    );
                    tracing::debug!(page_id = %action.page_id, "Wiki: updated existing page");
                }
            }
        }

        // Rebuild backlinks after all changes
        store.rebuild_backlinks();

        Ok(())
    }

    /// Run the lint workflow: check wiki consistency and log issues.
    ///
    /// Called periodically (every N turns) to detect contradictions,
    /// broken links, duplicates, and stale content.
    pub async fn lint(store: &WikiStore, llm: &dyn LlmApi) -> Result<()> {
        if store.page_count() == 0 {
            return Ok(());
        }

        // Build a text representation of all pages for the LLM
        let pages_text = store.all_pages_prompt_text();

        let prompt = build_lint_prompt(&pages_text);
        let messages = vec![
            ChatMessage::system(LINT_SYSTEM_PROMPT),
            ChatMessage::user(prompt),
        ];

        let response = llm.chat(&messages, None).await
            .context("Wiki lint LLM call failed")?;

        // For now, just log the lint results. Future iterations can
        // automatically apply fixes.
        let content = response.content.trim();
        if content.contains("NO_ISSUES") {
            tracing::info!("Wiki lint: no issues found");
        } else {
            tracing::info!(
                lint_results = %content,
                "Wiki lint: issues detected (logged for review)"
            );
        }

        Ok(())
    }

    /// Parse the LLM's compile response into a list of actions.
    ///
    /// Handles the structured output format defined in the compile prompt.
    /// Gracefully handles malformed output by skipping unparseable blocks.
    fn parse_compile_response(response: &str) -> Vec<CompileAction> {
        let response = response.trim();

        // Check for SKIP
        if response.contains("ACTION: SKIP") && !response.contains("ACTION: CREATE")
            && !response.contains("ACTION: UPDATE")
        {
            return Vec::new();
        }

        let mut actions = Vec::new();

        // Split by ACTION: markers
        let blocks: Vec<&str> = response.split("ACTION:").collect();

        for block in blocks.iter().skip(1) {
            // Skip empty blocks
            let block = block.trim();
            if block.is_empty() || block.starts_with("SKIP") {
                continue;
            }

            match Self::parse_single_action(block) {
                Some(action) => actions.push(action),
                None => {
                    tracing::warn!(
                        block = %block.chars().take(200).collect::<String>(),
                        "Wiki compiler: failed to parse action block, skipping"
                    );
                }
            }
        }

        actions
    }

    /// Parse a single action block from the LLM output.
    ///
    /// The block is a structured text format with header fields (PAGE_ID, TITLE, etc.)
    /// followed by a BODY section. Uses a simple state machine to separate
    /// header parsing from body collection.
    fn parse_single_action(block: &str) -> Option<CompileAction> {
        let lines: Vec<&str> = block.lines().collect();
        if lines.is_empty() {
            return None;
        }

        // First line determines action type
        let action_type = match lines[0].trim() {
            s if s.starts_with("CREATE") => ActionType::Create,
            s if s.starts_with("UPDATE") => ActionType::Update,
            _ => return None,
        };

        let mut page_id = String::new();
        let mut title = String::new();
        let mut page_type = PageType::Topic;
        let mut tags = Vec::new();
        let mut links = Vec::new();
        let mut body = String::new();
        let mut parsing_body = false;

        for line in &lines[1..] {
            let line = *line;

            // Body collection state: accumulate lines until END_BODY
            if parsing_body {
                if line.trim() == "END_BODY" {
                    parsing_body = false;
                    continue;
                }
                if !body.is_empty() {
                    body.push('\n');
                }
                body.push_str(line);
                continue;
            }

            // Header field parsing
            if line.starts_with("BODY:") {
                parsing_body = true;
                let rest = line.trim_start_matches("BODY:").trim();
                if !rest.is_empty() {
                    body.push_str(rest);
                }
            } else if let Some(value) = Self::parse_header_field(line) {
                match value {
                    HeaderField::PageId(v) => page_id = v,
                    HeaderField::Title(v) => title = v,
                    HeaderField::PageType(v) => page_type = v,
                    HeaderField::Tags(v) => tags = v,
                    HeaderField::Links(v) => links = v,
                }
            }
        }

        // If we never hit END_BODY, the body is whatever we collected
        // (handles cases where LLM forgets the END_BODY marker)

        if page_id.is_empty() || title.is_empty() {
            return None;
        }

        Some(CompileAction {
            action: action_type,
            page_id,
            title,
            page_type,
            tags,
            links,
            body,
        })
    }

    /// Parse a single header field line from the LLM output.
    ///
    /// Returns `None` if the line doesn't match any known field prefix.
    /// This reduces the cyclomatic complexity of `parse_single_action`
    /// by isolating field-level parsing into a dedicated method.
    fn parse_header_field(line: &str) -> Option<HeaderField> {
        if line.starts_with("PAGE_ID:") {
            Some(HeaderField::PageId(
                line.trim_start_matches("PAGE_ID:").trim().to_string(),
            ))
        } else if line.starts_with("TITLE:") {
            Some(HeaderField::Title(
                line.trim_start_matches("TITLE:").trim().to_string(),
            ))
        } else if line.starts_with("PAGE_TYPE:") {
            let pt = line.trim_start_matches("PAGE_TYPE:").trim().to_lowercase();
            let page_type = match pt.as_str() {
                "entity" => PageType::Entity,
                "topic" => PageType::Topic,
                "summary" => PageType::Summary,
                _ => PageType::Topic,
            };
            Some(HeaderField::PageType(page_type))
        } else if line.starts_with("TAGS:") {
            let tags = Self::parse_csv_field(line.trim_start_matches("TAGS:"));
            Some(HeaderField::Tags(tags))
        } else if line.starts_with("LINKS:") {
            let links = Self::parse_csv_field(line.trim_start_matches("LINKS:"));
            Some(HeaderField::Links(links))
        } else {
            None
        }
    }

    /// Parse a comma-separated field value into a list of trimmed, non-empty strings.
    fn parse_csv_field(value: &str) -> Vec<String> {
        value
            .trim()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

/// Parsed header field from a compile action block.
///
/// Used internally by `parse_header_field` to return typed field values
/// without exposing the parsing details to `parse_single_action`.
enum HeaderField {
    PageId(String),
    Title(String),
    PageType(PageType),
    Tags(Vec<String>),
    Links(Vec<String>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_compile_response_skip() {
        let response = "ACTION: SKIP";
        let actions = WikiCompiler::parse_compile_response(response);
        assert!(actions.is_empty());
    }

    #[test]
    fn test_parse_compile_response_create() {
        let response = r#"ACTION: CREATE
PAGE_ID: rust-ownership
TITLE: Rust Ownership Model
PAGE_TYPE: topic
TAGS: rust, memory-safety
LINKS: rust-borrowing
BODY:
# Rust Ownership

Rust uses an ownership system for memory management.

## Key Rules
- Each value has one owner
- [[rust-borrowing]] extends ownership
END_BODY"#;

        let actions = WikiCompiler::parse_compile_response(response);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].action, ActionType::Create);
        assert_eq!(actions[0].page_id, "rust-ownership");
        assert_eq!(actions[0].title, "Rust Ownership Model");
        assert_eq!(actions[0].page_type, PageType::Topic);
        assert_eq!(actions[0].tags, vec!["rust", "memory-safety"]);
        assert_eq!(actions[0].links, vec!["rust-borrowing"]);
        assert!(actions[0].body.contains("# Rust Ownership"));
        assert!(actions[0].body.contains("[[rust-borrowing]]"));
    }

    #[test]
    fn test_parse_compile_response_multiple() {
        let response = r#"ACTION: CREATE
PAGE_ID: page-a
TITLE: Page A
PAGE_TYPE: entity
TAGS: test
LINKS:
BODY:
Content A
END_BODY

ACTION: UPDATE
PAGE_ID: page-b
TITLE: Page B
PAGE_TYPE: topic
TAGS: test
LINKS: page-a
BODY:
Updated content B with [[page-a]] link.
END_BODY"#;

        let actions = WikiCompiler::parse_compile_response(response);
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].action, ActionType::Create);
        assert_eq!(actions[0].page_id, "page-a");
        assert_eq!(actions[1].action, ActionType::Update);
        assert_eq!(actions[1].page_id, "page-b");
    }

    #[test]
    fn test_parse_compile_response_no_end_body() {
        // LLM sometimes forgets END_BODY
        let response = r#"ACTION: CREATE
PAGE_ID: test-page
TITLE: Test Page
PAGE_TYPE: entity
TAGS: test
LINKS:
BODY:
Some content without END_BODY marker."#;

        let actions = WikiCompiler::parse_compile_response(response);
        assert_eq!(actions.len(), 1);
        assert!(actions[0].body.contains("Some content"));
    }
}
