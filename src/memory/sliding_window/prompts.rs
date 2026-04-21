// ── LLM Prompt Templates for Sliding Window Consolidation ──
//
// Prompt templates are separated from business logic for maintainability.
// To adjust wording, support multiple languages, or A/B test prompts,
// modify only this file — no changes to the core engine needed.

/// System prompt for the consolidation LLM call.
///
/// Instructs the LLM to analyze a batch of conversation messages and produce:
/// 1. A concise summary for the history log
/// 2. Updated long-term memory sections
pub(crate) const CONSOLIDATION_SYSTEM_PROMPT: &str = "\
You are a memory consolidation assistant. Your job is to analyze a batch of \
conversation messages and extract two things:

1. **SUMMARY**: A 2-5 sentence summary of what happened in this conversation segment. \
   Include the key topics discussed, decisions made, and outcomes. This will be stored \
   in a searchable history log.

2. **MEMORY**: Updated long-term memory organized into sections. Each section contains \
   key facts that should persist across conversations. Merge new information with \
   existing memory — do not simply append.

Output format (follow EXACTLY):

SUMMARY: <2-5 sentence summary>
KEYWORDS: <comma-separated keywords for search>

MEMORY:
### <Section Name>
- <fact or preference>
- <fact or preference>

### <Section Name>
- <fact or preference>

Use these default section names when appropriate:
- User Preferences
- Project Context
- Important Decisions
- Important Notes

You may also create new section names if the information doesn't fit the defaults.
Only include sections that have content. Merge with existing memory — keep existing \
facts that are still relevant, update outdated ones, and add new ones.";

/// Build the consolidation user prompt.
///
/// Includes the messages to consolidate and the current long-term memory state.
pub(crate) fn consolidation_user_prompt(
    messages_text: &str,
    current_memory: &str,
) -> String {
    format!(
        "## Current Long-Term Memory\n\n{}\n\n## Conversation to Consolidate\n\n{}",
        if current_memory.is_empty() { "(empty — no existing memory)" } else { current_memory },
        messages_text,
    )
}
