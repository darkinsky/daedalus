// ── LLM Prompt Templates for Dynamic Cheatsheet ──
//
// Prompt templates are separated from business logic for maintainability.
// To adjust wording, support multiple languages, or A/B test prompts,
// modify only this file — no changes to the core engine needed.
//
// Reference: Dynamic Cheatsheet (arxiv:2504.07952)
// Reference: github.com/suzgunmirac/dynamic-cheatsheet

// ═══════════════════════════════════════════════════════════════════
// ── Full-Rewrite Curator (paper-faithful) ──
// ═══════════════════════════════════════════════════════════════════

/// System prompt for the full-rewrite Curator.
///
/// Aligned with the DC paper's 6-section Curator prompt:
/// 1. Purpose & Goals
/// 2. Core Responsibilities (Curate, Accuracy, Refine, Practicality)
/// 3. Principles (Accuracy, Iterative Refinement, Clarity, Reusability)
/// 4. Cheatsheet Structure
/// 5. Formatting Guidelines
/// 6. Anti-catastrophic-forgetting warnings
pub(crate) const CURATOR_FULL_REWRITE_SYSTEM_PROMPT: &str = "\
You are a Cheatsheet Curator — an expert at maintaining and refining a living \
reference document that consolidates verified solutions, reusable strategies, \
and critical insights across diverse tasks.

## Core Responsibilities

1. **Curate & Preserve**: Select only the most actionable content from new \
   interactions. ALWAYS preserve existing useful content — never silently drop entries.
2. **Verify Accuracy**: Before incorporating any insight from the model's response, \
   assess whether the response is correct. Do NOT add strategies from incorrect \
   or flawed answers.
3. **Refine & Update**: Remove redundancies by merging similar entries. Update \
   outdated information. Improve clarity of existing entries.
4. **Ensure Practicality**: Include code snippets, worked examples, and concrete \
   guidelines — not just abstract principles.

## Principles

- **Accuracy first**: Only incorporate proven, verified solutions. State \
  assumptions and limitations. If the model's answer appears incorrect, \
  note the error pattern instead of the flawed strategy.
- **Iterative refinement**: Synthesize old and new knowledge — don't just \
  overwrite. Merge complementary insights. Document edge cases and optimizations.
- **Clarity**: Keep entries concise but complete. Each entry should be \
  immediately actionable without needing the original conversation context.
- **Reusability**: Focus on generalizable patterns, non-obvious details, \
  and transferable techniques. Include code templates where applicable.

## Cheatsheet Structure

Organize entries into these sections (include only sections that have content):

1. **Solutions & Code Patterns** — Annotated, reusable templates and implementations
2. **Edge Cases & Pitfalls** — Common failure modes, validation traps, and mitigations
3. **Meta-Reasoning Strategies** — High-level problem-solving heuristics and frameworks
4. **Domain Knowledge** — Key facts, conventions, and reference information

Each entry should include:
- The insight or strategy (can be multi-line, including code blocks)
- Usage count: ** Count: N (where N = how many times this has been applied)

## CRITICAL WARNING

You MUST output the COMPLETE updated cheatsheet. Once you output the new version, \
the previous cheatsheet is REPLACED entirely. Any content from the previous \
cheatsheet that you do not explicitly include in your output WILL BE LOST FOREVER. \
Make sure to copy all relevant existing entries!

Keep the cheatsheet under ~2000 tokens. When approaching the limit, compress \
less important entries and merge redundant ones, but never silently drop high-count entries.";

/// Build the full-rewrite Curator user prompt.
///
/// The Curator receives the previous cheatsheet, the current interaction,
/// and must output a COMPLETE updated cheatsheet.
pub(crate) fn curator_full_rewrite_prompt(
    user_query: &str,
    assistant_response: &str,
    current_cheatsheet: &str,
) -> String {
    format!(
        r#"## Previous Cheatsheet

{current_cheatsheet}

## Latest Interaction

**User**: {user_query}

**Assistant**: {assistant_response}

## Task

1. Assess whether the assistant's response is correct and useful.
2. Extract any new reusable insights (strategies, code patterns, edge cases, domain knowledge).
3. Merge new insights with the existing cheatsheet.
4. Remove redundancies and compress where needed.
5. Output the COMPLETE updated cheatsheet.

If the assistant's response contains errors, record the error pattern as a pitfall instead.
If no new insights are worth recording, output the previous cheatsheet unchanged.

IMPORTANT: Output ONLY the updated cheatsheet content. Do not include any preamble, \
explanation, or commentary outside the cheatsheet itself.

Begin the updated cheatsheet now:"#
    )
}

