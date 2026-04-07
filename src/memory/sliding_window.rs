use crate::llm::ChatMessage;

use super::Memory;

/// Sliding window memory — keeps the last N turns of conversation history.
///
/// When `max_turns` is `None`, the window is unlimited and all history is kept
/// (equivalent to full history mode). When `max_turns` is `Some(n)`, only the
/// most recent `n` turns (each turn = 1 user message + 1 assistant message)
/// are included when building messages for the LLM.
///
/// The system prompt is always included regardless of the window size.
/// All messages are stored internally; the window only affects what is sent
/// to the LLM via `build_messages()`.
pub struct SlidingWindowMemory {
    /// The system prompt message (always first in the message list).
    system_message: ChatMessage,
    /// All conversation messages (user + assistant), in chronological order.
    history: Vec<ChatMessage>,
    /// Maximum number of turns to include. `None` means unlimited (full history).
    max_turns: Option<usize>,
}

impl SlidingWindowMemory {
    /// Create a new sliding window memory.
    ///
    /// # Arguments
    /// * `system_prompt` - The system prompt to prepend to every request.
    /// * `max_turns` - Maximum turns to keep in the window.
    ///   - `None` = unlimited (full history, all messages sent every time).
    ///   - `Some(n)` = keep only the last `n` turns (2*n messages).
    pub fn new(system_prompt: &str, max_turns: Option<usize>) -> Self {
        Self {
            system_message: ChatMessage::system(system_prompt),
            history: Vec::new(),
            max_turns,
        }
    }

    /// Create a new memory with unlimited window (full history mode).
    pub fn unlimited(system_prompt: &str) -> Self {
        Self::new(system_prompt, None)
    }

    /// Return the configured max turns (None = unlimited).
    #[allow(dead_code)]
    pub fn max_turns(&self) -> Option<usize> {
        self.max_turns
    }
}

impl Memory for SlidingWindowMemory {
    fn add_user_message(&mut self, content: &str) {
        self.history.push(ChatMessage::user(content));
    }

    fn add_assistant_message(&mut self, content: &str) {
        self.history.push(ChatMessage::assistant(content));
    }

    fn build_messages(&self) -> Vec<ChatMessage> {
        let window = match self.max_turns {
            None => &self.history[..],
            Some(n) => {
                // Each turn = 2 messages (user + assistant), but the last
                // user message may not have an assistant reply yet.
                let max_messages = n * 2;
                if self.history.len() <= max_messages {
                    &self.history[..]
                } else {
                    &self.history[self.history.len() - max_messages..]
                }
            }
        };

        let mut messages = Vec::with_capacity(1 + window.len());
        messages.push(self.system_message.clone());
        messages.extend(window.iter().cloned());
        messages
    }

    fn clear(&mut self) {
        self.history.clear();
    }

    fn turn_count(&self) -> usize {
        self.history.len() / 2
    }

