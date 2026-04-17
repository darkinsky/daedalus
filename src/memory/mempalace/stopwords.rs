//! Shared stopword lists for MemPalace text processing.
//!
//! Centralizes stopword definitions used by multiple modules
//! (dialect, entity_detector) to avoid duplication.

use std::collections::HashSet;
use once_cell::sync::Lazy;

/// Common English stopwords for text processing.
///
/// Used by both the AAAK dialect compressor and the entity detector
/// to filter out noise words from analysis.
#[allow(dead_code)]
pub static STOPWORDS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        // Articles & determiners
        "the", "a", "an", "this", "that", "these", "those",
        // Be verbs
        "is", "are", "was", "were", "be", "been", "being",
        // Have verbs
        "have", "has", "had",
        // Do verbs
        "do", "does", "did",
        // Modal verbs
        "will", "would", "could", "should", "may", "might", "shall", "can",
        // Prepositions
        "to", "of", "in", "for", "on", "with", "at", "by", "from", "as",
        "into", "about", "between", "through", "during", "before", "after",
        "above", "below", "up", "down", "out", "off", "over", "under",
        // Conjunctions
        "and", "but", "or", "if", "while", "so", "nor", "yet",
        // Pronouns
        "it", "its", "i", "we", "you", "he", "she", "they",
        "me", "him", "her", "us", "them",
        "my", "your", "his", "our", "their",
        // Question words
        "what", "which", "who", "whom", "where", "when", "why", "how",
        // Adverbs & fillers
        "also", "much", "many", "like", "very", "too", "just", "really",
        "well", "only", "now", "then", "here", "there", "again", "further",
        "once", "not", "no",
        // Common verbs
        "get", "got", "use", "used", "using", "make", "made",
        "want", "need",
        // Generic nouns
        "thing", "things", "way",
        // Other
        "all", "each", "every", "both", "few", "more", "most", "other",
        "some", "such", "own", "same", "than", "because", "since",
        "don",
    ]
    .into_iter()
    .collect()
});
