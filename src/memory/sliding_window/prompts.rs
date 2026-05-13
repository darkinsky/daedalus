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

// ── Context Compression (Compact) Prompts ──

/// System prompt for the compact LLM call.
///
/// Instructs the LLM to compress a conversation into a concise summary
/// that preserves all information needed for the agent to continue working.
/// This is different from consolidation — compact is about reducing the
/// *active* context window, not extracting long-term facts.
pub(crate) const COMPACT_SYSTEM_PROMPT: &str = "\
You are a conversation compressor. Your job is to create a concise summary of a \
conversation between a user and an AI assistant. This summary will REPLACE the \
original messages in the AI's context window, so it must preserve all information \
the AI needs to continue the conversation seamlessly.

CRITICAL REQUIREMENTS:
1. Preserve ALL technical details: file paths, function names, variable names, \
   error messages, code snippets, and configuration values.
2. Preserve the current state of any ongoing task: what has been done, what \
   remains, and any blockers.
3. Preserve user preferences and constraints mentioned in the conversation.
4. Preserve any decisions made and their rationale.
5. Do NOT include pleasantries, acknowledgments, or filler text.
6. Use a structured format with clear sections.
7. Be as concise as possible while retaining all actionable information.

CONTEXT ROT PREVENTION — Aggressively discard stale information:
8. DISCARD information about issues that have been fully resolved. Only mention \
   them if the resolution is relevant to current work.
9. DISCARD old tool call details (file reads, searches) that produced no useful \
   results or whose results have been superseded by newer information.
10. COMPRESS repeated exploration patterns into one-line summaries \
    (e.g., 'Searched 15 files for X, found relevant code in A.rs and B.rs').
11. DISCARD intermediate debugging steps that led to dead ends. Only keep the \
    final working solution.
12. When in doubt, prefer DISCARDING over KEEPING. The goal is maximum \
    information density, not completeness.

PRIORITY FRAMEWORK for what to keep vs discard:
  ALWAYS KEEP: Current task goal, unresolved problems, recent code changes + rationale, \
    user preferences, active file paths, pending decisions.
  COMPRESS: Successful tool calls (keep tool name + key result, drop raw output), \
    completed sub-tasks (one-line summary each).
  ALWAYS DISCARD: Failed tool calls with no useful info, superseded file contents, \
    resolved error messages, exploratory reads that found nothing relevant, \
    any content that duplicates information already captured elsewhere in the summary.

Output format:

<compact_summary>
## Conversation Summary

### Task Context
<What the user is working on and their goal>

### Completed Actions
<What has been done so far, with specific details>

### Current State
<Where things stand right now>

### Key Details
<Important technical details, file paths, decisions, preferences>

### Pending Items
<What still needs to be done, if anything>
</compact_summary>";

/// Build the compact user prompt.
///
/// Includes the messages to compress, an optional custom focus instruction,
/// and an optional previous compact summary for incremental compression.
///
/// When `previous_summary` is provided, the LLM is instructed to merge
/// the old summary with the new messages into a single cohesive summary.
/// This enables O(ΔN) incremental compression instead of O(N) full compression.
pub(crate) fn compact_user_prompt(
    messages_text: &str,
    custom_instruction: Option<&str>,
    previous_summary: Option<&str>,
) -> String {
    let mut prompt = String::new();
    if let Some(instruction) = custom_instruction {
        prompt.push_str(&format!(
            "## Additional Focus\n\n{}\n\n",
            instruction,
        ));
    }
    if let Some(summary) = previous_summary {
        prompt.push_str(&format!(
            "## Previous Compact Summary\n\n\
             The following is a summary from a previous compression. \
             Merge it with the new messages below into a single cohesive summary. \
             Preserve all important details from both the old summary and new messages.\n\n\
             {}\n\n",
            summary,
        ));
    }
    prompt.push_str(&format!(
        "## Conversation to Compress\n\n{}",
        messages_text,
    ));
    prompt
}