    fn strategy_name(&self) -> &str {
        match self.max_turns {
            None => "sliding_window(unlimited)",
            Some(_) => "sliding_window",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ChatRole;

    // ── Unlimited (full history) mode ──

    #[test]
    fn test_unlimited_no_history() {
        let memory = SlidingWindowMemory::unlimited("You are helpful");
        assert_eq!(memory.turn_count(), 0);
        assert_eq!(memory.max_turns(), None);
        assert_eq!(memory.strategy_name(), "sliding_window(unlimited)");
    }

    #[test]
    fn test_unlimited_build_messages_system_only() {
        let memory = SlidingWindowMemory::unlimited("System prompt");
        let messages = memory.build_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, ChatRole::System);
        assert_eq!(messages[0].content, "System prompt");
    }

    #[test]
    fn test_unlimited_keeps_all_history() {
        let mut memory = SlidingWindowMemory::unlimited("System");
        for i in 0..10 {
            memory.add_user_message(&format!("Q{}", i));
            memory.add_assistant_message(&format!("A{}", i));
        }
        assert_eq!(memory.turn_count(), 10);

        let messages = memory.build_messages();
        // system + 10 user + 10 assistant = 21
        assert_eq!(messages.len(), 21);
        assert_eq!(messages[1].content, "Q0");
        assert_eq!(messages[20].content, "A9");
    }

    // ── Sliding window mode ──

    #[test]
    fn test_window_strategy_name() {
        let memory = SlidingWindowMemory::new("System", Some(3));
        assert_eq!(memory.strategy_name(), "sliding_window");
        assert_eq!(memory.max_turns(), Some(3));
    }

    #[test]
    fn test_window_within_limit() {
        let mut memory = SlidingWindowMemory::new("System", Some(5));
        memory.add_user_message("Q1");
        memory.add_assistant_message("A1");
        memory.add_user_message("Q2");
        memory.add_assistant_message("A2");

        // 2 turns < 5 window, so all messages should be included
        let messages = memory.build_messages();
        assert_eq!(messages.len(), 5); // system + 4 history
        assert_eq!(messages[1].content, "Q1");
        assert_eq!(messages[4].content, "A2");
    }

    #[test]
    fn test_window_exceeds_limit() {
        let mut memory = SlidingWindowMemory::new("System", Some(2));
        memory.add_user_message("Q1");
        memory.add_assistant_message("A1");
        memory.add_user_message("Q2");
        memory.add_assistant_message("A2");
        memory.add_user_message("Q3");
        memory.add_assistant_message("A3");

        // 3 turns, window = 2, so only last 2 turns (4 messages) + system
        let messages = memory.build_messages();
        assert_eq!(messages.len(), 5); // system + 4
        assert_eq!(messages[0].role, ChatRole::System);
        assert_eq!(messages[1].content, "Q2"); // Q1/A1 dropped
        assert_eq!(messages[2].content, "A2");
        assert_eq!(messages[3].content, "Q3");
        assert_eq!(messages[4].content, "A3");
    }

    #[test]
    fn test_window_with_pending_user_message() {
        // User sent a message but assistant hasn't replied yet
        let mut memory = SlidingWindowMemory::new("System", Some(2));
        memory.add_user_message("Q1");
        memory.add_assistant_message("A1");
        memory.add_user_message("Q2");
        memory.add_assistant_message("A2");
        memory.add_user_message("Q3"); // no assistant reply yet

        // Window = 2 turns = 4 messages, history has 5 messages
        // Should take last 4: A1, Q2, A2, Q3
        let messages = memory.build_messages();
        assert_eq!(messages.len(), 5); // system + 4
        assert_eq!(messages[1].content, "A1");
        assert_eq!(messages[4].content, "Q3");
    }

    #[test]
    fn test_window_size_one() {
        let mut memory = SlidingWindowMemory::new("System", Some(1));
        memory.add_user_message("Q1");
        memory.add_assistant_message("A1");
        memory.add_user_message("Q2");
        memory.add_assistant_message("A2");

        // Window = 1 turn = 2 messages, only last turn
        let messages = memory.build_messages();
        assert_eq!(messages.len(), 3); // system + 2
        assert_eq!(messages[1].content, "Q2");
        assert_eq!(messages[2].content, "A2");
    }

    // ── Common operations ──

    #[test]
    fn test_turn_count() {
        let mut memory = SlidingWindowMemory::new("System", Some(3));
        assert_eq!(memory.turn_count(), 0);

        memory.add_user_message("Q1");
        memory.add_assistant_message("A1");
        assert_eq!(memory.turn_count(), 1);

        memory.add_user_message("Q2");
        memory.add_assistant_message("A2");
        assert_eq!(memory.turn_count(), 2);
    }

    #[test]
    fn test_clear() {
        let mut memory = SlidingWindowMemory::new("System", Some(3));
        memory.add_user_message("Hello");
        memory.add_assistant_message("Hi");
        assert_eq!(memory.turn_count(), 1);

        memory.clear();
        assert_eq!(memory.turn_count(), 0);

        let messages = memory.build_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, ChatRole::System);
    }
}
