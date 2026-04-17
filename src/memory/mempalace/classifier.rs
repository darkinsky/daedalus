use anyhow::{Context, Result};

use crate::llm::LlmApi;

use super::palace::{HallType, Triple};
use super::prompts::{CLASSIFIER_SYSTEM_PROMPT, classifier_prompt};

/// Classification result from the LLM classifier.
#[derive(Debug, Clone)]
pub struct ClassificationResult {
    /// Wing identifier (project or person slug).
    pub wing_id: String,
    /// Wing human-readable label.
    pub wing_label: String,
    /// Room identifier (topic slug).
    pub room_id: String,
    /// Room human-readable label.
    pub room_label: String,
    /// Hall type for the memory.
    pub hall_type: HallType,
    /// Concise memory summary.
    pub memory: String,
    /// Extracted knowledge graph triples.
    pub triples: Vec<Triple>,
}

/// Classify a conversation turn into the palace spatial structure.
///
/// Uses a single LLM call to determine Wing, Room, HallType, memory
/// summary, and knowledge graph triples.
pub async fn classify_turn(
    user_input: &str,
    assistant_response: &str,
    existing_wings: &[String],
    existing_rooms: &[String],
    llm: &dyn LlmApi,
) -> Result<ClassificationResult> {
    let prompt = classifier_prompt(
        user_input,
        assistant_response,
        existing_wings,
        existing_rooms,
    );

    let messages = vec![
        crate::llm::ChatMessage::system(CLASSIFIER_SYSTEM_PROMPT),
        crate::llm::ChatMessage::user(prompt),
    ];

    let response = llm.chat(&messages, None).await
        .context("Failed to classify conversation turn")?;

    parse_classification_response(&response.content)
}

/// Parse the LLM's classification response into a structured result.
fn parse_classification_response(response: &str) -> Result<ClassificationResult> {
    let mut wing_id = String::new();
    let mut wing_label = String::new();
    let mut room_id = String::new();
    let mut room_label = String::new();
    let mut hall_type = HallType::Facts;
    let mut memory = String::new();
    let mut triples = Vec::new();

    for line in response.lines() {
        let line = line.trim();

        if let Some(rest) = strip_prefix_ci(line, "WING_ID:") {
            wing_id = sanitize_slug(rest);
        } else if let Some(rest) = strip_prefix_ci(line, "WING_LABEL:") {
            wing_label = rest.trim().trim_matches('"').to_string();
        } else if let Some(rest) = strip_prefix_ci(line, "ROOM_ID:") {
            room_id = sanitize_slug(rest);
        } else if let Some(rest) = strip_prefix_ci(line, "ROOM_LABEL:") {
            room_label = rest.trim().trim_matches('"').to_string();
        } else if let Some(rest) = strip_prefix_ci(line, "HALL_TYPE:") {
            hall_type = parse_hall_type(rest.trim());
        } else if let Some(rest) = strip_prefix_ci(line, "MEMORY:") {
            memory = rest.trim().to_string();
        } else if let Some(rest) = strip_prefix_ci(line, "TRIPLES:") {
            triples = parse_triples(rest.trim(), &wing_id, &room_id);
        }
    }

    // Apply defaults for missing required fields
    if wing_id.is_empty() {
        wing_id = "default".to_string();
        wing_label = "Default".to_string();
    }
    if room_id.is_empty() {
        room_id = "general".to_string();
        room_label = "General".to_string();
    }
    if memory.is_empty() {
        memory = "(no summary extracted)".to_string();
    }

    if wing_label.is_empty() {
        wing_label = wing_id.clone();
    }
    if room_label.is_empty() {
        room_label = room_id.clone();
    }

    Ok(ClassificationResult {
        wing_id,
        wing_label,
        room_id,
        room_label,
        hall_type,
        memory,
        triples,
    })
}

/// Strip a prefix case-insensitively, returning the remainder.
fn strip_prefix_ci<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    if line.len() >= prefix.len()
        && line[..prefix.len()].eq_ignore_ascii_case(prefix)
    {
        Some(&line[prefix.len()..])
    } else {
        None
    }
}

