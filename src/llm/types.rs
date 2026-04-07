/// A single message in a conversation.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

/// The role of a message sender.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

impl std::fmt::Display for ChatRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChatRole::System => write!(f, "system"),
            ChatRole::User => write!(f, "user"),
            ChatRole::Assistant => write!(f, "assistant"),
        }
    }
}

impl std::fmt::Display for ChatMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.role, self.content)
    }
}

/// Format a slice of ChatMessages into a JSON string for logging.
pub fn format_messages_for_log(messages: &[ChatMessage]) -> String {
    let entries: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| {
            serde_json::json!({
                "role": m.role.to_string(),
                "content": m.content,
            })
        })
        .collect();
    serde_json::to_string(&entries).unwrap_or_else(|_| "[]".to_string())
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: ChatRole::System, content: content.into() }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self { role: ChatRole::User, content: content.into() }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: ChatRole::Assistant, content: content.into() }
    }
}

/// Response from an LLM chat completion.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    /// The text content of the response.
    pub content: String,
    /// Token usage information (if available).
    pub usage: Option<TokenUsage>,
}

/// Token usage statistics.
#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

/// Options for chat completion requests.
#[derive(Debug, Clone, Default)]
pub struct ChatOptions {
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    pub top_p: Option<f64>,
}

/// Configuration for an LLM provider.
#[derive(Debug, Clone)]
pub struct LlmConfig {
    /// API key for authentication.
    pub api_key: String,
    /// Model identifier (e.g., "gpt-4o", "claude-sonnet-4-6").
    pub model: String,
    /// Optional custom API base URL.
    pub api_base: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chat_message_constructors() {
        let sys = ChatMessage::system("You are helpful");
        assert_eq!(sys.role, ChatRole::System);
        assert_eq!(sys.content, "You are helpful");

        let user = ChatMessage::user("Hello");
        assert_eq!(user.role, ChatRole::User);
        assert_eq!(user.content, "Hello");

        let asst = ChatMessage::assistant("Hi there");
        assert_eq!(asst.role, ChatRole::Assistant);
        assert_eq!(asst.content, "Hi there");
    }

    #[test]
    fn test_chat_message_accepts_string() {
        let msg = ChatMessage::user(String::from("owned string"));
        assert_eq!(msg.content, "owned string");
    }

    #[test]
    fn test_chat_role_equality() {
        assert_eq!(ChatRole::User, ChatRole::User);
        assert_ne!(ChatRole::User, ChatRole::Assistant);
        assert_ne!(ChatRole::System, ChatRole::User);
    }

    #[test]
    fn test_chat_options_default() {
        let opts = ChatOptions::default();
        assert!(opts.temperature.is_none());
        assert!(opts.max_tokens.is_none());
        assert!(opts.top_p.is_none());
    }

    #[test]
    fn test_token_usage_default() {
        let usage = TokenUsage::default();
        assert!(usage.prompt_tokens.is_none());
        assert!(usage.completion_tokens.is_none());
        assert!(usage.total_tokens.is_none());
    }

    #[test]
    fn test_chat_response_with_usage() {
        let resp = ChatResponse {
            content: "Hello!".to_string(),
            usage: Some(TokenUsage {
                prompt_tokens: Some(10),
                completion_tokens: Some(5),
                total_tokens: Some(15),
            }),
        };
        assert_eq!(resp.content, "Hello!");
        assert_eq!(resp.usage.as_ref().unwrap().total_tokens, Some(15));
    }

    #[test]
    fn test_chat_response_without_usage() {
        let resp = ChatResponse {
            content: "No usage info".to_string(),
            usage: None,
        };
        assert!(resp.usage.is_none());
    }

    #[test]
    fn test_llm_config() {
        let config = LlmConfig {
            api_key: "test-key".to_string(),
            model: "gpt-4o".to_string(),
            api_base: Some("https://example.com".to_string()),
        };
        assert_eq!(config.api_key, "test-key");
        assert_eq!(config.model, "gpt-4o");
        assert_eq!(config.api_base.unwrap(), "https://example.com");
    }
}
