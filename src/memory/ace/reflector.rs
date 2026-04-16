use crate::llm::{ChatMessage, LlmApi};
use crate::memory::strip_directive_prefix;

use super::config::AceConfig;
use super::curator::Curator;
use super::playbook::{DeltaEntry, Playbook};

/// LLM-based reflection engine for the ACE memory strategy.
///
/// The Reflector analyzes each conversation turn and produces structured
/// delta entries (ADD/UPDATE/REINFORCE/REMOVE) that the Curator then
/// merges into the Playbook using deterministic logic.
///
/// This separation of concerns is the key ACE innovation:
/// - **Reflector** (LLM): Produces small, focused delta entries
/// - **Curator** (deterministic): Merges deltas without LLM calls
///
/// This prevents context collapse because the LLM never rewrites
/// the entire playbook — it only suggests incremental changes.
///
/// Reference: ACE (arxiv:2510.04618)
pub struct Reflector;

impl Reflector {
    /// Perform a full reflection cycle: call the LLM and apply deltas via Curator.
    ///
    /// This is the main entry point called by `AceMemory::reflect_on_turn()`.
    /// It encapsulates the complete Generate→Reflect→Curate flow:
    /// 1. Check `auto_reflect` config
    /// 2. Build the reflection prompt (with current playbook state)
    /// 3. Call the LLM to produce delta entries
    /// 4. Parse the response into `DeltaEntry` values
    /// 5. Apply deltas via the Curator (deterministic merge)
    ///
    /// Reflection failures are logged but never propagated — they must not
    /// disrupt the main conversation flow.
    pub async fn reflect_and_curate(
        playbook: &mut Playbook,
        user_input: &str,
        assistant_response: &str,
        llm: &dyn LlmApi,
        config: &AceConfig,
    ) {
        if !config.auto_reflect {
            return;
        }

        let current_playbook = playbook.to_numbered_text();
        let user_prompt = super::prompts::build_reflect_prompt(
            user_input,
            assistant_response,
            &current_playbook,
        );

        let messages = vec![
            ChatMessage::system(super::prompts::REFLECT_SYSTEM_PROMPT),
            ChatMessage::user(user_prompt),
        ];

        match llm.chat(&messages, None).await {
            Ok(response) => {
                let deltas = Self::parse_reflection_response(&response.content);
                if deltas.is_empty() {
                    tracing::debug!("ACE Reflector: no changes from reflection");
                    return;
                }

                let delta_count = deltas.len();
                Curator::apply_deltas(playbook, deltas, config);

                tracing::info!(
                    deltas = delta_count,
                    total_bullets = playbook.total_bullets(),
                    sections = playbook.sections.len(),
                    "ACE reflection complete"
                );
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "ACE Reflector LLM call failed, continuing without update"
                );
            }
        }
    }

    /// Parse the LLM's reflection response into delta entries.
    ///
    /// Expected format (one per line):
    /// ```text
    /// ADD: <section_title> | <content>
    /// UPDATE: <section_title> | <bullet_number> | <refined_content>
    /// REINFORCE: <section_title> | <bullet_number>
    /// REMOVE: <section_title> | <bullet_number>
    /// NO_CHANGES
    /// ```
    fn parse_reflection_response(response: &str) -> Vec<DeltaEntry> {
        let trimmed = response.trim();

        if trimmed.eq_ignore_ascii_case("NO_CHANGES") {
            return vec![];
        }

        let mut deltas = Vec::new();

        for line in trimmed.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            if let Some(rest) = strip_directive_prefix(line, "ADD:") {
                if let Some(delta) = Self::parse_add_directive(rest) {
                    deltas.push(delta);
                }
            } else if let Some(rest) = strip_directive_prefix(line, "UPDATE:") {
                if let Some(delta) = Self::parse_update_directive(rest) {
                    deltas.push(delta);
                }
            } else if let Some(rest) = strip_directive_prefix(line, "REINFORCE:") {
                if let Some(delta) = Self::parse_reinforce_directive(rest) {
                    deltas.push(delta);
                }
            } else if let Some(rest) = strip_directive_prefix(line, "REMOVE:") {
                if let Some(delta) = Self::parse_remove_directive(rest) {
                    deltas.push(delta);
                }
            }
            // Ignore unrecognized lines (LLM may add commentary).
        }

        deltas
    }

    /// Parse an `ADD:` directive.
    ///
    /// Expected: `<section_title> | <content>`
    fn parse_add_directive(body: &str) -> Option<DeltaEntry> {
        let (section, content) = body.split_once('|')?;
        let section = section.trim();
        let content = content.trim();
        if section.is_empty() || content.is_empty() {
            return None;
        }
        Some(DeltaEntry::Add {
            section: section.to_string(),
            content: content.to_string(),
        })
    }

    /// Parse an `UPDATE:` directive.
    ///
    /// Expected: `<section_title> | <bullet_number> | <refined_content>`
    fn parse_update_directive(body: &str) -> Option<DeltaEntry> {
        let parts: Vec<&str> = body.splitn(3, '|').collect();
        if parts.len() < 3 {
            return None;
        }
        let section = parts[0].trim();
        let bullet_index: usize = parts[1].trim().parse().ok()?;
        let new_content = parts[2].trim();
        if section.is_empty() || new_content.is_empty() || bullet_index < 1 {
            return None;
        }
        Some(DeltaEntry::Update {
            section: section.to_string(),
            bullet_index,
            new_content: new_content.to_string(),
        })
    }

    /// Parse a `REINFORCE:` directive.
    ///
    /// Expected: `<section_title> | <bullet_number>`
    fn parse_reinforce_directive(body: &str) -> Option<DeltaEntry> {
        let (section, num_str) = body.split_once('|')?;
        let section = section.trim();
        let bullet_index: usize = num_str.trim().parse().ok()?;
        if section.is_empty() || bullet_index < 1 {
            return None;
        }
        Some(DeltaEntry::Reinforce {
            section: section.to_string(),
            bullet_index,
        })
    }

    /// Parse a `REMOVE:` directive.
    ///
    /// Expected: `<section_title> | <bullet_number>`
    fn parse_remove_directive(body: &str) -> Option<DeltaEntry> {
        let (section, num_str) = body.split_once('|')?;
        let section = section.trim();
        let bullet_index: usize = num_str.trim().parse().ok()?;
        if section.is_empty() || bullet_index < 1 {
            return None;
        }
        Some(DeltaEntry::Remove {
            section: section.to_string(),
            bullet_index,
        })
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_add_directive() {
        let delta = Reflector::parse_add_directive(" Error Handling | Always check return values ").unwrap();
        match delta {
            DeltaEntry::Add { section, content } => {
                assert_eq!(section, "Error Handling");
                assert_eq!(content, "Always check return values");
            }
            _ => panic!("Expected Add delta"),
        }
    }

    #[test]
    fn test_parse_add_directive_empty_content() {
        assert!(Reflector::parse_add_directive(" Section | ").is_none());
    }

    #[test]
    fn test_parse_update_directive() {
        let delta = Reflector::parse_update_directive(" Strategies | 2 | Refined content ").unwrap();
        match delta {
            DeltaEntry::Update { section, bullet_index, new_content } => {
                assert_eq!(section, "Strategies");
                assert_eq!(bullet_index, 2);
                assert_eq!(new_content, "Refined content");
            }
            _ => panic!("Expected Update delta"),
        }
    }

    #[test]
    fn test_parse_update_directive_missing_parts() {
        assert!(Reflector::parse_update_directive(" Section | 1 ").is_none());
    }

    #[test]
    fn test_parse_reinforce_directive() {
        let delta = Reflector::parse_reinforce_directive(" Strategies | 3 ").unwrap();
        match delta {
            DeltaEntry::Reinforce { section, bullet_index } => {
                assert_eq!(section, "Strategies");
                assert_eq!(bullet_index, 3);
            }
            _ => panic!("Expected Reinforce delta"),
        }
    }

    #[test]
    fn test_parse_remove_directive() {
        let delta = Reflector::parse_remove_directive(" Errors | 1 ").unwrap();
        match delta {
            DeltaEntry::Remove { section, bullet_index } => {
                assert_eq!(section, "Errors");
                assert_eq!(bullet_index, 1);
            }
            _ => panic!("Expected Remove delta"),
        }
    }

    #[test]
    fn test_parse_reflection_response_no_changes() {
        let deltas = Reflector::parse_reflection_response("NO_CHANGES");
        assert!(deltas.is_empty());
    }

    #[test]
    fn test_parse_reflection_response_case_insensitive() {
        let deltas = Reflector::parse_reflection_response("no_changes");
        assert!(deltas.is_empty());
    }

    #[test]
    fn test_parse_reflection_response_mixed() {
        let response = "\
ADD: Strategies | Use binary search for sorted arrays
UPDATE: Error Handling | 1 | Always validate input before processing
REINFORCE: Performance | 2
REMOVE: Deprecated | 1
Some commentary line that should be ignored";

        let deltas = Reflector::parse_reflection_response(response);
        assert_eq!(deltas.len(), 4);

        match &deltas[0] {
            DeltaEntry::Add { section, content } => {
                assert_eq!(section, "Strategies");
                assert!(content.contains("binary search"));
            }
            _ => panic!("Expected Add"),
        }

        match &deltas[1] {
            DeltaEntry::Update { section, bullet_index, new_content } => {
                assert_eq!(section, "Error Handling");
                assert_eq!(*bullet_index, 1);
                assert!(new_content.contains("validate input"));
            }
            _ => panic!("Expected Update"),
        }

        match &deltas[2] {
            DeltaEntry::Reinforce { section, bullet_index } => {
                assert_eq!(section, "Performance");
                assert_eq!(*bullet_index, 2);
            }
            _ => panic!("Expected Reinforce"),
        }

        match &deltas[3] {
            DeltaEntry::Remove { section, bullet_index } => {
                assert_eq!(section, "Deprecated");
                assert_eq!(*bullet_index, 1);
            }
            _ => panic!("Expected Remove"),
        }
    }

    #[test]
    fn test_parse_reflection_response_empty() {
        let deltas = Reflector::parse_reflection_response("");
        assert!(deltas.is_empty());
    }

    #[test]
    fn test_parse_reflection_response_case_insensitive_directives() {
        let response = "add: Strategies | New insight\nreinforce: Test | 1";
        let deltas = Reflector::parse_reflection_response(response);
        assert_eq!(deltas.len(), 2);
    }

    #[test]
    fn test_strip_directive_prefix() {
        assert_eq!(strip_directive_prefix("ADD: content", "ADD:"), Some(" content"));
        assert_eq!(strip_directive_prefix("add: content", "ADD:"), Some(" content"));
        assert_eq!(strip_directive_prefix("REMOVE: x", "ADD:"), None);
        assert_eq!(strip_directive_prefix("AD", "ADD:"), None);
    }
}
