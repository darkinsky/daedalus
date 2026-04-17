//! Entity detection for MemPalace.
//!
//! Extracts proper nouns (people, projects, tools, concepts) from text
//! using regex-based pattern matching. Matches the original MemPalace
//! entity_detector.py.

use std::collections::{HashMap, HashSet};

use regex::Regex;
use once_cell::sync::Lazy;

/// Common capitalized words that look like proper nouns but are usually
/// sentence-starters or filler. Filtered out of entity extraction.
/// Matches the original MemPalace `_ENTITY_STOPLIST`.
#[allow(dead_code)]
static ENTITY_STOPLIST: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        "The", "This", "That", "These", "Those", "When", "Where", "What",
        "Why", "Who", "Which", "How", "After", "Before", "Then", "Now",
        "Here", "There", "And", "But", "Or", "Yet", "So", "If", "Else",
        "Yes", "No", "Maybe", "Okay", "User", "Assistant", "System", "Tool",
        "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday",
        "Sunday", "January", "February", "March", "April", "May", "June",
        "July", "August", "September", "October", "November", "December",
    ]
    .into_iter()
    .collect()
});

#[allow(dead_code)]
static CAPITALIZED_WORD_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\b[A-Z][a-z]{2,}\b").unwrap()
});

/// Extract proper noun candidates from text.
///
/// Returns a map of {name: frequency} for names appearing 2+ times.
/// Filters out common stopwords and sentence-starters.
#[allow(dead_code)]
pub fn extract_candidates(text: &str) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();

    for cap in CAPITALIZED_WORD_RE.find_iter(text) {
        let word = cap.as_str();
        if ENTITY_STOPLIST.contains(word) {
            continue;
        }
        if word.to_lowercase().as_str() == word {
            continue;
        }
        *counts.entry(word.to_string()).or_insert(0) += 1;
    }

    counts
        .into_iter()
        .filter(|(_, count)| *count >= 2)
        .collect()
}

/// Extract entities for metadata storage.
///
/// Returns a semicolon-separated string of the top 5 entities
/// found in the text, sorted by frequency.
#[allow(dead_code)]
pub fn extract_entities_for_metadata(text: &str) -> String {
    let candidates = extract_candidates(text);
    let mut sorted: Vec<(String, usize)> = candidates.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));

    sorted
        .into_iter()
        .take(5)
        .map(|(name, _)| name)
        .collect::<Vec<_>>()
        .join(";")
}

/// Entity type detection patterns: (type_name, format_patterns).
///
/// Each pattern uses `{}` as a placeholder for the entity name.
/// The first matching pattern determines the entity type.
const ENTITY_TYPE_PATTERNS: &[(&str, &[&str])] = &[
    ("person", &["{} said", "{} told", "{} asked", "with {}", "{}'s"]),
    ("project", &["{} project", "{} repo", "{} codebase", "deploy {}"]),
    ("tool", &["using {}", "install {}", "{} version"]),
];

/// Detect the type of an entity based on context patterns.
///
/// Returns one of: "person", "project", "tool", "unknown".
#[allow(dead_code)]
pub fn detect_entity_type(name: &str, context: &str) -> &'static str {
    let name_lower = name.to_lowercase();
    let context_lower = context.to_lowercase();

    for &(entity_type, patterns) in ENTITY_TYPE_PATTERNS {
        for pattern_template in patterns {
            let pattern = pattern_template.replace("{}", &name_lower);
            if context_lower.contains(&pattern) {
                return entity_type;
            }
        }
    }

    "unknown"
}

/// Common English words that could be confused with names.
/// Used for ambiguity detection.
#[allow(dead_code)]
pub static COMMON_ENGLISH_WORDS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        "ever", "grace", "will", "bill", "mark", "april", "may", "june",
        "joy", "hope", "faith", "chance", "chase", "hunter", "dash", "flash",
        "star", "sky", "river", "brook", "lane", "art", "clay", "max", "rex",
        "ray", "jay", "rose", "violet", "lily", "ivy", "ash", "reed", "sage",
    ]
    .into_iter()
    .collect()
});

/// Check if a name is ambiguous (could be a common English word).
#[allow(dead_code)]
pub fn is_ambiguous_name(name: &str) -> bool {
    COMMON_ENGLISH_WORDS.contains(name.to_lowercase().as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_candidates() {
        let text = "Alice went to the store. Alice bought some food. Bob was there too.";
        let candidates = extract_candidates(text);
        assert!(candidates.contains_key("Alice"));
        assert_eq!(candidates["Alice"], 2);
    }

    #[test]
    fn test_extract_entities_for_metadata() {
        let text = "Alice and Bob discussed the project. Alice mentioned ChromaDB. Bob agreed with Alice.";
        let entities = extract_entities_for_metadata(text);
        assert!(entities.contains("Alice"));
    }

    #[test]
    fn test_entity_stoplist_filtered() {
        let text = "The system was running. The system was fast. When the system started.";
        let candidates = extract_candidates(text);
        assert!(!candidates.contains_key("The"));
        assert!(!candidates.contains_key("When"));
    }

    #[test]
    fn test_is_ambiguous_name() {
        assert!(is_ambiguous_name("Grace"));
        assert!(is_ambiguous_name("will"));
        assert!(!is_ambiguous_name("Alice"));
    }
}
