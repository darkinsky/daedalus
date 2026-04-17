//! AAAK Dialect — Compressed Symbolic Memory for MemPalace.
//!
//! AAAK is a compressed memory dialect that MemPalace uses for efficient storage.
//! It is designed to be readable by both humans and LLMs without decoding.
//!
//! Matches the original MemPalace dialect.py.
//!
//! FORMAT:
//!   ENTITIES: 3-letter uppercase codes. ALC=Alice, JOR=Jordan, RIL=Riley.
//!   EMOTIONS: *action markers* before/during text.
//!   STRUCTURE: Pipe-separated fields.
//!   DATES: ISO format (2026-03-31).
//!   IMPORTANCE: ★ to ★★★★★ (1-5 scale).

use std::collections::HashMap;
use std::path::Path;

use regex::Regex;
use once_cell::sync::Lazy;

use super::stopwords::STOPWORDS;

/// Emotion keyword → AAAK code mapping.
#[allow(dead_code)]
static EMOTION_CODES: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    let mut m = HashMap::new();
    m.insert("joy", "*warm*");
    m.insert("happy", "*warm*");
    m.insert("love", "*bloom*");
    m.insert("tender", "*bloom*");
    m.insert("sad", "*ache*");
    m.insert("grief", "*ache*");
    m.insert("angry", "*fierce*");
    m.insert("determined", "*fierce*");
    m.insert("fear", "*chill*");
    m.insert("scared", "*chill*");
    m.insert("vulnerable", "*raw*");
    m.insert("honest", "*raw*");
    m.insert("surprised", "*spark*");
    m.insert("curious", "*spark*");
    m.insert("proud", "*glow*");
    m.insert("confident", "*glow*");
    m.insert("anxious", "*tremor*");
    m.insert("worried", "*tremor*");
    m
});

/// Flag keyword → flag code mapping.
#[allow(dead_code)]
static FLAG_SIGNALS: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    let mut m = HashMap::new();
    m.insert("decided", "DECISION");
    m.insert("chose", "DECISION");
    m.insert("switched", "DECISION");
    m.insert("migrated", "DECISION");
    m.insert("replaced", "DECISION");
    m.insert("because", "DECISION");
    m.insert("founded", "ORIGIN");
    m.insert("created", "ORIGIN");
    m.insert("started", "ORIGIN");
    m.insert("born", "ORIGIN");
    m.insert("launched", "ORIGIN");
    m.insert("core", "CORE");
    m.insert("fundamental", "CORE");
    m.insert("essential", "CORE");
    m.insert("principle", "CORE");
    m.insert("turning point", "PIVOT");
    m.insert("changed everything", "PIVOT");
    m.insert("realized", "PIVOT");
    m.insert("breakthrough", "PIVOT");
    m.insert("api", "TECHNICAL");
    m.insert("database", "TECHNICAL");
    m.insert("architecture", "TECHNICAL");
    m.insert("deploy", "TECHNICAL");
    m.insert("framework", "TECHNICAL");
    m
});

/// AAAK Dialect encoder — works on plain text.
#[allow(dead_code)]
pub struct Dialect {
    /// Entity name → short code mapping.
    pub entity_codes: HashMap<String, String>,
    /// Names to skip.
    pub skip_names: Vec<String>,
}

#[allow(dead_code)]
impl Dialect {
    /// Create a new dialect with optional entity mappings.
    pub fn new(entities: Option<HashMap<String, String>>) -> Self {
        let mut entity_codes = HashMap::new();
        if let Some(ents) = entities {
            for (name, code) in ents {
                entity_codes.insert(name.to_lowercase(), code.clone());
                entity_codes.insert(name.clone(), code);
            }
        }
        Self {
            entity_codes,
            skip_names: Vec::new(),
        }
    }