// ═══════════════════════════════════════════════════════════════════
// ── Incremental Curator (lightweight mode) ──
// ═══════════════════════════════════════════════════════════════════

/// System prompt for the incremental Curator.
pub(crate) const CURATOR_INCREMENTAL_SYSTEM_PROMPT: &str = "\
You are a Cheatsheet Curator that extracts reusable insights from interactions. \
Analyze the conversation and identify strategies, patterns, error fixes, \
code snippets, and worked examples that would be valuable for future similar tasks.

## Critical Rules

1. **Verify correctness**: Before extracting an insight, assess whether the \
   assistant's response is actually correct. Do NOT record strategies from \
   flawed or incorrect answers — instead, record the error pattern as a pitfall.
2. **Be thorough**: Include code snippets, worked examples, and edge cases — \
   not just one-line summaries. Multi-line content (including code blocks) is encouraged.
3. **Avoid duplicates**: Check the existing cheatsheet carefully. Only extract \
   genuinely NEW insights not already captured.
4. **Reinforce existing entries**: If an existing entry was validated by this \
   interaction, use REINFORCE to increment its usage count.";

/// Build the incremental Curator user prompt.
///
/// The LLM analyzes the latest interaction and outputs structured directives:
/// - `NEW: <category> | <content>` — add a new entry
/// - `UPDATE: <number> | <refined_content>` — refine an existing entry
/// - `REINFORCE: <number>` — increment an existing entry's usage count
/// - `NO_NEW_INSIGHTS` — nothing worth recording
pub(crate) fn curator_incremental_prompt(
    user_query: &str,
    assistant_response: &str,
    current_cheatsheet: &str,
) -> String {
    format!(
        r#"Analyze this interaction and update the cheatsheet.

## Current Cheatsheet
{current_cheatsheet}

## Latest Interaction
**User**: {user_query}
**Assistant**: {assistant_response}

## Instructions

First, assess whether the assistant's response is correct and useful.

Then output directives (one per line):
- NEW: <category> | <content> — for genuinely new insights (category is free-form, e.g., strategy, code_pattern, edge_case, meta_reasoning, domain_knowledge)
- UPDATE: <number> | <refined_content> — to refine/correct an existing entry (1-based number)
- REINFORCE: <number> — to mark an existing entry as validated by this interaction
- NO_NEW_INSIGHTS — if nothing new is worth recording

Content can be multi-line for code blocks or detailed explanations. Use a blank line \
before the next directive to separate multi-line content.

If the assistant's response contains errors, add a NEW entry under "edge_case" or \
"pitfall" category describing the error pattern, NOT the flawed strategy."#
    )
}

// ═══════════════════════════════════════════════════════════════════
// ── Generator guidance (injected with the cheatsheet) ──
// ═══════════════════════════════════════════════════════════════════

/// Preamble injected before the cheatsheet in the system prompt.
///
/// Guides the Generator (main LLM) to actively consult the cheatsheet,
/// matching the paper's generator prompt design.
pub(crate) const CHEATSHEET_INJECTION_PREAMBLE: &str = "\
<cheatsheet_reference>
The following is your Dynamic Cheatsheet — a curated collection of verified strategies, \
code patterns, edge cases, and insights accumulated from previous interactions. \
Before responding to the user's request:
1. Review the cheatsheet for applicable strategies and patterns.
2. Identify relevant entries and adapt them to the current problem.
3. Note any limitations or caveats mentioned in the cheatsheet.
If the cheatsheet contains relevant code templates or solutions, use them as a starting point.";

/// Closing tag for the cheatsheet injection.
pub(crate) const CHEATSHEET_INJECTION_EPILOGUE: &str = "</cheatsheet_reference>";

// ═══════════════════════════════════════════════════════════════════
// ── Legacy aliases (backward compatibility) ──
// ═══════════════════════════════════════════════════════════════════

/// Legacy alias — maps to the incremental system prompt for backward compatibility.
#[allow(dead_code)]
pub(crate) const REFLECTION_SYSTEM_PROMPT: &str = CURATOR_INCREMENTAL_SYSTEM_PROMPT;

/// Legacy alias — maps to the incremental user prompt builder.
#[allow(dead_code)]
pub(crate) fn reflection_user_prompt(
    user_query: &str,
    assistant_response: &str,
    current_cheatsheet: &str,
) -> String {
    curator_incremental_prompt(user_query, assistant_response, current_cheatsheet)
}
