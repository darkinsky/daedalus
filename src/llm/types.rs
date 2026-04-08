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

// ── Tool calling types (provider-agnostic) ──

/// A tool call requested by the LLM.
///
/// This is our own type, decoupled from any specific provider (e.g., genai).
/// The provider layer is responsible for converting to/from this type.
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Unique identifier for this tool call (used to correlate responses).
    pub call_id: String,
    /// Name of the function/tool to invoke.
    pub fn_name: String,
    /// Arguments as a JSON value.
    pub fn_arguments: serde_json::Value,
}

/// A tool response to feed back to the LLM.
#[derive(Debug, Clone)]
pub struct ToolResponse {
    /// The call_id this response corresponds to.
    pub call_id: String,
    /// The tool output content.
    pub content: String,
}

impl ToolResponse {
    /// Create a new tool response.
    pub fn new(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            content: content.into(),
        }
    }
}

/// Response from an LLM chat completion.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    /// The text content of the response.
    pub content: String,
    /// Token usage information (if available).
    pub usage: Option<TokenUsage>,
    /// Tool calls requested by the model (if any).
    pub tool_calls: Vec<ToolCall>,
}

/// The result of an agent chat turn, returned to the CLI layer.
///
/// Contains the response content plus optional token usage metadata.
#[derive(Debug, Clone)]
pub struct ChatResult {
    /// The assistant's text response.
    pub content: String,
    /// Token usage for this turn (if available).
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
    /// Adapter kind hint (e.g., "openai", "anthropic", "gemini").
    /// Defaults to "openai" if not specified.
    pub adapter_kind: Option<String>,
}

/// A tool description exposed to the CLI layer for `/tools` display.
#[derive(Debug, Clone)]
pub struct ToolInfo {
    /// The tool name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Which MCP server provides this tool.
    pub server: String,
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
            tool_calls: vec![],
        };
        assert_eq!(resp.content, "Hello!");
        assert_eq!(resp.usage.as_ref().unwrap().total_tokens, Some(15));
    }

    #[test]
    fn test_chat_response_without_usage() {
        let resp = ChatResponse {
            content: "No usage info".to_string(),
            usage: None,
            tool_calls: vec![],
        };
        assert!(resp.usage.is_none());
    }

    #[test]
    fn test_llm_config() {
        let config = LlmConfig {
            api_key: "test-key".to_string(),
            model: "gpt-4o".to_string(),
            api_base: Some("https://example.com".to_string()),
            adapter_kind: None,
        };
        assert_eq!(config.api_key, "test-key");
        assert_eq!(config.model, "gpt-4o");
        assert_eq!(config.api_base.unwrap(), "https://example.com");
    }

    #[test]
    fn test_tool_response_constructor() {
        let resp = ToolResponse::new("call-123", "result data");
        assert_eq!(resp.call_id, "call-123");
        assert_eq!(resp.content, "result data");
    }

    #[test]
    fn test_chat_result() {
        let result = ChatResult {
            content: "Hello!".to_string(),
            usage: Some(TokenUsage {
                prompt_tokens: Some(10),
                completion_tokens: Some(5),
                total_tokens: Some(15),
            }),
        };
        assert_eq!(result.content, "Hello!");
        assert_eq!(result.usage.as_ref().unwrap().total_tokens, Some(15));
    }
}
