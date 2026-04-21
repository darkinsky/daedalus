#[cfg(test)]
mod tests {
    use crate::llm::ChatRole;
    use crate::memory::Memory;
    use crate::memory::sliding_window::{
        SlidingWindowConfig, SlidingWindowMemory, SlidingWindowFactory,
        ConsolidationResult, HistoryEntry, LongTermMemory,
    };

    // ── Basic construction ──

    #[test]
    fn test_default_config() {
        let config = SlidingWindowConfig::default();
        assert_eq!(config.max_messages, None);
        assert_eq!(config.consolidation_threshold, 100);
        assert_eq!(config.retention_window, 50);
    }

    #[test]
    fn test_unlimited_config() {
        let config = SlidingWindowConfig::unlimited();
        assert_eq!(config.max_messages, None);
        assert_eq!(config.consolidation_threshold, usize::MAX);
    }

    #[test]
    fn test_new_memory_empty() {
        let memory = SlidingWindowMemory::with_defaults("System prompt");
        assert_eq!(memory.turn_count(), 0);
        assert_eq!(memory.strategy_name(), "sliding_window");
        assert!(memory.long_term_memory().is_empty());
        assert!(memory.history_log().is_empty());
        assert_eq!(memory.unconsolidated_count(), 0);
        assert!(!memory.should_consolidate());
    }

    // ── Message handling ──

