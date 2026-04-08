use crate::llm::TokenUsage;

/// Tracks cumulative token usage across a session.
pub struct SessionCost {
    prompt_tokens: u64,
    completion_tokens: u64,
    requests: u64,
}

impl SessionCost {
    pub fn new() -> Self {
        Self {
            prompt_tokens: 0,
            completion_tokens: 0,
            requests: 0,
        }
    }

    /// Record token usage from a single request.
    ///
    /// Accepts an optional `&TokenUsage` directly, avoiding the need for
    /// callers to manually extract and unwrap individual token fields.
    pub fn add_usage(&mut self, usage: Option<&TokenUsage>) {
        if let Some(u) = usage {
            self.prompt_tokens += u.prompt_tokens.unwrap_or(0);
            self.completion_tokens += u.completion_tokens.unwrap_or(0);
        }
        self.requests += 1;
    }

    /// Reset all counters (e.g. on `/new`).
    pub fn reset(&mut self) {
        self.prompt_tokens = 0;
        self.completion_tokens = 0;
        self.requests = 0;
    }

    pub fn prompt_tokens(&self) -> u64 {
        self.prompt_tokens
    }

    pub fn completion_tokens(&self) -> u64 {
        self.completion_tokens
    }

    pub fn total_tokens(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }

    pub fn requests(&self) -> u64 {
        self.requests
    }
}