    /// Encode an entity name to its short code.
    pub fn encode_entity(&self, name: &str) -> Option<String> {
        if self.skip_names.iter().any(|s| name.to_lowercase().contains(s)) {
            return None;
        }
        if let Some(code) = self.entity_codes.get(name) {
            return Some(code.clone());
        }
        if let Some(code) = self.entity_codes.get(&name.to_lowercase()) {
            return Some(code.clone());
        }
        // Auto-code: first 3 chars uppercase
        Some(name.chars().take(3).collect::<String>().to_uppercase())
    }

    /// Compress plain text into AAAK Dialect format.
    ///
    /// This is lossy — the original text cannot be reconstructed from the output.
    pub fn compress(&self, text: &str, metadata: Option<&CompressMetadata>) -> String {
        // Detect entities
        let entities = self.detect_entities_in_text(text);
        let entity_str = if entities.is_empty() {
            "???".to_string()
        } else {
            entities[..entities.len().min(3)].join("+")
        };

        // Extract topics
        let topics = self.extract_topics(text, 3);
        let topic_str = if topics.is_empty() {
            "misc".to_string()
        } else {
            topics.join("_")
        };

        // Extract key sentence
        let quote = self.extract_key_sentence(text);
        let quote_part = if quote.is_empty() {
            String::new()
        } else {
            format!("\"{}\"", quote)
        };

        // Detect emotions
        let emotions = self.detect_emotions(text);
        let emotion_str = emotions.join("+");

        // Detect flags
        let flags = self.detect_flags(text);
        let flag_str = flags.join("+");

        let mut lines = Vec::new();

        // Header line (if metadata available)
        if let Some(meta) = metadata {
            let header_parts = vec![
                meta.wing.as_deref().unwrap_or("?"),
                meta.room.as_deref().unwrap_or("?"),
                meta.date.as_deref().unwrap_or("?"),
                meta.source_file
                    .as_deref()
                    .and_then(|s| Path::new(s).file_stem())
                    .and_then(|s| s.to_str())
                    .unwrap_or("?"),
            ];
            lines.push(header_parts.join("|"));
        }

        // Content line
        let mut parts = vec![format!("0:{}", entity_str), topic_str];
        if !quote_part.is_empty() {
            parts.push(quote_part);
        }
        if !emotion_str.is_empty() {
            parts.push(emotion_str);
        }
        if !flag_str.is_empty() {
            parts.push(flag_str);
        }

        lines.push(parts.join("|"));
        lines.join("\n")
    }

    /// Detect keyword signals from text using a signal mapping.
    ///
    /// Shared implementation for both emotion detection and flag detection.
    fn detect_signals(
        text_lower: &str,
        signals: &HashMap<&str, &str>,
        max_results: usize,
    ) -> Vec<String> {
        let mut detected = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for (keyword, code) in signals.iter() {
            if text_lower.contains(keyword) && !seen.contains(*code) {
                detected.push(code.to_string());
                seen.insert(*code);
            }
        }
        detected.truncate(max_results);
        detected
    }

    /// Detect emotions from plain text using keyword signals.
    fn detect_emotions(&self, text: &str) -> Vec<String> {
        Self::detect_signals(&text.to_lowercase(), &EMOTION_CODES, 3)
    }

    /// Detect importance flags from plain text.
    fn detect_flags(&self, text: &str) -> Vec<String> {
        Self::detect_signals(&text.to_lowercase(), &FLAG_SIGNALS, 3)
    }

