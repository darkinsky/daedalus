use crate::llm::TokenUsage;

/// Tracks cumulative token usage across a session.
pub struct SessionCost {
    prompt_tokens: u64,
    completion_tokens: u64,
    requests: u64,
    /// Token usage from subagent executions (tracked separately).
    subagent_prompt_tokens: u64,
    subagent_completion_tokens: u64,
    subagent_invocations: u64,
}

impl SessionCost {
    pub fn new() -> Self {
        Self {
            prompt_tokens: 0,
            completion_tokens: 0,
            requests: 0,
            subagent_prompt_tokens: 0,
            subagent_completion_tokens: 0,
            subagent_invocations: 0,
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

    /// Record token usage from a subagent execution.
    pub fn add_subagent_usage(&mut self, usage: Option<&TokenUsage>) {
        if let Some(u) = usage {
            self.subagent_prompt_tokens += u.prompt_tokens.unwrap_or(0);
            self.subagent_completion_tokens += u.completion_tokens.unwrap_or(0);
        }
        self.subagent_invocations += 1;
    }

    /// Reset all counters (e.g. on `/new`).
    pub fn reset(&mut self) {
        self.prompt_tokens = 0;
        self.completion_tokens = 0;
        self.requests = 0;
        self.subagent_prompt_tokens = 0;
        self.subagent_completion_tokens = 0;
        self.subagent_invocations = 0;
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

    pub fn subagent_prompt_tokens(&self) -> u64 {
        self.subagent_prompt_tokens
    }

    pub fn subagent_completion_tokens(&self) -> u64 {
        self.subagent_completion_tokens
    }

    pub fn subagent_total_tokens(&self) -> u64 {
        self.subagent_prompt_tokens + self.subagent_completion_tokens
    }

    pub fn subagent_invocations(&self) -> u64 {
        self.subagent_invocations
    }

    /// Grand total tokens (lead agent + all subagents).
    pub fn grand_total_tokens(&self) -> u64 {
        self.total_tokens() + self.subagent_total_tokens()
    }
}