/// Sanitize a string into a URL-safe slug.
fn sanitize_slug(s: &str) -> String {
    s.trim()
        .trim_matches('"')
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// Parse a hall type string into the enum.
fn parse_hall_type(s: &str) -> HallType {
    match s.to_lowercase().trim() {
        "facts" => HallType::Facts,
        "events" => HallType::Events,
        "discoveries" => HallType::Discoveries,
        "preferences" => HallType::Preferences,
        "advice" => HallType::Advice,
        _ => HallType::Facts, // Default fallback
    }
}

/// Parse triples from the LLM response format: "subject|predicate|object, ..."
fn parse_triples(s: &str, wing_id: &str, room_id: &str) -> Vec<Triple> {
    if s.eq_ignore_ascii_case("NONE") || s.is_empty() {
        return Vec::new();
    }

    s.split(',')
        .filter_map(|triple_str| {
            let parts: Vec<&str> = triple_str.trim().trim_matches('"').split('|').collect();
            match parts.len() {
                3 => Some(Triple::new(
                    parts[0].trim().to_string(),
                    parts[1].trim().to_string(),
                    parts[2].trim().to_string(),
                    room_id.to_string(),
                    wing_id.to_string(),
                )),
                4 => Some(Triple::with_validity(
                    parts[0].trim().to_string(),
                    parts[1].trim().to_string(),
                    parts[2].trim().to_string(),
                    room_id.to_string(),
                    wing_id.to_string(),
                    Some(parts[3].trim().to_string()),
                    None,
                )),
                _ => None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_classification_response() {
        let response = r#"WING_ID: project-daedalus
WING_LABEL: "Daedalus Project"
ROOM_ID: memory-system
ROOM_LABEL: "Memory System Design"
HALL_TYPE: facts
MEMORY: MemPalace uses ChromaDB as its vector database for spatial memory retrieval.
TRIPLES: Daedalus|uses|Rust, MemPalace|stores_in|ChromaDB"#;

        let result = parse_classification_response(response).unwrap();
        assert_eq!(result.wing_id, "project-daedalus");
        assert_eq!(result.wing_label, "Daedalus Project");
        assert_eq!(result.room_id, "memory-system");
        assert_eq!(result.room_label, "Memory System Design");
        assert_eq!(result.hall_type, HallType::Facts);
        assert!(result.memory.contains("ChromaDB"));
        assert_eq!(result.triples.len(), 2);
    }

    #[test]
    fn test_parse_classification_response_fallback() {
        let response = "Some random text without expected format";
        let result = parse_classification_response(response).unwrap();
        assert_eq!(result.wing_id, "default");
        assert_eq!(result.room_id, "general");
    }

    #[test]
    fn test_sanitize_slug() {
        assert_eq!(sanitize_slug("  Project Daedalus  "), "project-daedalus");
        assert_eq!(sanitize_slug("\"auth-migration\""), "auth-migration");
        assert_eq!(sanitize_slug("Hello World!"), "hello-world");
    }

    #[test]
    fn test_parse_hall_type() {
        assert_eq!(parse_hall_type("facts"), HallType::Facts);
        assert_eq!(parse_hall_type("EVENTS"), HallType::Events);
        assert_eq!(parse_hall_type("discoveries"), HallType::Discoveries);
        assert_eq!(parse_hall_type("preferences"), HallType::Preferences);
        assert_eq!(parse_hall_type("advice"), HallType::Advice);
        assert_eq!(parse_hall_type("unknown"), HallType::Facts);
    }

    #[test]
    fn test_parse_triples() {
        let triples = parse_triples(
            "Daedalus|uses|Rust, MemPalace|stores_in|ChromaDB",
            "wing1",
            "room1",
        );
        assert_eq!(triples.len(), 2);
        assert_eq!(triples[0].subject, "Daedalus");
        assert_eq!(triples[0].predicate, "uses");
        assert_eq!(triples[0].object, "Rust");
    }

    #[test]
    fn test_parse_triples_none() {
        let triples = parse_triples("NONE", "wing1", "room1");
        assert!(triples.is_empty());
    }

    #[test]
    fn test_parse_triples_with_valid_from() {
        let triples = parse_triples(
            "Max|started_school|Year 7|2026-09-01",
            "wing1",
            "room1",
        );
        assert_eq!(triples.len(), 1);
        assert_eq!(triples[0].subject, "Max");
        assert_eq!(triples[0].predicate, "started_school");
        assert_eq!(triples[0].object, "Year 7");
        assert_eq!(triples[0].valid_from, Some("2026-09-01".to_string()));
    }
}
