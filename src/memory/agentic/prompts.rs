// ── LLM Prompt Templates for A-MEM ──
//
// Prompt templates are separated from business logic for maintainability.
// To adjust wording, support multiple languages, or A/B test prompts,
// modify only this file — no changes to the core engine needed.

pub(super) const METADATA_SYSTEM_PROMPT: &str =
    "You are a memory indexing assistant. Extract structured metadata from content. \
     Be concise and precise. Keywords should capture key concepts. \
     Tags should be categorical labels. Context should summarize significance.";

pub(super) const LINK_VALIDATION_SYSTEM_PROMPT: &str =
    "You are a memory linking assistant. Analyze semantic relationships between \
     memory notes and determine which should be connected. Only link notes that \
     share meaningful semantic relationships, not just surface-level keyword overlap.";

pub(super) const EVOLUTION_SYSTEM_PROMPT: &str =
    "You are a memory evolution assistant. Update memory note metadata to reflect \
     new connections and higher-order knowledge patterns. Preserve the original \
     meaning while enriching with insights from newly linked notes.";

pub(super) fn metadata_extraction_prompt(content: &str) -> String {
    format!(
        r#"Analyze the following content and extract structured metadata.

Content: {}

Respond in EXACTLY this format (no extra text):
KEYWORDS: keyword1, keyword2, keyword3
TAGS: tag1, tag2, tag3
CONTEXT: A one-sentence description of the significance and context of this information."#,
        content
    )
}

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
