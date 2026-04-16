// ── LLM Prompt Templates for ACE (Agentic Context Engineering) ──
//
// Prompt templates are separated from business logic for maintainability.
// To adjust wording, support multiple languages, or A/B test prompts,
// modify only this file — no changes to the core engine needed.
//
// Reference: ACE (arxiv:2510.04618), kayba-ai/agentic-context-engine

/// System prompt for the Reflector.
///
/// Instructs the LLM to analyze a conversation turn and produce
/// structured delta entries for the Curator to apply.
pub(crate) const REFLECT_SYSTEM_PROMPT: &str = "\
You are an expert at analyzing conversations and extracting reusable insights. \
Your job is to review a conversation turn and produce INCREMENTAL updates to a \
structured playbook of strategies and knowledge.

CRITICAL RULES:
1. NEVER rewrite the entire playbook. Only output small delta changes.
2. Group insights into meaningful sections (e.g., \"Error Handling\", \"Performance\", \"Best Practices\").
3. Each bullet should be concise, actionable, and self-contained (1-2 sentences max).
4. If an existing bullet already covers the insight, use REINFORCE instead of adding a duplicate.
5. If an existing bullet needs refinement, use UPDATE with improved wording.
6. If an existing bullet is wrong or outdated, use REMOVE.
7. Only extract genuinely useful, reusable insights — skip trivial or one-off observations.

Output format (one instruction per line):
ADD: <section_title> | <insight_content>
UPDATE: <section_title> | <bullet_number> | <refined_content>
REINFORCE: <section_title> | <bullet_number>
REMOVE: <section_title> | <bullet_number>
NO_CHANGES";

/// Build the reflection user prompt.
///
/// The LLM analyzes the latest interaction and produces delta entries
/// for the Curator to merge into the playbook.
pub(crate) fn build_reflect_prompt(
    user_input: &str,
    assistant_response: &str,
    current_playbook: &str,
) -> String {
    format!(
        r#"Analyze this interaction and produce incremental updates to the playbook.

## Current Playbook
{current_playbook}

## Latest Interaction
**User**: {user_input}
**Assistant**: {assistant_response}

## Instructions
- If the interaction reveals NEW strategies, patterns, or knowledge not in the playbook, use ADD.
- If an existing bullet should be refined or corrected, use UPDATE with the bullet number.
- If an existing bullet is validated by this interaction, use REINFORCE with the bullet number.
- If an existing bullet is contradicted or outdated, use REMOVE with the bullet number.
- If no updates are needed, respond with exactly "NO_CHANGES".

Output one instruction per line:
ADD: <section_title> | <insight_content>
UPDATE: <section_title> | <bullet_number> | <refined_content>
REINFORCE: <section_title> | <bullet_number>
REMOVE: <section_title> | <bullet_number>"#
    )
}