    /// Extract key topic words from plain text.
    fn extract_topics(&self, text: &str, max_topics: usize) -> Vec<String> {
        let word_re = Regex::new(r"[a-zA-Z][a-zA-Z_-]{2,}").unwrap();
        let mut freq: HashMap<String, usize> = HashMap::new();

        for cap in word_re.find_iter(text) {
            let w = cap.as_str().to_lowercase();
            if STOPWORDS.contains(w.as_str()) || w.len() < 3 {
                continue;
            }
            *freq.entry(w).or_insert(0) += 1;
        }

        // Boost proper nouns and technical terms
        for cap in word_re.find_iter(text) {
            let w = cap.as_str();
            let w_lower = w.to_lowercase();
            if STOPWORDS.contains(w_lower.as_str()) {
                continue;
            }
            if w.chars().next().map_or(false, |c| c.is_uppercase()) {
                if let Some(count) = freq.get_mut(&w_lower) {
                    *count += 2;
                }
            }
            if w.contains('_') || w.contains('-') {
                if let Some(count) = freq.get_mut(&w_lower) {
                    *count += 2;
                }
            }
        }

        let mut ranked: Vec<(String, usize)> = freq.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1));
        ranked
            .into_iter()
            .take(max_topics)
            .map(|(w, _)| w)
            .collect()
    }

    /// Extract the most important sentence fragment from text.
    fn extract_key_sentence(&self, text: &str) -> String {
        let sentences: Vec<&str> = text
            .split(|c: char| c == '.' || c == '!' || c == '?' || c == '\n')
            .map(|s| s.trim())
            .filter(|s| s.len() > 10)
            .collect();

        if sentences.is_empty() {
            return String::new();
        }

        let decision_words = [
            "decided", "because", "instead", "prefer", "switched", "chose",
            "realized", "important", "key", "critical", "discovered", "learned",
            "conclusion", "solution", "reason", "why", "breakthrough", "insight",
        ];

        let mut best_score = -1i32;
        let mut best = "";

        for s in &sentences {
            let s_lower = s.to_lowercase();
            let mut score: i32 = 0;
            for w in &decision_words {
                if s_lower.contains(w) {
                    score += 2;
                }
            }
            if s.len() < 80 {
                score += 1;
            }
            if s.len() < 40 {
                score += 1;
            }
            if s.len() > 150 {
                score -= 2;
            }
            if score > best_score {
                best_score = score;
                best = s;
            }
        }

        if best.chars().count() > 55 {
            let truncated: String = best.chars().take(52).collect();
            format!("{}...", truncated)
        } else {
            best.to_string()
        }
    }

    /// Find known entities in text, or detect capitalized names.
    fn detect_entities_in_text(&self, text: &str) -> Vec<String> {
        let mut found = Vec::new();

        // Check known entities
        for (name, code) in &self.entity_codes {
            if !name.chars().next().map_or(false, |c| c.is_lowercase())
                && text.to_lowercase().contains(&name.to_lowercase())
            {
                if !found.contains(code) {
                    found.push(code.clone());
                }
            }
        }
        if !found.is_empty() {
            return found;
        }

        // Fallback: find capitalized words that look like names
        let words: Vec<&str> = text.split_whitespace().collect();
        for (i, w) in words.iter().enumerate() {
            let clean: String = w.chars().filter(|c| c.is_alphabetic()).collect();
            if clean.len() >= 2
                && clean.chars().next().map_or(false, |c| c.is_uppercase())
                && clean[1..].chars().all(|c| c.is_lowercase())
                && i > 0
                && !STOPWORDS.contains(clean.to_lowercase().as_str())
            {
                let code = clean.chars().take(3).collect::<String>().to_uppercase();
                if !found.contains(&code) {
                    found.push(code);
                }
                if found.len() >= 3 {
                    break;
                }
            }
        }

        found
    }

    /// Estimate token count using word-based heuristic (~1.3 tokens per word).
    pub fn count_tokens(text: &str) -> usize {
        let words = text.split_whitespace().count();
        (words as f64 * 1.3).max(1.0) as usize
    }

    /// Get compression stats for a text→AAAK conversion.
    pub fn compression_stats(original: &str, compressed: &str) -> CompressionStats {
        let orig_tokens = Self::count_tokens(original);
        let comp_tokens = Self::count_tokens(compressed);
        CompressionStats {
            original_tokens_est: orig_tokens,
            summary_tokens_est: comp_tokens,
            size_ratio: orig_tokens as f64 / comp_tokens.max(1) as f64,
            original_chars: original.len(),
            summary_chars: compressed.len(),
        }
    }
}

