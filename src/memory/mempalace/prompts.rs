// ── LLM Prompt Templates for MemPalace ──
//
// Prompt templates are separated from business logic for maintainability.
// Enhanced to match the original MemPalace MCP server prompts.

use super::dialect::{AAAK_SPEC, PALACE_PROTOCOL};

/// System prompt for the classifier that routes conversations into
/// the palace spatial structure (Wing/Room/HallType) and extracts
/// knowledge graph triples.
pub(super) const CLASSIFIER_SYSTEM_PROMPT: &str =
    "You are a memory classification assistant for a Memory Palace system. \
     Your job is to analyze a conversation turn and classify it into a spatial \
     structure: Wing (project or person), Room (specific topic), and Hall type \
     (category of memory). You also extract knowledge graph triples (subject-predicate-object) \
     with optional temporal validity. \
     Be precise and consistent with naming — reuse existing wing/room names when possible.";

/// Build the classifier prompt for a single conversation turn.
///
/// Enhanced to extract temporal validity for KG triples and entity types.
pub(super) fn classifier_prompt(
    user_input: &str,
    assistant_response: &str,
    existing_wings: &[String],
    existing_rooms: &[String],
) -> String {
    let wings_list = if existing_wings.is_empty() {
        "(none yet)".to_string()
    } else {
        existing_wings.join(", ")
    };
    let rooms_list = if existing_rooms.is_empty() {
        "(none yet)".to_string()
    } else {
        existing_rooms.join(", ")
    };

    format!(
        r#"Analyze this conversation turn and classify it into the Memory Palace structure.

USER INPUT:
{user_input}

ASSISTANT RESPONSE:
{assistant_response}

EXISTING WINGS: {wings_list}
EXISTING ROOMS: {rooms_list}

Respond in EXACTLY this format (no extra text):
WING_ID: <slug identifier for the project or person, e.g., "project-daedalus" or "person-alice">
WING_LABEL: <human-readable name, e.g., "Daedalus Project" or "Alice">
ROOM_ID: <slug identifier for the specific topic, e.g., "auth-migration" or "memory-system">
ROOM_LABEL: <human-readable name, e.g., "Auth Migration" or "Memory System Design">
HALL_TYPE: <one of: facts, events, discoveries, preferences, advice>
MEMORY: <a concise one-sentence summary of the key information from this turn>
TRIPLES: <comma-separated SPO triples in "subject|predicate|object" format, or "NONE". Include temporal info as "subject|predicate|object|valid_from" where valid_from is YYYY-MM-DD>
ENTITIES: <comma-separated entities in "name:type" format where type is person/project/tool/concept, or "NONE">

Example TRIPLES: "Daedalus|uses|Rust, MemPalace|stores_in|ChromaDB, Max|started_school|Year 7|2026-09-01"
Example ENTITIES: "Daedalus:project, Alice:person, ChromaDB:tool""#
    )
}

/// System prompt for closet (summary) generation.
pub(super) const CLOSET_SYSTEM_PROMPT: &str =
    "You are a memory summarization assistant. Compress multiple conversation \
     turns into a concise summary that preserves all key facts, decisions, and \
     insights. The summary should be self-contained and useful for future retrieval. \
     Extract key topics, proper nouns, and action verbs for indexing.";

/// Build the closet summarization prompt.
pub(super) fn closet_prompt(drawer_texts: &[String]) -> String {
    let entries = drawer_texts
        .iter()
        .enumerate()
        .map(|(i, text)| format!("--- Turn {} ---\n{}", i + 1, text))
        .collect::<Vec<_>>()
        .join("\n\n");

    format!(
        r#"Summarize the following conversation turns into a concise, self-contained summary.
Preserve all key facts, decisions, preferences, and insights.
Include proper nouns (names, projects, tools) and key action verbs.

{entries}

Respond with ONLY the summary text (no headers or formatting)."#
    )
}

/// System prompt for entity extraction.
#[allow(dead_code)]
pub(super) const ENTITY_EXTRACTION_PROMPT: &str =
    "You are an entity extraction assistant. Extract all named entities \
     (people, projects, tools, concepts) from the given text. For each entity, \
     determine its type and any relationships to other entities.";

/// Build the entity extraction prompt.
#[allow(dead_code)]
pub(super) fn entity_extraction_prompt(text: &str) -> String {
    format!(
        r#"Extract all named entities from this text:

{text}

Respond in this format (one per line):
ENTITY: <name> | TYPE: <person/project/tool/concept> | RELATIONSHIPS: <subject|predicate|object, ...> or NONE

Example:
ENTITY: Alice | TYPE: person | RELATIONSHIPS: Alice|works_on|Daedalus, Alice|uses|Rust
ENTITY: ChromaDB | TYPE: tool | RELATIONSHIPS: Daedalus|stores_in|ChromaDB"#
    )
}

/// System prompt for diary entry generation.
#[allow(dead_code)]
pub(super) const DIARY_SYSTEM_PROMPT: &str =
    "You are an AI agent writing a personal diary entry. Record what happened \
     in this session, what you learned, what matters, and any observations. \
     Write in first person. Be concise but capture the essence.";

/// Build the diary entry prompt.
#[allow(dead_code)]
pub(super) fn diary_prompt(
    session_summary: &str,
    agent_name: &str,
) -> String {
    format!(
        r#"Write a brief diary entry for agent "{agent_name}" based on this session:

{session_summary}

Write in first person. Include:
- What was worked on
- Key decisions or discoveries
- What matters for future sessions
- Any observations about the user's preferences

Keep it under 200 words."#
    )
}

/// Get the AAAK dialect specification.
#[allow(dead_code)]
pub(super) fn aaak_spec() -> &'static str {
    AAAK_SPEC
}

/// Get the Palace Protocol specification.
#[allow(dead_code)]
pub(super) fn palace_protocol() -> &'static str {
    PALACE_PROTOCOL
}

/// Build the wake-up prompt that includes L0 Identity + L1 Essential Story.
#[allow(dead_code)]
pub(super) fn wake_up_prompt(identity: &str, essential_story: &str) -> String {
    format!(
        r#"{identity}

{essential_story}

---
{PALACE_PROTOCOL}"#,
        PALACE_PROTOCOL = PALACE_PROTOCOL
    )
}
