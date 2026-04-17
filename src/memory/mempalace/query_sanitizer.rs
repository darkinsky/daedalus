//! Query sanitizer for MemPalace search.
//!
//! Cleans up search queries that may contain system prompts, long context,
//! or other noise. Extracts the actual search intent from bloated queries.
//!
//! Matches the original MemPalace query_sanitizer.py.

use regex::Regex;
use once_cell::sync::Lazy;

/// Maximum query length before sanitization kicks in.
const SAFE_QUERY_LENGTH: usize = 250;
/// Minimum acceptable query length after extraction.
const MIN_QUERY_LENGTH: usize = 5;
/// Maximum query length after sanitization.
const MAX_QUERY_LENGTH: usize = 250;

static SENTENCE_SPLIT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"[.!?。！？]+\s*").unwrap()
});

static QUESTION_MARK: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"[?？]").unwrap()
});

/// Result of query sanitization.
#[derive(Debug, Clone)]
pub struct SanitizeResult {
    /// The cleaned query.
    pub clean_query: String,
    /// Whether the query was modified.
    #[allow(dead_code)]
    pub was_sanitized: bool,
    /// Original query length.
    #[allow(dead_code)]
    pub original_length: usize,
    /// Clean query length.
    #[allow(dead_code)]
    pub clean_length: usize,
    /// Method used for sanitization.
    #[allow(dead_code)]
    pub method: String,
}

/// Sanitize a search query.
///
/// Handles queries that are too long (e.g., contain system prompts or
/// full conversation context). Extracts the actual search intent.
pub fn sanitize_query(raw_query: &str) -> SanitizeResult {
    let raw_query = raw_query.trim();
    let original_length = raw_query.len();

    // Step 1: Short query passthrough
    if original_length <= SAFE_QUERY_LENGTH {
        return SanitizeResult {
            clean_query: raw_query.to_string(),
            was_sanitized: false,
            original_length,
            clean_length: original_length,
            method: "passthrough".to_string(),
        };
    }

    // Step 2: Question extraction
    // Split on newlines and find segments ending with ?
    let all_segments: Vec<&str> = raw_query
        .lines()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    // Look for question marks in segments (prefer later ones)
    for seg in all_segments.iter().rev() {
        if QUESTION_MARK.is_match(seg) && seg.len() >= MIN_QUERY_LENGTH {
            let candidate = trim_candidate(seg);
            if candidate.len() >= MIN_QUERY_LENGTH {
                tracing::debug!(
                    "Query sanitized: {} → {} chars (method=question_extraction)",
                    original_length,
                    candidate.len()
                );
                return SanitizeResult {
                    clean_query: candidate.clone(),
                    was_sanitized: true,
                    original_length,
                    clean_length: candidate.len(),
                    method: "question_extraction".to_string(),
                };
            }
        }
    }

    // Step 3: Tail sentence extraction
    for seg in all_segments.iter().rev() {
        if seg.len() >= MIN_QUERY_LENGTH {
            let candidate = trim_candidate(seg);
            if candidate.len() >= MIN_QUERY_LENGTH {
                tracing::debug!(
                    "Query sanitized: {} → {} chars (method=tail_sentence)",
                    original_length,
                    candidate.len()
                );
                return SanitizeResult {
                    clean_query: candidate.clone(),
                    was_sanitized: true,
                    original_length,
                    clean_length: candidate.len(),
                    method: "tail_sentence".to_string(),
                };
            }
        }
    }

    // Step 4: Tail truncation (fallback)
    let start = if raw_query.len() > MAX_QUERY_LENGTH {
        raw_query.len() - MAX_QUERY_LENGTH
    } else {
        0
    };
    let candidate = raw_query[start..].trim().to_string();
    tracing::debug!(
        "Query sanitized: {} → {} chars (method=tail_truncation)",
        original_length,
        candidate.len()
    );
    SanitizeResult {
        clean_query: candidate.clone(),
        was_sanitized: true,
        original_length,
        clean_length: candidate.len(),
        method: "tail_truncation".to_string(),
    }
}

/// Trim a candidate query to fit within MAX_QUERY_LENGTH.
fn trim_candidate(candidate: &str) -> String {
    let candidate = strip_wrapping_quotes(candidate);
    if candidate.len() <= MAX_QUERY_LENGTH {
        return candidate;
    }

    // Try splitting into sentences and finding a good fragment
    for frag in SENTENCE_SPLIT.split(&candidate) {
        let frag = strip_wrapping_quotes(frag.trim());
        if frag.len() >= MIN_QUERY_LENGTH && frag.len() <= MAX_QUERY_LENGTH {
            return frag;
        }
    }

    // Fallback: take the last MAX_QUERY_LENGTH characters
    let start = candidate.len().saturating_sub(MAX_QUERY_LENGTH);
    candidate[start..].trim().to_string()
}

/// Strip wrapping quotes from a string.
fn strip_wrapping_quotes(s: &str) -> String {
    let s = s.trim();
    let quote_pairs: &[(char, char)] = &[
        ('\'', '\''),
        ('"', '"'),
        ('`', '`'),
        ('\u{201c}', '\u{201d}'),  // "" (left/right double)
        ('\u{2018}', '\u{2019}'),  // '' (left/right single)
    ];

    let mut result = s.to_string();
    loop {
        if result.len() < 2 {
            break;
        }
        let first = result.chars().next().unwrap();
        let last = result.chars().next_back().unwrap();

        let matched = quote_pairs.iter().any(|&(open, close)| {
            first == open && last == close
        });

        if matched {
            let start = first.len_utf8();
            let end = result.len() - last.len_utf8();
            if start < end {
                result = result[start..end].trim().to_string();
            } else {
                break;
            }
        } else {
            break;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_short_query_passthrough() {
        let result = sanitize_query("What is ChromaDB?");
        assert!(!result.was_sanitized);
        assert_eq!(result.method, "passthrough");
    }

    #[test]
    fn test_long_query_with_question() {
        let long_prefix = "a ".repeat(200);
        let query = format!("{}What is the best database for vector search?", long_prefix);
        let result = sanitize_query(&query);
        assert!(result.was_sanitized);
        assert!(result.clean_query.contains("database"));
    }

    #[test]
    fn test_strip_wrapping_quotes() {
        assert_eq!(strip_wrapping_quotes("\"hello\""), "hello");
        assert_eq!(strip_wrapping_quotes("'world'"), "world");
        assert_eq!(strip_wrapping_quotes("no quotes"), "no quotes");
    }
}
