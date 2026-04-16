// ── LLM Prompt Templates for Dynamic Cheatsheet ──
//
// Prompt templates are separated from business logic for maintainability.
// To adjust wording, support multiple languages, or A/B test prompts,
// modify only this file — no changes to the core engine needed.
//
// Reference: Dynamic Cheatsheet (arxiv:2504.07952)

pub(crate) const REFLECTION_SYSTEM_PROMPT: &str =
    "You are a learning assistant that extracts reusable insights from interactions. \
     Analyze the conversation and identify strategies, patterns, error fixes, \
     and code snippets that would be valuable for future similar tasks. \
     Be concise and actionable. Only extract genuinely useful insights.";

/// Build the reflection user prompt.
///
/// The LLM analyzes the latest interaction and extracts new insights
/// or updates to existing cheatsheet entries.
pub(crate) fn reflection_user_prompt(
    user_query: &str,
    assistant_response: &str,
    current_cheatsheet: &str,
) -> String {
    format!(
        r#"Analyze this interaction and extract reusable insights for a cheatsheet.

## Current Cheatsheet
{current_cheatsheet}

## Latest Interaction
**User**: {user_query}
**Assistant**: {assistant_response}

## Instructions
Extract NEW insights not already captured in the cheatsheet. For each insight, provide:
- CATEGORY: one of [strategy, error_pattern, code_snippet, best_practice, domain_knowledge]
- CONTENT: a concise, actionable description (1-2 sentences max)

If an existing cheatsheet entry should be UPDATED (refined or corrected), indicate:
- UPDATE: <entry_number>
- CONTENT: the refined content

If no new insights are worth recording, respond with exactly "NO_NEW_INSIGHTS".

Format each entry on its own line:
NEW: <category> | <content>
UPDATE: <number> | <refined_content>"#
    )
}
