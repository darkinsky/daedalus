// ── LLM Prompt Templates for A-MEM ──
//
// Prompt templates are separated from business logic for maintainability.
// To adjust wording, support multiple languages, or A/B test prompts,
// modify only this file — no changes to the core engine needed.
//
// Reference: A-MEM (arxiv:2502.12110, NeurIPS 2025)

// ═══════════════════════════════════════════════════════════════════
// ── Phase 1: Note Construction (metadata extraction) ──
// ═══════════════════════════════════════════════════════════════════

pub(super) const METADATA_SYSTEM_PROMPT: &str =
    "You are a memory indexing assistant. Extract structured metadata from content. \
     Be concise and precise. Keywords should capture key concepts. \
     Tags should be categorical labels. Context should summarize significance.";

pub(super) fn metadata_extraction_prompt(content: &str) -> String {
    format!(
        r#"Analyze the following content and extract structured metadata.

Content: {}

Respond in EXACTLY this format (no extra text):
KEYWORDS: keyword1, keyword2, keyword3
TAGS: tag1, tag2, tag3
CATEGORY: one of [user_preference, project_context, technical_decision, code_pattern, error_pattern, domain_knowledge, relationship, uncategorized]
CONTEXT: A one-sentence description of the significance and context of this information."#,
        content
    )
}

// ═══════════════════════════════════════════════════════════════════
// ── Phase 2+3: Unified process_memory (link + evolve in one call) ──
// ═══════════════════════════════════════════════════════════════════

/// System prompt for the unified process_memory call.
///
/// This replaces the previous 2 separate LLM calls (link_validation +
/// evolution) with a single call that decides:
/// 1. Which candidates should be linked
/// 2. Whether evolution should happen
/// 3. How to update neighbor metadata
///
/// Aligned with the paper's `process_memory()` design.
pub(super) const PROCESS_MEMORY_SYSTEM_PROMPT: &str = "\
You are an agentic memory manager implementing the Zettelkasten method. \
Your job is to analyze a new memory note, find meaningful connections to \
existing notes, and decide whether the knowledge network should evolve.

You must make THREE decisions:
1. LINKING: Which candidate notes should be connected to the new note?
2. SHOULD_EVOLVE: Should existing linked notes be updated with new insights?
3. EVOLUTION: If evolving, what metadata updates should be applied to neighbors?

Only link notes with genuine semantic relationships — not surface-level keyword overlap. \
Only evolve when the new note provides genuinely new context that enriches existing notes.";

/// Build the unified process_memory user prompt.
///
/// Combines linking + evolution into a single LLM call (paper-faithful).
pub(super) fn process_memory_prompt(
    new_note_text: &str,
    candidates_text: &str,
) -> String {
    format!(
        r#"A new memory note has been added. Analyze it against candidate related notes and decide how to update the knowledge network.

## New Note
{new_note_text}

## Candidate Related Notes
{candidates_text}

## Instructions

Respond in EXACTLY this format:

LINK: <comma-separated candidate numbers to link, or NONE>
SHOULD_EVOLVE: <true or false>
EVOLVE: <for each linked candidate that should evolve, one line per candidate>
  CANDIDATE_<number>_TAGS: tag1, tag2, tag3
  CANDIDATE_<number>_CONTEXT: Updated context reflecting the new connection

If SHOULD_EVOLVE is false, omit the EVOLVE section entirely.
If LINK is NONE, also set SHOULD_EVOLVE to false.

Example:
LINK: 1, 3
SHOULD_EVOLVE: true
CANDIDATE_1_TAGS: rust, memory_safety, ownership
CANDIDATE_1_CONTEXT: Rust ownership system discussed in context of memory management patterns
CANDIDATE_3_TAGS: systems_programming, performance
CANDIDATE_3_CONTEXT: Systems programming approaches with focus on zero-cost abstractions"#
    )
}

// ═══════════════════════════════════════════════════════════════════
// ── Legacy prompts (kept for backward compatibility / fallback) ──
// ═══════════════════════════════════════════════════════════════════

#[allow(dead_code)]
pub(super) const LINK_VALIDATION_SYSTEM_PROMPT: &str =
    "You are a memory linking assistant. Analyze semantic relationships between \
     memory notes and determine which should be connected. Only link notes that \
     share meaningful semantic relationships, not just surface-level keyword overlap.";

#[allow(dead_code)]
pub(super) const EVOLUTION_SYSTEM_PROMPT: &str =
    "You are a memory evolution assistant. Update memory note metadata to reflect \
     new connections and higher-order knowledge patterns. Preserve the original \
     meaning while enriching with insights from newly linked notes.";

#[allow(dead_code)]
pub(super) fn link_validation_prompt(note_text: &str, candidates_text: &str) -> String {
    format!(
        r#"Given a new memory note and candidate related notes, determine which candidates are truly semantically related and should be linked.

NEW NOTE:
{}

CANDIDATE NOTES:
{}

Respond with ONLY the candidate numbers that should be linked, separated by commas.
If none should be linked, respond with "NONE".
Example: 1, 3, 5"#,
        note_text, candidates_text
    )
}

#[allow(dead_code)]
pub(super) fn evolution_prompt(existing_note_text: &str, new_note_text: &str) -> String {
    format!(
        r#"An existing memory note has a new related note linked to it. Re-analyze the existing note in light of this new connection and produce updated metadata.

EXISTING NOTE:
{}

NEWLY LINKED NOTE:
{}

Produce updated metadata for the EXISTING note that reflects any new insights from the connection. Keep the original meaning but enrich with new patterns.

Respond in EXACTLY this format:
KEYWORDS: keyword1, keyword2, keyword3
TAGS: tag1, tag2, tag3
CONTEXT: Updated one-sentence description reflecting the new connection."#,
        existing_note_text, new_note_text
    )
}

// ═══════════════════════════════════════════════════════════════════
// ── Retrieval context injection ──
// ═══════════════════════════════════════════════════════════════════

/// Preamble injected before the memory context in the system prompt.
///
/// Guides the main LLM to actively use retrieved memories.
pub(super) const MEMORY_INJECTION_PREAMBLE: &str = "\
<agentic_memory>
The following memories have been retrieved from your knowledge network based on \
relevance to the current conversation. Use this information to:
1. Maintain continuity with past interactions and decisions
2. Apply previously learned preferences, patterns, and context
3. Avoid contradicting established facts or repeating past mistakes
Do not mention this memory system to the user unless asked.";

/// Closing tag for the memory injection.
pub(super) const MEMORY_INJECTION_EPILOGUE: &str = "</agentic_memory>";
