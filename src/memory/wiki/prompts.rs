// ── LLM Prompt Templates for Wiki Memory ──
//
// Prompt templates are separated from business logic for maintainability.
// To adjust wording, support multiple languages, or A/B test prompts,
// modify only this file — no changes to the core engine needed.

// ── Ingest + Compile (combined into a single LLM call for efficiency) ──

pub(super) const COMPILE_SYSTEM_PROMPT: &str = "\
You are a knowledge wiki compiler. Your job is to analyze a conversation turn \
(user question + assistant answer) and decide how to update a structured knowledge wiki.

You must output a structured response in the EXACT format specified. \
Do NOT output any other text outside the format.";

/// Build the compile prompt that asks the LLM to analyze a conversation turn
/// and produce wiki update instructions.
///
/// The LLM receives:
/// 1. The current wiki page listing (titles + IDs)
/// 2. The conversation turn (user input + assistant response)
///
/// And must output structured instructions for page creation/updates.
pub(super) fn build_compile_prompt(
    page_listing: &str,
    user_input: &str,
    assistant_response: &str,
) -> String {
    format!(
        r#"Analyze the following conversation turn and decide how to update the knowledge wiki.

## Current Wiki Pages
{page_listing}

## Conversation Turn
USER: {user_input}

ASSISTANT: {assistant_response}

## Instructions
Decide what wiki updates are needed. For each update, output one block in this format:

ACTION: CREATE | UPDATE
PAGE_ID: slug-format-id (lowercase, hyphens, no spaces)
TITLE: Human Readable Title
PAGE_TYPE: entity | topic | summary
TAGS: tag1, tag2, tag3
LINKS: page-id-1, page-id-2
BODY:
The markdown body content for this page.
Use [[page-id]] wikilink syntax to reference other pages.
END_BODY

If no meaningful knowledge worth persisting was exchanged, output:
ACTION: SKIP

Rules:
- Only create/update pages for substantive knowledge (facts, preferences, decisions, concepts).
- Do NOT create pages for casual greetings, small talk, or trivial exchanges.
- Page IDs must be slug format (lowercase, hyphens only, e.g., "rust-ownership").
- When updating an existing page, include the COMPLETE updated body (not just the diff).
- Keep pages focused and concise (aim for 100-500 words per page).
- Use [[page-id]] wikilinks in the body to reference related pages.
- Tags should be lowercase, single words or hyphenated."#
    )
}

// ── Lint ──

pub(super) const LINT_SYSTEM_PROMPT: &str = "\
You are a knowledge wiki quality checker. Analyze wiki pages for consistency issues \
and suggest fixes. Be precise and actionable.";

/// Build the lint prompt that asks the LLM to check wiki consistency.
pub(super) fn build_lint_prompt(pages_text: &str) -> String {
    format!(
        r#"Review the following wiki pages for quality issues:

{pages_text}

Check for:
1. **Contradictions**: Pages that make conflicting claims.
2. **Broken links**: [[wikilinks]] that reference non-existent pages.
3. **Duplicates**: Pages that cover the same topic and should be merged.
4. **Stale content**: Information that appears outdated or superseded.

For each issue found, output one block:

ISSUE_TYPE: CONTRADICTION | BROKEN_LINK | DUPLICATE | STALE
AFFECTED_PAGES: page-id-1, page-id-2
DESCRIPTION: Brief description of the issue.
SUGGESTED_FIX: What should be done to resolve it.

If no issues are found, output:
NO_ISSUES"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compile_prompt_contains_inputs() {
        let result = build_compile_prompt(
            "- rust-ownership: Rust Ownership",
            "How does Rust handle memory?",
            "Rust uses an ownership system...",
        );
        assert!(result.contains("rust-ownership"));
        assert!(result.contains("How does Rust handle memory?"));
        assert!(result.contains("Rust uses an ownership system"));
    }

    #[test]
    fn test_lint_prompt_contains_pages() {
        let result = build_lint_prompt("[Page: test]\nContent here.");
        assert!(result.contains("[Page: test]"));
        assert!(result.contains("Contradictions"));
        assert!(result.contains("Broken links"));
    }
}