/// Metadata for AAAK compression.
#[allow(dead_code)]
pub struct CompressMetadata {
    pub wing: Option<String>,
    pub room: Option<String>,
    pub date: Option<String>,
    pub source_file: Option<String>,
}

/// Compression statistics.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct CompressionStats {
    pub original_tokens_est: usize,
    pub summary_tokens_est: usize,
    pub size_ratio: f64,
    pub original_chars: usize,
    pub summary_chars: usize,
}

/// The AAAK dialect specification string.
///
/// Included in status responses so the AI learns it on first wake-up call.
pub const AAAK_SPEC: &str = r#"AAAK is a compressed memory dialect that MemPalace uses for efficient storage.
It is designed to be readable by both humans and LLMs without decoding.

FORMAT:
  ENTITIES: 3-letter uppercase codes. ALC=Alice, JOR=Jordan, RIL=Riley, MAX=Max, BEN=Ben.
  EMOTIONS: *action markers* before/during text. *warm*=joy, *fierce*=determined, *raw*=vulnerable, *bloom*=tenderness.
  STRUCTURE: Pipe-separated fields. FAM: family | PROJ: projects | ⚠: warnings/reminders.
  DATES: ISO format (2026-03-31). COUNTS: Nx = N mentions (e.g., 570x).
  IMPORTANCE: ★ to ★★★★★ (1-5 scale).
  HALLS: hall_facts, hall_events, hall_discoveries, hall_preferences, hall_advice.
  WINGS: wing_user, wing_agent, wing_team, wing_code, wing_myproject, wing_hardware, wing_ue5, wing_ai_research.
  ROOMS: Hyphenated slugs representing named ideas (e.g., chromadb-setup, gpu-pricing).

EXAMPLE:
  FAM: ALC→♡JOR | 2D(kids): RIL(18,sports) MAX(11,chess+swimming) | BEN(contributor)

Read AAAK naturally — expand codes mentally, treat *markers* as emotional context.
When WRITING AAAK: use entity codes, mark emotions, keep structure tight."#;

/// The Palace Protocol specification string.
pub const PALACE_PROTOCOL: &str = r#"IMPORTANT — MemPalace Memory Protocol:
1. ON WAKE-UP: Load palace overview + AAAK spec.
2. BEFORE RESPONDING about any person, project, or past event: search the palace FIRST. Never guess — verify.
3. IF UNSURE about a fact (name, gender, age, relationship): say "let me check" and query the palace. Wrong is worse than slow.
4. AFTER EACH SESSION: record what happened, what you learned, what matters.
5. WHEN FACTS CHANGE: invalidate the old fact, add the new one.

This protocol ensures the AI KNOWS before it speaks. Storage is not memory — but storage + this protocol = memory."#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compress_basic() {
        let dialect = Dialect::new(None);
        let text = "We decided to use GraphQL instead of REST because it's more flexible.";
        let compressed = dialect.compress(text, None);
        assert!(!compressed.is_empty());
    }

    #[test]
    fn test_compress_with_entities() {
        let mut entities = HashMap::new();
        entities.insert("Alice".to_string(), "ALC".to_string());
        entities.insert("Bob".to_string(), "BOB".to_string());
        let dialect = Dialect::new(Some(entities));
        let text = "Alice and Bob discussed the project architecture.";
        let compressed = dialect.compress(text, None);
        assert!(compressed.contains("ALC") || compressed.contains("BOB"));
    }

    #[test]
    fn test_count_tokens() {
        assert_eq!(Dialect::count_tokens("hello world"), 2); // 2 * 1.3 = 2.6 → 2
    }

    #[test]
    fn test_compression_stats() {
        let original = "This is a longer text with many words that should compress well into AAAK format.";
        let compressed = "0:???|text_words_compress|DECISION";
        let stats = Dialect::compression_stats(original, compressed);
        assert!(stats.size_ratio > 1.0);
    }
}
