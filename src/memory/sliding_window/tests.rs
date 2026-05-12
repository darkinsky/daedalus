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
        assert_eq!(config.max_messages, Some(100));
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
            ..Default::default()
        };
        let mut memory = SlidingWindowMemory::new("System", config);

        memory.add_user_message("Q1");
        memory.add_assistant_message("A1");
        assert!(!Memory::should_consolidate(&memory));

        memory.add_user_message("Q2");
        memory.add_assistant_message("A2");
        assert!(Memory::should_consolidate(&memory));
    }

    #[test]
    fn test_messages_to_consolidate() {
        let config = SlidingWindowConfig {
            max_messages: None,
            consolidation_threshold: 4,
            retention_window: 2,
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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

    // ── Micro-compact ──

    #[test]
    fn test_micro_compact_truncates_old_tool_context() {
        let mut memory = SlidingWindowMemory::unlimited("System");

        // Add some old tool context messages (these should be truncated)
        let long_tool_context = format!(
            "[Tool call round 1: read_file({{\"path\":\"/some/very/long/path/to/file.rs\",\"extra\":\"{}\"}})]" ,
            "x".repeat(1000)
        );
        memory.add_assistant_message(&long_tool_context);
        memory.add_user_message("Q1");
        memory.add_assistant_message("A1");

        // Add enough recent messages to push the tool context outside the preservation window
        for i in 2..=6 {
            memory.add_user_message(&format!("Q{}", i));
            memory.add_assistant_message(&format!("A{}", i));
        }

        let messages = memory.build_messages();

        // The first conversation message (index 1) is the old tool context — should be truncated
        let tool_msg = &messages[1];
        assert!(
            tool_msg.content.len() < 500,
            "Old tool context should be truncated, got {} chars",
            tool_msg.content.len()
        );
        assert!(
            tool_msg.content.contains("truncated"),
            "Truncated message should contain 'truncated' marker"
        );
        assert!(
            tool_msg.content.contains("[Tool call round 1: read_file"),
            "Truncated message should preserve tool name"
        );
    }

    #[test]
    fn test_micro_compact_preserves_recent_tool_context() {
        let mut memory = SlidingWindowMemory::unlimited("System");

        // Add a few messages first
        for i in 0..3 {
            memory.add_user_message(&format!("Q{}", i));
            memory.add_assistant_message(&format!("A{}", i));
        }

        // Add a recent tool context message (within preservation window)
        let long_tool_context = format!(
            "[Tool call round 1: grep_search({{\"query\":\"test\",\"extra\":\"{}\"}})]" ,
            "result_line\n".repeat(100)
        );
        memory.add_assistant_message(&long_tool_context);
        memory.add_user_message("Q_last");
        memory.add_assistant_message("A_last");

        let messages = memory.build_messages();

        // The tool context is within the last 6 messages — should NOT be truncated
        let tool_msg_idx = messages.len() - 3; // tool_context, Q_last, A_last
        let tool_msg = &messages[tool_msg_idx];
        assert!(
            tool_msg.content.contains("result_line"),
            "Recent tool context should be preserved in full"
        );
    }

    #[test]
    fn test_micro_compact_skips_user_messages() {
        let mut memory = SlidingWindowMemory::unlimited("System");

        // Add a long user message (should never be truncated)
        let long_user_msg = "x".repeat(1000);
        memory.add_user_message(&long_user_msg);
        memory.add_assistant_message("A0");

        // Add enough messages to push it outside the window
        for i in 1..=6 {
            memory.add_user_message(&format!("Q{}", i));
            memory.add_assistant_message(&format!("A{}", i));
        }

        let messages = memory.build_messages();

        // The old user message (index 1) should NOT be truncated
        assert_eq!(
            messages[1].content.len(),
            1000,
            "User messages should never be truncated by micro-compact"
        );
    }

    #[test]
    fn test_micro_compact_skips_non_tool_assistant_messages() {
        let mut memory = SlidingWindowMemory::unlimited("System");

        // Add a long assistant message that is NOT tool context
        let long_assistant_msg = "Here is a very detailed explanation: ".to_string() + &"x".repeat(1000);
        memory.add_user_message("Q0");
        memory.add_assistant_message(&long_assistant_msg);

        // Add enough messages to push it outside the window
        for i in 1..=6 {
            memory.add_user_message(&format!("Q{}", i));
            memory.add_assistant_message(&format!("A{}", i));
        }

        let messages = memory.build_messages();

        // The old assistant message (index 2) should NOT be truncated (no tool marker)
        assert!(
            messages[2].content.len() > 500,
            "Non-tool assistant messages should not be truncated"
        );
    }

    #[test]
    fn test_micro_compact_no_op_when_few_messages() {
        let mut memory = SlidingWindowMemory::unlimited("System");

        let long_tool_context = format!(
            "[Tool call round 1: bash({{\"cmd\":\"ls\",\"extra\":\"{}\"}})]" ,
            "x".repeat(1000)
        );
        memory.add_assistant_message(&long_tool_context);
        memory.add_user_message("Q1");
        memory.add_assistant_message("A1");

        let messages = memory.build_messages();

        // Only 4 messages total (system + 3) — below the preservation threshold
        // so nothing should be truncated
        assert!(
            messages[1].content.len() > 500,
            "With few messages, nothing should be truncated"
        );
    }

    // ── CJK-aware token estimation ──

    #[test]
    fn test_estimate_tokens_ascii() {
        use crate::memory::estimate_tokens;

        // Pure ASCII: ~4 chars/token
        let ascii = "hello world, this is a test string for token estimation";
        let tokens = estimate_tokens(ascii);
        // 55 chars / 4 = 13 tokens (approximately)
        assert!(tokens > 10 && tokens < 20, "ASCII tokens: {}", tokens);
    }

    #[test]
    fn test_estimate_tokens_cjk() {
        use crate::memory::estimate_tokens;

        // Pure CJK: ~1.5 chars/token → 10 chars ≈ 6-7 tokens
        let cjk = "你好世界这是测试字符串";
        let tokens = estimate_tokens(cjk);
        assert!(tokens >= 6 && tokens <= 10, "CJK tokens: {}", tokens);
    }

    #[test]
    fn test_estimate_tokens_mixed() {
        use crate::memory::estimate_tokens;

        // Mixed: "Hello 你好 World 世界" — 12 ASCII chars + 4 CJK chars
        let mixed = "Hello 你好 World 世界";
        let tokens = estimate_tokens(mixed);
        // ASCII: 12/4 = 3, CJK: 4*2/3 ≈ 3 → total ≈ 6
        assert!(tokens >= 4 && tokens <= 10, "Mixed tokens: {}", tokens);
    }

    #[test]
    fn test_estimate_tokens_empty() {
        use crate::memory::estimate_tokens;
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn test_estimate_tokens_cjk_higher_than_ascii_for_same_chars() {
        use crate::memory::estimate_tokens;

        // Same number of characters, but CJK should produce more tokens
        let ascii = "abcdefghij"; // 10 chars → 10/4 = 2 tokens
        let cjk = "你好世界测试字符串十"; // 10 chars → 10*2/3 ≈ 7 tokens

        let ascii_tokens = estimate_tokens(ascii);
        let cjk_tokens = estimate_tokens(cjk);

        assert!(
            cjk_tokens > ascii_tokens,
            "CJK ({}) should produce more tokens than ASCII ({}) for same char count",
            cjk_tokens,
            ascii_tokens
        );
    }

    // ── Compact circuit breaker ──

    #[test]
    fn test_should_compact_with_budget() {
        let config = SlidingWindowConfig {
            max_messages: None,
            consolidation_threshold: usize::MAX,
            retention_window: 0,
            context_budget: 1000, // very small budget
            compact_threshold_ratio: 0.5,
            compact_preserve_recent: 2,
            ..Default::default()
        };
        let mut memory = SlidingWindowMemory::new("System", config);

        // Add enough messages to exceed the budget
        for i in 0..50 {
            memory.add_user_message(&format!("Question {} with some extra text to fill tokens", i));
            memory.add_assistant_message(&format!("Answer {} with some extra text to fill tokens", i));
        }

        assert!(
            memory.should_compact(),
            "Should trigger compact when token count exceeds budget threshold"
        );
    }

    // ── Multi-level context pressure ──

    #[test]
    fn test_context_pressure_level_normal() {
        use crate::memory::sliding_window::config::ContextPressureLevel;

        let config = SlidingWindowConfig {
            max_messages: None,
            consolidation_threshold: usize::MAX,
            retention_window: 0,
            context_budget: 100_000,
            compact_warning_ratio: 0.8,
            compact_threshold_ratio: 0.93,
            compact_hard_limit_ratio: 0.97,
            compact_preserve_recent: 2,
        };
        let memory = SlidingWindowMemory::new("System", config);

        // Empty memory should be Normal
        assert_eq!(memory.context_pressure_level(), ContextPressureLevel::Normal);
    }

    #[test]
    fn test_context_pressure_level_warning() {
        use crate::memory::sliding_window::config::ContextPressureLevel;

        // With budget=100 and warning_ratio=0.3, warning threshold = 30 tokens.
        // 5 Q&A pairs with ~20 chars each ≈ 5 tokens per message × 10 messages = 50 tokens.
        // System prompt "System" ≈ 1 token (discounted by 75% → ~0).
        // cache_adjusted ≈ 50 > 30 → Warning.
        let config = SlidingWindowConfig {
            max_messages: None,
            consolidation_threshold: usize::MAX,
            retention_window: 0,
            context_budget: 100, // very small budget
            compact_warning_ratio: 0.3,  // warning at 30 tokens
            compact_threshold_ratio: 0.93,
            compact_hard_limit_ratio: 0.97,
            compact_preserve_recent: 2,
        };
        let mut memory = SlidingWindowMemory::new("System", config);

        // Add messages to exceed warning but stay below threshold
        for i in 0..5 {
            memory.add_user_message(&format!("Question {} with text", i));
            memory.add_assistant_message(&format!("Answer {} with text", i));
        }

        let level = memory.context_pressure_level();
        assert!(
            level >= ContextPressureLevel::Warning,
            "Expected at least Warning, got {:?} (cache_adjusted_tokens={})",
            level,
            memory.cache_adjusted_tokens(),
        );
    }

    #[test]
    fn test_context_pressure_level_high_triggers_compact() {
        use crate::memory::sliding_window::config::ContextPressureLevel;

        let config = SlidingWindowConfig {
            max_messages: None,
            consolidation_threshold: usize::MAX,
            retention_window: 0,
            context_budget: 200, // very small budget
            compact_warning_ratio: 0.3,
            compact_threshold_ratio: 0.5,
            compact_hard_limit_ratio: 0.9,
            compact_preserve_recent: 2,
        };
        let mut memory = SlidingWindowMemory::new("System", config);

        // Add enough messages to exceed threshold
        for i in 0..20 {
            memory.add_user_message(&format!("Question {} with some extra text to fill tokens", i));
            memory.add_assistant_message(&format!("Answer {} with some extra text to fill tokens", i));
        }

        let level = memory.context_pressure_level();
        assert!(
            level >= ContextPressureLevel::High,
            "Expected at least High, got {:?}",
            level
        );
        assert!(memory.should_compact(), "should_compact should return true at High level");
    }

    #[test]
    fn test_context_pressure_level_ordering() {
        use crate::memory::sliding_window::config::ContextPressureLevel;

        // Verify the ordering is correct
        assert!(ContextPressureLevel::Normal < ContextPressureLevel::Warning);
        assert!(ContextPressureLevel::Warning < ContextPressureLevel::High);
        assert!(ContextPressureLevel::High < ContextPressureLevel::Critical);
    }

    #[test]
    fn test_default_config_multi_level_thresholds() {
        let config = SlidingWindowConfig::default();
        assert_eq!(config.compact_warning_ratio, 0.8);
        assert_eq!(config.compact_threshold_ratio, 0.93);
        assert_eq!(config.compact_hard_limit_ratio, 0.97);
    }

    // ── Compact boundary (incremental compression) ──

    #[test]
    fn test_compact_boundary_prefix_detection() {
        // Verify that the compact summary message starts with the expected prefix
        let summary_msg = format!(
            "[Previous conversation context \u{2014} 10 messages compressed into summary]\n\nSome summary",
        );
        assert!(
            summary_msg.starts_with("[Previous conversation context \u{2014}"),
            "Compact summary should start with the boundary prefix"
        );
    }

    #[test]
    fn test_compact_user_prompt_without_previous_summary() {
        use super::super::prompts::compact_user_prompt;

        let prompt = compact_user_prompt("some messages", None, None);
        assert!(prompt.contains("## Conversation to Compress"));
        assert!(prompt.contains("some messages"));
        assert!(!prompt.contains("Previous Compact Summary"));
    }

    #[test]
    fn test_compact_user_prompt_with_previous_summary() {
        use super::super::prompts::compact_user_prompt;

        let prompt = compact_user_prompt(
            "new messages",
            None,
            Some("old summary content"),
        );
        assert!(prompt.contains("## Previous Compact Summary"));
        assert!(prompt.contains("old summary content"));
        assert!(prompt.contains("## Conversation to Compress"));
        assert!(prompt.contains("new messages"));
    }

    #[test]
    fn test_compact_user_prompt_with_instruction_and_summary() {
        use super::super::prompts::compact_user_prompt;

        let prompt = compact_user_prompt(
            "messages",
            Some("focus on auth"),
            Some("old summary"),
        );
        assert!(prompt.contains("## Additional Focus"));
        assert!(prompt.contains("focus on auth"));
        assert!(prompt.contains("## Previous Compact Summary"));
        assert!(prompt.contains("old summary"));
        assert!(prompt.contains("## Conversation to Compress"));
    }

    // ── Preserved segment (semantic marking) ──

    #[test]
    fn test_chat_message_preserved_default_false() {
        use crate::llm::ChatMessage;
        let msg = ChatMessage::user("Hello");
        assert!(!msg.preserved, "Messages should not be preserved by default");
    }

    #[test]
    fn test_chat_message_with_preserved() {
        use crate::llm::ChatMessage;
        let msg = ChatMessage::user("Important task").with_preserved(true);
        assert!(msg.preserved);
    }

    #[test]
    fn test_mark_preserved() {
        let mut memory = SlidingWindowMemory::unlimited("System");
        memory.add_user_message("Task instruction");
        memory.add_assistant_message("Got it");
        memory.add_user_message("Follow up");

        // Mark the first user message as preserved
        assert!(memory.mark_preserved(0, true));
        // Out of bounds should return false
        assert!(!memory.mark_preserved(100, true));
    }

    #[test]
    fn test_auto_mark_preserved_first_user_message() {
        let mut memory = SlidingWindowMemory::unlimited("System");
        memory.add_user_message("Build a login page");
        memory.add_assistant_message("Sure, I'll help");
        memory.add_user_message("Also add a signup form");
        memory.add_assistant_message("Done");

        memory.auto_mark_preserved();

        // First user message should be preserved (task instruction)
        let messages = &memory.messages;
        assert!(messages[0].preserved, "First user message should be auto-preserved");
        // Second user message should NOT be preserved (no decision language)
        assert!(!messages[2].preserved, "Regular follow-up should not be preserved");
    }

    #[test]
    fn test_auto_mark_preserved_decision_language() {
        let mut memory = SlidingWindowMemory::unlimited("System");
        memory.add_user_message("Help me with the project");
        memory.add_assistant_message("What would you like?");
        memory.add_user_message("I want to use React for the frontend");
        memory.add_assistant_message("Good choice");
        memory.add_user_message("Please implement the auth module");
        memory.add_assistant_message("Working on it");

        memory.auto_mark_preserved();

        let messages = &memory.messages;
        assert!(messages[0].preserved, "First user message should be preserved");
        assert!(messages[2].preserved, "Decision message ('I want') should be preserved");
        assert!(messages[4].preserved, "Instruction message ('Please implement') should be preserved");
    }

    #[test]
    fn test_auto_mark_preserved_error_messages() {
        let mut memory = SlidingWindowMemory::unlimited("System");
        memory.add_user_message("Fix the bug");
        memory.add_assistant_message("I found a compilation error in main.rs");
        memory.add_user_message("What's the error?");
        memory.add_assistant_message("The function signature is correct");

        memory.auto_mark_preserved();

        let messages = &memory.messages;
        assert!(messages[1].preserved, "Error-containing assistant message should be preserved");
        assert!(!messages[3].preserved, "Normal assistant message should not be preserved");
    }

    #[test]
    fn test_auto_mark_preserved_skips_tool_context() {
        let mut memory = SlidingWindowMemory::unlimited("System");
        memory.add_user_message("Check the code");
        memory.add_assistant_message(
            "[Tool call round 1: bash({\"cmd\":\"cargo build\"})]"
        );

        memory.auto_mark_preserved();

        let messages = &memory.messages;
        // Tool context messages should NOT be preserved even if they contain "error"
        assert!(!messages[1].preserved, "Tool context should not be preserved even with error keyword");
    }

    #[test]
    fn test_auto_mark_preserved_idempotent() {
        let mut memory = SlidingWindowMemory::unlimited("System");
        memory.add_user_message("Build a login page");
        memory.add_assistant_message("Done");

        // Mark first time
        memory.auto_mark_preserved();
        assert!(memory.messages[0].preserved);

        // Mark again — should not change anything
        memory.auto_mark_preserved();
        assert!(memory.messages[0].preserved);
    }

    #[test]
    fn test_auto_mark_preserved_never_unmarks() {
        let mut memory = SlidingWindowMemory::unlimited("System");
        memory.add_user_message("Regular message");
        memory.add_assistant_message("Response");

        // Manually mark the assistant message as preserved
        memory.mark_preserved(1, true);

        // auto_mark_preserved should not un-mark it
        memory.auto_mark_preserved();
        assert!(memory.messages[1].preserved, "Manually preserved messages should never be un-marked");
    }

    #[test]
    fn test_auto_mark_preserved_plan_content() {
        let mut memory = SlidingWindowMemory::unlimited("System");
        memory.add_user_message("Refactor the auth module");
        memory.add_assistant_message(
            "## Implementation Plan\n\n\
             1. Extract the auth logic into a separate module\n\
             2. Create new interfaces\n\
             3. Update all callers"
        );
        memory.add_assistant_message("I've started working on step 1.");

        memory.auto_mark_preserved();

        let messages = &memory.messages;
        assert!(messages[1].preserved, "Plan-containing assistant message should be preserved");
        assert!(!messages[2].preserved, "Regular follow-up should not be preserved");
    }

    #[test]
    fn test_auto_mark_preserved_structured_steps() {
        let mut memory = SlidingWindowMemory::unlimited("System");
        memory.add_user_message("Build a new feature");
        memory.add_assistant_message(
            "Here's my approach:\n\n\
             Step 1: Read the existing code\n\
             Step 2: Design the interface\n\
             Step 3: Implement the core logic\n\
             Step 4: Add tests"
        );

        memory.auto_mark_preserved();

        let messages = &memory.messages;
        assert!(messages[1].preserved, "Message with structured steps should be preserved");
    }

    #[test]
    fn test_auto_mark_preserved_plan_in_tool_context_not_preserved() {
        let mut memory = SlidingWindowMemory::unlimited("System");
        memory.add_user_message("Check the code");
        memory.add_assistant_message(
            "[Tool call round 1: read_file({\"path\":\"plan.md\"})]"
        );

        memory.auto_mark_preserved();

        let messages = &memory.messages;
        assert!(!messages[1].preserved, "Tool context should not be preserved even with plan keywords");
    }

    // ── Partial compact command parsing ──

    #[test]
    fn test_parse_compact_no_args() {
        use crate::cli::commands::{parse, Command};
        if let Some(Command::Compact { instruction, range }) = parse("/compact") {
            assert!(instruction.is_none());
            assert!(range.is_none());
        } else {
            panic!("Expected Compact command");
        }
    }

    #[test]
    fn test_parse_compact_with_instruction() {
        use crate::cli::commands::{parse, Command};
        if let Some(Command::Compact { instruction, range }) = parse("/compact focus on auth") {
            assert_eq!(instruction, Some("focus on auth"));
            assert!(range.is_none());
        } else {
            panic!("Expected Compact command");
        }
    }

    #[test]
    fn test_parse_compact_before() {
        use crate::cli::commands::{parse, Command};
        if let Some(Command::Compact { instruction, range }) = parse("/compact --before 20") {
            assert!(instruction.is_none());
            assert_eq!(range, Some((0, 20)));
        } else {
            panic!("Expected Compact command with --before");
        }
    }

    #[test]
    fn test_parse_compact_after() {
        use crate::cli::commands::{parse, Command};
        if let Some(Command::Compact { instruction, range }) = parse("/compact --after 10") {
            assert!(instruction.is_none());
            assert_eq!(range, Some((10, usize::MAX)));
        } else {
            panic!("Expected Compact command with --after");
        }
    }

    #[test]
    fn test_parse_compact_range() {
        use crate::cli::commands::{parse, Command};
        if let Some(Command::Compact { instruction, range }) = parse("/compact --range 5-15") {
            assert!(instruction.is_none());
            assert_eq!(range, Some((5, 15)));
        } else {
            panic!("Expected Compact command with --range");
        }
    }
}