    #[test]
    fn test_add_messages_and_build() {
        let mut memory = SlidingWindowMemory::unlimited("System");
        memory.add_user_message("Hello");
        memory.add_assistant_message("Hi there");

        let messages = memory.build_messages();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, ChatRole::System);
        assert_eq!(messages[0].content, "System");
        assert_eq!(messages[1].content, "Hello");
        assert_eq!(messages[2].content, "Hi there");
    }

    #[test]
    fn test_turn_count() {
        let mut memory = SlidingWindowMemory::unlimited("System");
        assert_eq!(memory.turn_count(), 0);

        memory.add_user_message("Q1");
        memory.add_assistant_message("A1");
        assert_eq!(memory.turn_count(), 1);

        memory.add_user_message("Q2");
        memory.add_assistant_message("A2");
        assert_eq!(memory.turn_count(), 2);
    }

    // ── Windowing ──

    #[test]
    fn test_window_within_limit() {
        let config = SlidingWindowConfig::with_max_messages(10);
        let mut memory = SlidingWindowMemory::new("System", config);
        memory.add_user_message("Q1");
        memory.add_assistant_message("A1");

        let messages = memory.build_messages();
        assert_eq!(messages.len(), 3);
    }

    #[test]
    fn test_window_exceeds_limit() {
        let config = SlidingWindowConfig::with_max_messages(4);
        let mut memory = SlidingWindowMemory::new("System", config);
        for i in 0..5 {
            memory.add_user_message(&format!("Q{}", i));
            memory.add_assistant_message(&format!("A{}", i));
        }

        let messages = memory.build_messages();
        assert_eq!(messages.len(), 5);
        assert_eq!(messages[1].content, "Q3");
        assert_eq!(messages[4].content, "A4");
    }

    #[test]
    fn test_unlimited_keeps_all() {
        let mut memory = SlidingWindowMemory::unlimited("System");
        for i in 0..10 {
            memory.add_user_message(&format!("Q{}", i));
            memory.add_assistant_message(&format!("A{}", i));
        }

        let messages = memory.build_messages();
        assert_eq!(messages.len(), 21);
    }

    // ── Long-term memory ──

    #[test]
    fn test_long_term_memory_injection() {
        let mut memory = SlidingWindowMemory::unlimited("You are helpful.");
        memory.long_term_memory_mut().user_preferences_mut().push("Prefers Rust".to_string());
        memory.add_user_message("Hello");

        let messages = memory.build_messages();
        assert_eq!(messages[0].role, ChatRole::System);
        assert!(messages[0].content.contains("You are helpful."));
        assert!(messages[0].content.contains("Long-Term Memory"));
        assert!(messages[0].content.contains("Prefers Rust"));
    }

    #[test]
    fn test_long_term_memory_empty_no_injection() {
        let memory = SlidingWindowMemory::unlimited("System prompt");
        let messages = memory.build_messages();
        assert_eq!(messages[0].content, "System prompt");
    }

    #[test]
    fn test_long_term_memory_markdown() {
        let mut ltm = LongTermMemory::default();
        ltm.user_preferences_mut().push("Likes concise answers".to_string());
        ltm.project_context_mut().push("Working on Daedalus".to_string());

        let md = ltm.to_markdown().unwrap();
        assert!(md.contains("### User Preferences"));
        assert!(md.contains("- Likes concise answers"));
        assert!(md.contains("### Project Context"));
        assert!(md.contains("- Working on Daedalus"));
    }

    #[test]
    fn test_long_term_memory_empty_markdown() {
        let ltm = LongTermMemory::default();
        assert!(ltm.to_markdown().is_none());
        assert!(ltm.is_empty());
    }

    #[test]
    fn test_long_term_memory_replace() {
        let mut ltm = LongTermMemory::default();
        ltm.user_preferences_mut().push("old fact".to_string());

        let mut new_ltm = LongTermMemory::default();
        new_ltm.user_preferences_mut().push("new fact".to_string());
        new_ltm.important_notes_mut().push("note".to_string());

        ltm.replace_with(new_ltm);
        assert_eq!(ltm.user_preferences(), &["new fact"]);
        assert_eq!(ltm.important_notes(), &["note"]);
    }

    // ── History log ──

    #[test]
    fn test_history_entry_log_line() {
        let entry = HistoryEntry::new(
            "User discussed Rust memory management".to_string(),
            vec!["rust".to_string(), "memory".to_string()],
        );
        let line = entry.to_log_line();
        assert!(line.contains("User discussed Rust memory management"));
        assert!(line.contains("[keywords: rust, memory]"));
    }

    #[test]
    fn test_append_and_search_history() {
        let mut memory = SlidingWindowMemory::unlimited("System");

        memory.append_history_entry(HistoryEntry::new(
            "Discussed Rust ownership model".to_string(),
            vec!["rust".to_string(), "ownership".to_string()],
        ));
        memory.append_history_entry(HistoryEntry::new(
            "Set up Python virtual environment".to_string(),
            vec!["python".to_string(), "venv".to_string()],
        ));

        let results = memory.search_history("rust", None);
        assert_eq!(results.len(), 1);
        assert!(results[0].summary.contains("Rust ownership"));

        let results = memory.search_history("python", None);
        assert_eq!(results.len(), 1);

        let results = memory.search_history("nonexistent", None);
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_search_history_case_insensitive() {
        let mut memory = SlidingWindowMemory::unlimited("System");
        memory.append_history_entry(HistoryEntry::new(
            "Discussed RUST patterns".to_string(),
            vec!["Rust".to_string()],
        ));

        let results = memory.search_history("rust", None);
        assert_eq!(results.len(), 1);

        let results = memory.search_history("RUST", None);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_history_by_keyword() {
        let mut memory = SlidingWindowMemory::unlimited("System");
        memory.append_history_entry(HistoryEntry::new(
            "Some conversation about code".to_string(),
            vec!["refactoring".to_string(), "architecture".to_string()],
        ));

        let results = memory.search_history("refactoring", None);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_history_with_limit() {
        let mut memory = SlidingWindowMemory::unlimited("System");
        for i in 0..5 {
            memory.append_history_entry(HistoryEntry::new(
                format!("Rust discussion part {}", i),
                vec!["rust".to_string()],
            ));
        }

        let results = memory.search_history("rust", Some(2));
        assert_eq!(results.len(), 2);

        let results = memory.search_history("rust", None);
        assert_eq!(results.len(), 5);
    }

    // ── Consolidation tracking ──

    #[test]
    fn test_unconsolidated_count() {
        let mut memory = SlidingWindowMemory::with_defaults("System");
        assert_eq!(memory.unconsolidated_count(), 0);

        memory.add_user_message("Q1");
        memory.add_assistant_message("A1");
        assert_eq!(memory.unconsolidated_count(), 2);

        memory.add_user_message("Q2");
        assert_eq!(memory.unconsolidated_count(), 3);
    }

    #[test]
    fn test_should_consolidate() {
        let config = SlidingWindowConfig {
            max_messages: None,
            consolidation_threshold: 4,
            retention_window: 2,
        };
        let mut memory = SlidingWindowMemory::new("System", config);

        memory.add_user_message("Q1");
        memory.add_assistant_message("A1");
        assert!(!memory.should_consolidate());

        memory.add_user_message("Q2");
        memory.add_assistant_message("A2");
        assert!(memory.should_consolidate());
    }

    #[test]
    fn test_messages_to_consolidate() {
        let config = SlidingWindowConfig {
            max_messages: None,
            consolidation_threshold: 4,
            retention_window: 2,
        };
        let mut memory = SlidingWindowMemory::new("System", config);

        for i in 0..3 {
            memory.add_user_message(&format!("Q{}", i));
            memory.add_assistant_message(&format!("A{}", i));
        }

        let to_consolidate = memory.messages_to_consolidate();
        assert_eq!(to_consolidate.len(), 4);
        assert_eq!(to_consolidate[0].content, "Q0");
        assert_eq!(to_consolidate[3].content, "A1");
    }

    #[test]
    fn test_messages_to_consolidate_nothing() {
        let config = SlidingWindowConfig {
            max_messages: None,
            consolidation_threshold: 100,
            retention_window: 10,
        };
        let mut memory = SlidingWindowMemory::new("System", config);

        memory.add_user_message("Q1");
        memory.add_assistant_message("A1");

        let to_consolidate = memory.messages_to_consolidate();
        assert!(to_consolidate.is_empty());
    }

    #[test]
    fn test_apply_consolidation() {
        let config = SlidingWindowConfig {
            max_messages: None,
            consolidation_threshold: 4,
            retention_window: 2,
        };
        let mut memory = SlidingWindowMemory::new("System", config);

        for i in 0..3 {
            memory.add_user_message(&format!("Q{}", i));
            memory.add_assistant_message(&format!("A{}", i));
        }

        let mut new_ltm = LongTermMemory::default();
        new_ltm.user_preferences_mut().push("Extracted fact".to_string());

        let result = ConsolidationResult {
            history_entry: HistoryEntry::new(
                "User asked 3 questions about topics Q0-Q2".to_string(),
                vec!["questions".to_string()],
            ),
            memory_update: new_ltm,
        };

        memory.apply_consolidation(result, 4);

        assert_eq!(memory.unconsolidated_count(), 2);
        assert_eq!(memory.long_term_memory().user_preferences(), &["Extracted fact"]);
        assert_eq!(memory.history_log().len(), 1);
        assert!(memory.history_log()[0].summary.contains("Q0-Q2"));

        let messages = memory.build_messages();
        assert!(messages[0].content.contains("Extracted fact"));
    }

    #[test]
    fn test_apply_full_archive() {
        let mut memory = SlidingWindowMemory::with_defaults("System");
        memory.add_user_message("Q1");
        memory.add_assistant_message("A1");

        let mut new_ltm = LongTermMemory::default();
        new_ltm.important_notes_mut().push("Archived note".to_string());

        let result = ConsolidationResult {
            history_entry: HistoryEntry::new(
                "Full session archived".to_string(),
                vec!["archive".to_string()],
            ),
            memory_update: new_ltm,
        };

        memory.apply_full_archive(result);

        assert_eq!(memory.turn_count(), 0);
        assert_eq!(memory.long_term_memory().important_notes(), &["Archived note"]);
        assert_eq!(memory.history_log().len(), 1);
    }

    // ── Persistent state migration ──

    #[test]
    fn test_take_and_restore_persistent_state() {
        let mut old_memory = SlidingWindowMemory::with_defaults("Old System");
        old_memory.long_term_memory_mut().user_preferences_mut().push("fact1".to_string());
        old_memory.append_history_entry(HistoryEntry::new(
            "past event".to_string(),
            vec!["event".to_string()],
        ));

        // Use the Memory trait method (returns Option<PersistentState>)
        let state = Memory::take_persistent_state(&mut old_memory)
            .expect("SlidingWindowMemory should produce persistent state");
        assert!(old_memory.long_term_memory().is_empty());
        assert!(old_memory.history_log().is_empty());

        let mut new_memory = SlidingWindowMemory::with_defaults("New System");
        Memory::restore_persistent_state(&mut new_memory, state);

        assert_eq!(new_memory.long_term_memory().user_preferences(), &["fact1"]);
        assert_eq!(new_memory.history_log().len(), 1);

        let messages = new_memory.build_messages();
        assert!(messages[0].content.contains("New System"));
        assert!(messages[0].content.contains("fact1"));
    }

    // ── Clear ──

    #[test]
    fn test_clear() {
        let mut memory = SlidingWindowMemory::with_defaults("System");
        memory.add_user_message("Hello");
        memory.add_assistant_message("Hi");

        memory.clear();
        assert_eq!(memory.turn_count(), 0);
        assert_eq!(memory.unconsolidated_count(), 0);

        let messages = memory.build_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, ChatRole::System);
    }

    #[test]
    fn test_clear_preserves_long_term_memory() {
        let mut memory = SlidingWindowMemory::with_defaults("System");
        memory.long_term_memory_mut().user_preferences_mut().push("fact".to_string());
        memory.append_history_entry(HistoryEntry::new(
            "past event".to_string(),
            vec!["event".to_string()],
        ));

        memory.add_user_message("Hello");
        memory.clear();

        assert!(!memory.long_term_memory().is_empty());
        assert_eq!(memory.history_log().len(), 1);
    }

    // ── Integration: consolidation + windowing ──

    #[test]
    fn test_consolidation_with_windowing() {
        let config = SlidingWindowConfig {
            max_messages: Some(4),
            consolidation_threshold: 6,
            retention_window: 2,
        };
        let mut memory = SlidingWindowMemory::new("System", config);

        for i in 0..4 {
            memory.add_user_message(&format!("Q{}", i));
            memory.add_assistant_message(&format!("A{}", i));
        }

        assert!(memory.should_consolidate());

        let to_consolidate = memory.messages_to_consolidate();
        assert_eq!(to_consolidate.len(), 6);

        let mut new_ltm = LongTermMemory::default();
        new_ltm.project_context_mut().push("Context from consolidation".to_string());

        let result = ConsolidationResult {
            history_entry: HistoryEntry::new(
                "Consolidated 3 turns".to_string(),
                vec!["consolidation".to_string()],
            ),
            memory_update: new_ltm,
        };
        memory.apply_consolidation(result, 6);

        let messages = memory.build_messages();
        assert_eq!(messages.len(), 5);
        assert!(messages[0].content.contains("Context from consolidation"));
        assert_eq!(messages[1].content, "Q2");
        assert_eq!(messages[4].content, "A3");
    }

    // ── Factory ──

    #[test]
    fn test_sliding_window_factory() {
        use crate::memory::MemoryFactory;
        let factory = SlidingWindowFactory::new();
        assert_eq!(factory.strategy_name(), "sliding_window");

        let memory = factory.create_memory("Test prompt");
        assert_eq!(memory.strategy_name(), "sliding_window");
        assert_eq!(memory.turn_count(), 0);
    }

    // ── Consolidation response parsing ──

    #[test]
    fn test_parse_consolidation_response_full() {
        let response = "\
SUMMARY: User discussed Rust project setup and decided to use tokio for async runtime.
KEYWORDS: rust, tokio, async, project setup

MEMORY:
### Project Context
- Using Rust with tokio async runtime
- Project name is daedalus

### User Preferences
- Prefers explicit error handling with anyhow
- Uses 4-space indentation

### Important Decisions
- Chose tokio over async-std for the async runtime";

        let result = SlidingWindowMemory::parse_consolidation_response(response);
        assert!(result.is_some());

        let result = result.unwrap();
        assert!(result.history_entry.summary.contains("tokio"));
        assert_eq!(result.history_entry.keywords.len(), 4);
        assert!(result.history_entry.keywords.contains(&"rust".to_string()));
        assert!(result.history_entry.keywords.contains(&"tokio".to_string()));

        assert_eq!(result.memory_update.section("Project Context").len(), 2);
        assert_eq!(result.memory_update.section("User Preferences").len(), 2);
        assert_eq!(result.memory_update.section("Important Decisions").len(), 1);
    }

    #[test]
    fn test_parse_consolidation_response_no_memory() {
        let response = "\
SUMMARY: Brief chat about nothing important.
KEYWORDS: chat, casual

MEMORY:";

        let result = SlidingWindowMemory::parse_consolidation_response(response);
        assert!(result.is_some());

        let result = result.unwrap();
        assert!(result.history_entry.summary.contains("nothing important"));
        assert!(result.memory_update.is_empty());
    }

    #[test]
    fn test_parse_consolidation_response_missing_summary() {
        let response = "\
KEYWORDS: something

MEMORY:
### Notes
- A note";

        let result = SlidingWindowMemory::parse_consolidation_response(response);
        assert!(result.is_none(), "Should return None when SUMMARY is missing");
    }

    #[test]
    fn test_parse_consolidation_response_no_keywords() {
        let response = "\
SUMMARY: User asked about memory consolidation.

MEMORY:
### Important Notes
- Consolidation is triggered automatically";

        let result = SlidingWindowMemory::parse_consolidation_response(response);
        assert!(result.is_some());

        let result = result.unwrap();
        assert!(result.history_entry.keywords.is_empty());
        assert_eq!(result.memory_update.section("Important Notes").len(), 1);
    }
}
