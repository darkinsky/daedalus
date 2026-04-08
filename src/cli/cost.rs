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
    pub fn add(&mut self, prompt: u64, completion: u64) {
        self.prompt_tokens += prompt;
        self.completion_tokens += completion;
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
