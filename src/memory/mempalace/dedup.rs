//! Deduplication for MemPalace drawers.
//!
//! Detects near-duplicate content before filing into the palace.
//! Uses Jaccard similarity (word-level set overlap) for fast local
//! dedup checking without requiring ChromaDB.
//!
//! Matches the original MemPalace dedup.py.

/// Check if content is a near-duplicate of existing drawers.
///
/// Returns true if the content is too similar to an existing drawer
/// (cosine distance < threshold).
pub fn is_duplicate(
    content: &str,
    existing_contents: &[String],
    threshold: f32,
) -> bool {
    if existing_contents.is_empty() {
        return false;
    }

    // Simple text-based dedup: check for exact or near-exact matches
    let content_lower = content.to_lowercase();
    let content_words: std::collections::HashSet<&str> = content_lower
        .split_whitespace()
        .filter(|w| w.len() > 2)
        .collect();

    if content_words.is_empty() {
        return false;
    }

    for existing in existing_contents {
        let existing_lower = existing.to_lowercase();
        let existing_words: std::collections::HashSet<&str> = existing_lower
            .split_whitespace()
            .filter(|w| w.len() > 2)
            .collect();

        if existing_words.is_empty() {
            continue;
        }

        // Jaccard similarity
        let intersection = content_words.intersection(&existing_words).count();
        let union = content_words.union(&existing_words).count();

        if union > 0 {
            let similarity = intersection as f32 / union as f32;
            // Convert similarity to distance for comparison with threshold
            let distance = 1.0 - similarity;
            if distance < threshold {
                return true;
            }
        }
    }

    false
}

/// Dedup statistics for a set of drawers.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct DedupStats {
    /// Total drawers checked.
    pub total_checked: usize,
    /// Number of duplicates found.
    pub duplicates_found: usize,
    /// Number of unique drawers.
    pub unique_count: usize,
}

/// Analyze a set of drawer contents for duplicates.
#[allow(dead_code)]
pub fn analyze_duplicates(
    contents: &[String],
    threshold: f32,
) -> DedupStats {
    let mut unique_indices = Vec::new();
    let mut duplicates = 0;

    for (i, content) in contents.iter().enumerate() {
        let existing: Vec<String> = unique_indices
            .iter()
            .map(|&idx: &usize| contents[idx].clone())
            .collect();

        if is_duplicate(content, &existing, threshold) {
            duplicates += 1;
        } else {
            unique_indices.push(i);
        }
    }

    DedupStats {
        total_checked: contents.len(),
        duplicates_found: duplicates,
        unique_count: unique_indices.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_duplicate() {
        let content = "This is a test sentence with enough words to match";
        let existing = vec![content.to_string()];
        assert!(is_duplicate(content, &existing, 0.15));
    }

    #[test]
    fn test_no_duplicate() {
        let content = "This is about Rust programming language features";
        let existing = vec!["Python is great for data science and machine learning".to_string()];
        assert!(!is_duplicate(content, &existing, 0.15));
    }

    #[test]
    fn test_empty_existing() {
        let content = "Some content here";
        assert!(!is_duplicate(content, &[], 0.15));
    }

    #[test]
    fn test_analyze_duplicates() {
        let contents = vec![
            "The quick brown fox jumps over the lazy dog".to_string(),
            "The quick brown fox jumps over the lazy dog".to_string(),
            "A completely different sentence about programming".to_string(),
        ];
        let stats = analyze_duplicates(&contents, 0.15);
        assert_eq!(stats.total_checked, 3);
        assert_eq!(stats.duplicates_found, 1);
        assert_eq!(stats.unique_count, 2);
    }
}
