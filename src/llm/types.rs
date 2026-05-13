// ── LLM provider configuration types ──
//
// These types define the configuration and parameters for LLM providers.
// They live in the `llm` module because they are LLM-domain concepts.
// The `config` module imports them when building configuration from env vars.

/// Reasoning effort level for models that support it.
///
/// Maps to OpenAI's `reasoning_effort` and Venus proxy's `thinking_level`/`reasoning_effort`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
}

impl std::fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReasoningEffort::Low => write!(f, "low"),
            ReasoningEffort::Medium => write!(f, "medium"),
            ReasoningEffort::High => write!(f, "high"),
        }
    }
}

impl std::str::FromStr for ReasoningEffort {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "low" => Ok(ReasoningEffort::Low),
            "medium" => Ok(ReasoningEffort::Medium),
            "high" => Ok(ReasoningEffort::High),
            _ => Err(format!("Invalid reasoning effort: '{}'. Expected: low, medium, high", s)),
        }
    }
}

/// Venus API proxy advanced parameters.
///
/// Shared between `LlmConfig` (instance-level defaults) and `ChatOptions`
/// (per-request overrides). This eliminates field duplication and provides
/// a single merge operation for the "request overrides config" pattern.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct VenusExtensions {
    /// Enable thinking/reasoning mode for supported models.
    /// Maps to Venus `thinking_enabled` (Claude, Gemini, VenusLLMServing).
    pub thinking_enabled: Option<bool>,
    /// Maximum tokens for the thinking/reasoning process.
    /// Maps to Venus `thinking_tokens` (Claude, Gemini).
    /// Must be > 1024 and <= max_tokens.
    pub thinking_tokens: Option<u32>,
    /// Reasoning effort level.
    /// Maps to OpenAI `reasoning_effort` (o-series) and
    /// Gemini `thinking_level`/`reasoning_effort` (gemini-3 series).
    pub reasoning_effort: Option<ReasoningEffort>,
}

impl VenusExtensions {
    /// Return true if any Venus extension parameter is configured.
    ///
    /// Useful for provider selection logic and diagnostics.
    #[allow(dead_code)]
    pub fn is_active(&self) -> bool {
        self.thinking_enabled.is_some()
            || self.thinking_tokens.is_some()
            || self.reasoning_effort.is_some()
    }

    /// Return true if advanced thinking parameters are configured.
    ///
    /// Useful for diagnostics and logging.
    #[allow(dead_code)]
    pub fn needs_thinking_provider(&self) -> bool {
        self.thinking_enabled.is_some() || self.thinking_tokens.is_some()
    }

    /// Merge with another `VenusExtensions`, using `other` as the
    /// higher-priority source (request-level overrides config-level).
    ///
    /// For each field, if `other` has a value it wins; otherwise `self`
    /// (the config default) is used.
    pub fn merge_with_overrides(&self, overrides: &VenusExtensions) -> VenusExtensions {
        VenusExtensions {
            thinking_enabled: overrides.thinking_enabled.or(self.thinking_enabled),
            thinking_tokens: overrides.thinking_tokens.or(self.thinking_tokens),
            reasoning_effort: overrides.reasoning_effort.clone().or(self.reasoning_effort.clone()),
        }
    }

}

/// Configuration for an LLM provider.
#[derive(Clone, serde::Deserialize)]
#[serde(default)]
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
    /// Venus API proxy advanced options (thinking, reasoning_effort).
    #[serde(default)]
    pub venus: VenusExtensions,
    /// Override the model's context window size (in tokens).
    ///
    /// When set, this value takes priority over the built-in model registry.
    /// When not set, the system looks up the model name in the registry,
    /// falling back to 128K if the model is not recognized.
    ///
    /// This affects truncation budgets and auto-compact thresholds.
    pub context_window: Option<usize>,
    /// Optional session ID for routing affinity.
    ///
    /// When set, this is sent as `Venus-Session-Id` header to ensure all
    /// requests from the same logical session (e.g., a single subagent's
    /// tool-calling loop) are routed to the same backend node.
    ///
    /// This is critical for prompt cache efficiency: parallel subagents
    /// sharing the same API token would otherwise all route to the same
    /// backend (via `Venus-Sticky-Routing: token`), causing cache eviction
    /// storms. With per-subagent session IDs, each subagent gets its own
    /// backend affinity and maintains independent prefix cache.
    #[serde(skip)]
    pub session_id: Option<String>,
}

/// Custom Debug implementation that redacts the API key to prevent
/// accidental leakage in logs, tracing output, or error messages.
impl std::fmt::Debug for LlmConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted_key = if self.api_key.len() > 8 {
            format!("{}...{}", &self.api_key[..4], &self.api_key[self.api_key.len()-4..])
        } else if !self.api_key.is_empty() {
            "***REDACTED***".to_string()
        } else {
            "(empty)".to_string()
        };
        f.debug_struct("LlmConfig")
            .field("api_key", &redacted_key)
            .field("model", &self.model)
            .field("api_base", &self.api_base)
            .field("adapter_kind", &self.adapter_kind)
            .field("venus", &self.venus)
            .field("context_window", &self.context_window)
            .field("session_id", &self.session_id)
            .finish()
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: "gpt-4o".to_string(),
            api_base: None,
            adapter_kind: None,
            venus: VenusExtensions::default(),
            context_window: None,
            session_id: None,
        }
    }
}

impl LlmConfig {
    /// Resolve the effective context window size for this configuration.
    ///
    /// Priority:
    /// 1. Explicit `context_window` value from config
    /// 2. Built-in model registry lookup by model name
    /// 3. Default (128K)
    pub fn resolved_context_window(&self) -> usize {
        super::model_registry::resolve_context_window(&self.model, self.context_window)
    }
}

/// A part of a multimodal message content.
///
/// Messages can contain a mix of text and images. When `content_parts` is
/// non-empty on a `ChatMessage`, it takes precedence over the `content` field.
#[derive(Debug, Clone)]
pub enum ContentPart {
    /// A text content block.
    Text { text: String },
    /// An image content block.
    Image {
        /// The image source (base64 or URL).
        source: ImageSource,
        /// Detail level hint: "auto", "low", or "high".
        /// Controls how the model processes the image.
        #[allow(dead_code)]
        detail: Option<String>,
    },
}

/// Source of an image in a multimodal message.
#[derive(Debug, Clone)]
pub enum ImageSource {
    /// Base64-encoded image data.
    Base64 {
        /// MIME type (e.g., "image/png", "image/jpeg").
        media_type: String,
        /// Base64-encoded image data.
        data: String,
    },
    /// Image from a URL.
    #[allow(dead_code)]
    Url {
        /// The image URL.
        url: String,
    },
}

/// A single message in a conversation.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
    /// Rich content parts (text + images). If non-empty, takes precedence
    /// over `content` when building API requests.
    pub content_parts: Vec<ContentPart>,
    /// Optional cache control hint for prompt caching optimization.
    ///
    /// When set, the provider should mark this message as a cache
    /// breakpoint. This enables API-level prompt caching where the
    /// static prefix of the conversation is cached and reused across
    /// requests, significantly reducing latency and cost.
    ///
    /// Typically set on the system message that contains the static
    /// portion of the prompt (identity, rules, tool definitions).
    pub cache_control: Option<CacheControl>,
    /// Whether this message is semantically preserved (not compressible).
    ///
    /// When `true`, the compact algorithm will skip this message during
    /// compression, keeping it verbatim in the message list even if it
    /// falls outside the `compact_preserve_recent` window.
    ///
    /// Use cases:
    /// - User's initial task instruction (the "goal" message)
    /// - Messages containing critical error information
    /// - Important decision points
    /// - Messages explicitly marked by the user or agent
    ///
    /// This field is memory-internal metadata and is NOT sent to the LLM
    /// provider — it only affects the compact algorithm's behavior.
    pub preserved: bool,
}

/// The role of a message sender.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatRole {
    System,
    User,
    Assistant,
    /// A tool/function response message (OpenAI `tool` role).
    ///
    /// Reserved for future use when memory needs to distinguish tool
    /// responses from regular assistant messages.
    #[allow(dead_code)]
    Tool,
}

impl std::fmt::Display for ChatRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChatRole::System => write!(f, "system"),
            ChatRole::User => write!(f, "user"),
            ChatRole::Assistant => write!(f, "assistant"),
            ChatRole::Tool => write!(f, "tool"),
        }
    }
}

/// Format a slice of ChatMessages into a JSON string for logging.
pub fn format_messages_for_log(messages: &[ChatMessage]) -> String {
    let entries: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| {
            let mut obj = serde_json::json!({
                "role": m.role.to_string(),
                "content": m.content,
            });
            if m.cache_control.is_some() {
                obj["cache_control"] = serde_json::json!("ephemeral");
            }
            obj
        })
        .collect();
    serde_json::to_string(&entries).unwrap_or_else(|_| "[]".to_string())
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: ChatRole::System, content: content.into(), content_parts: Vec::new(), cache_control: None, preserved: false }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self { role: ChatRole::User, content: content.into(), content_parts: Vec::new(), cache_control: None, preserved: false }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: ChatRole::Assistant, content: content.into(), content_parts: Vec::new(), cache_control: None, preserved: false }
    }

    /// Create a tool message.
    ///
    /// Reserved for future use when tool responses are stored as
    /// distinct message types in conversation memory.
    #[allow(dead_code)]
    pub fn tool(content: impl Into<String>) -> Self {
        Self { role: ChatRole::Tool, content: content.into(), content_parts: Vec::new(), cache_control: None, preserved: false }
    }

    /// Create a user message with text and image content parts.
    pub fn user_with_image(text: &str, image_source: ImageSource) -> Self {
        Self {
            role: ChatRole::User,
            content: text.to_string(),
            content_parts: vec![
                ContentPart::Text { text: text.to_string() },
                ContentPart::Image { source: image_source, detail: Some("auto".to_string()) },
            ],
            cache_control: None,
            preserved: false,
        }
    }

    /// Check if this message has multimodal content parts.
    pub fn has_content_parts(&self) -> bool {
        !self.content_parts.is_empty()
    }

    /// Set cache control on this message (builder pattern).
    ///
    /// Marks this message as a cache breakpoint for prompt caching.
    /// The provider will use this hint to enable API-level caching.
    pub fn with_cache_control(mut self, cc: CacheControl) -> Self {
        self.cache_control = Some(cc);
        self
    }

    /// Mark this message as semantically preserved (builder pattern).
    ///
    /// Preserved messages are never compressed by the compact algorithm,
    /// even if they fall outside the `compact_preserve_recent` window.
    #[allow(dead_code)]
    pub fn with_preserved(mut self, preserved: bool) -> Self {
        self.preserved = preserved;
        self
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
    /// Name of the tool/function to invoke.
    pub function_name: String,
    /// Arguments as a JSON value.
    pub arguments: serde_json::Value,
}

/// A tool response to feed back to the LLM.
#[derive(Debug, Clone)]
pub struct ToolResponse {
    /// The call_id this response corresponds to.
    pub call_id: String,
    /// The tool output content.
    pub content: String,
    /// Whether the tool call succeeded.
    ///
    /// This replaces the fragile `content.starts_with("Error")` heuristic
    /// with an explicit success/failure signal from the execution layer.
    pub success: bool,
}

impl ToolResponse {
    /// Create a new successful tool response.
    pub fn new(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            content: content.into(),
            success: true,
        }
    }

    /// Create a new failed tool response.
    pub fn error(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            content: content.into(),
            success: false,
        }
    }
}

/// A single round of tool calls and their corresponding responses.
///
/// Replaces the raw `(Vec<ToolCall>, Vec<ToolResponse>)` tuple with a
/// named struct for better readability at call sites and in function
/// signatures.
#[derive(Debug, Clone)]
pub struct ToolRound {
    /// Tool calls requested by the LLM in this round.
    pub calls: Vec<ToolCall>,
    /// Responses from executing the tool calls, in the same order.
    pub responses: Vec<ToolResponse>,
    /// Optional reasoning/thinking content from the LLM for this round.
    ///
    /// DeepSeek V4 requires `reasoning_content` to be passed back to the API
    /// in subsequent requests when thinking mode is active. Without this,
    /// the API returns a 400 error: "The reasoning_content in the thinking
    /// mode must be passed back to the API."
    pub reasoning_content: Option<String>,
}

/// Response from an LLM chat completion.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    /// The text content of the response.
    pub content: String,
    /// Optional reasoning/thinking content returned by reasoning models
    /// (e.g., Claude extended thinking, DeepSeek-R1, OpenAI o1/o3).
    pub reasoning_content: Option<String>,
    /// Token usage information (if available).
    pub usage: Option<TokenUsage>,
    /// Tool calls requested by the model (if any).
    pub tool_calls: Vec<ToolCall>,
}

/// Token usage statistics.
#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    /// Number of prompt tokens that were served from cache.
    ///
    /// When prompt caching is active, this indicates how many input
    /// tokens were reused from a previous request's cache, avoiding
    /// recomputation. A high ratio of `cached_tokens / prompt_tokens`
    /// indicates effective cache utilization.
    ///
    /// - Anthropic: `usage.cache_read_input_tokens`
    /// - OpenAI: `usage.prompt_tokens_details.cached_tokens`
    /// - Venus proxy: `usage.prompt_tokens_details.cached_tokens`
    pub cached_tokens: Option<u64>,
}

impl TokenUsage {
    /// Accumulate token usage from another `TokenUsage` into this one.
    ///
    /// Each field is summed independently. If both sides are `None`, the
    /// result stays `None`; otherwise the values are added.
    pub fn accumulate(&mut self, other: &TokenUsage) {
        self.prompt_tokens = sum_optional(self.prompt_tokens, other.prompt_tokens);
        self.completion_tokens = sum_optional(self.completion_tokens, other.completion_tokens);
        self.total_tokens = sum_optional(self.total_tokens, other.total_tokens);
        self.cached_tokens = sum_optional(self.cached_tokens, other.cached_tokens);
    }
}

/// Sum two optional token counts, returning `None` only if both are `None`.
fn sum_optional(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x + y),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

/// Options for chat completion requests.
///
/// Includes standard parameters (temperature, max_tokens, top_p) and
/// Venus API proxy advanced parameters via `VenusExtensions`.
#[derive(Debug, Clone, Default)]
pub struct ChatOptions {
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    pub top_p: Option<f64>,
    /// Venus API proxy advanced parameters (thinking, reasoning_effort).
    pub venus: VenusExtensions,
}

/// Cache control hint for prompt caching optimization.
///
/// When set on a `ChatMessage`, tells the provider to mark this message
/// (or content block) as a cache breakpoint. Providers that support
/// prompt caching (Anthropic, OpenAI, Venus proxy) will use this to
/// avoid reprocessing static prefix content across requests.
///
/// Messages *without* `cache_control` are treated normally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheControl {
    /// Ephemeral cache — content is cached for the duration of the
    /// conversation but not persisted across sessions.
    /// Maps to Anthropic's `{"type": "ephemeral"}` and OpenAI's
    /// automatic prefix caching boundary hint.
    Ephemeral,
}

// ── Streaming types ──

/// A single chunk from a streaming LLM response.
///
/// The streaming protocol emits a sequence of these chunks, each carrying
/// an incremental piece of the response. The consumer accumulates them
/// into a full `ChatResponse`.
#[derive(Debug, Clone)]
pub enum StreamChunk {
    /// Incremental text content (the main response body).
    ContentDelta(String),
    /// Incremental reasoning/thinking content.
    ReasoningDelta(String),
    /// A complete tool call (emitted once fully parsed from the stream).
    ToolCall(ToolCall),
    /// Token usage statistics (typically the last chunk in a stream).
    Usage(TokenUsage),
    /// The stream has ended. No more chunks will follow.
    Done,
}

/// Accumulator that assembles `StreamChunk`s into a final `ChatResponse`.
///
/// Used by the tool loop and CLI layer to collect streaming output.
#[derive(Debug, Default)]
pub struct StreamAccumulator {
    pub content: String,
    pub reasoning_content: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Option<TokenUsage>,
}

impl StreamAccumulator {
    /// Apply a single chunk to the accumulator.
    pub fn apply(&mut self, chunk: &StreamChunk) {
        match chunk {
            StreamChunk::ContentDelta(delta) => self.content.push_str(delta),
            StreamChunk::ReasoningDelta(delta) => self.reasoning_content.push_str(delta),
            StreamChunk::ToolCall(tc) => self.tool_calls.push(tc.clone()),
            StreamChunk::Usage(u) => self.usage = Some(u.clone()),
            StreamChunk::Done => {}
        }
    }

    /// Convert the accumulated state into a `ChatResponse`.
    pub fn into_response(self) -> ChatResponse {
        ChatResponse {
            content: self.content,
            reasoning_content: if self.reasoning_content.is_empty() {
                None
            } else {
                Some(self.reasoning_content)
            },
            usage: self.usage,
            tool_calls: self.tool_calls,
        }
    }
}

// NOTE: `ToolInfo` has been moved to `crate::tools::ToolInfo` where it
// semantically belongs (it describes tools, not LLM concepts).

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

        let tool = ChatMessage::tool("tool result");
        assert_eq!(tool.role, ChatRole::Tool);
        assert_eq!(tool.content, "tool result");
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
        assert_ne!(ChatRole::Tool, ChatRole::Assistant);
    }

    #[test]
    fn test_chat_role_display() {
        assert_eq!(ChatRole::System.to_string(), "system");
        assert_eq!(ChatRole::User.to_string(), "user");
        assert_eq!(ChatRole::Assistant.to_string(), "assistant");
        assert_eq!(ChatRole::Tool.to_string(), "tool");
    }

    #[test]
    fn test_chat_options_default() {
        let opts = ChatOptions::default();
        assert!(opts.temperature.is_none());
        assert!(opts.max_tokens.is_none());
        assert!(opts.top_p.is_none());
        assert!(opts.venus.thinking_enabled.is_none());
        assert!(opts.venus.thinking_tokens.is_none());
        assert!(opts.venus.reasoning_effort.is_none());
    }

    #[test]
    fn test_reasoning_effort_parse() {
        assert_eq!("low".parse::<ReasoningEffort>().unwrap(), ReasoningEffort::Low);
        assert_eq!("Medium".parse::<ReasoningEffort>().unwrap(), ReasoningEffort::Medium);
        assert_eq!("HIGH".parse::<ReasoningEffort>().unwrap(), ReasoningEffort::High);
        assert!("invalid".parse::<ReasoningEffort>().is_err());
    }

    #[test]
    fn test_reasoning_effort_display() {
        assert_eq!(ReasoningEffort::Low.to_string(), "low");
        assert_eq!(ReasoningEffort::Medium.to_string(), "medium");
        assert_eq!(ReasoningEffort::High.to_string(), "high");
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
            reasoning_content: None,
            usage: Some(TokenUsage {
                prompt_tokens: Some(10),
                completion_tokens: Some(5),
                total_tokens: Some(15),
                cached_tokens: None,
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
            reasoning_content: None,
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
            venus: VenusExtensions {
                thinking_enabled: Some(true),
                thinking_tokens: Some(2048),
                reasoning_effort: Some(ReasoningEffort::High),
            },
            context_window: None,
            session_id: None,
        };
        assert_eq!(config.api_key, "test-key");
        assert_eq!(config.model, "gpt-4o");
        assert_eq!(config.api_base.unwrap(), "https://example.com");
        assert_eq!(config.venus.thinking_enabled, Some(true));
        assert_eq!(config.venus.thinking_tokens, Some(2048));
        assert_eq!(config.venus.reasoning_effort, Some(ReasoningEffort::High));
    }

    #[test]
    fn test_tool_response_constructor() {
        let resp = ToolResponse::new("call-123", "result data");
        assert_eq!(resp.call_id, "call-123");
        assert_eq!(resp.content, "result data");
        assert!(resp.success);
    }

    #[test]
    fn test_tool_response_error() {
        let resp = ToolResponse::error("call-456", "Something went wrong");
        assert_eq!(resp.call_id, "call-456");
        assert_eq!(resp.content, "Something went wrong");
        assert!(!resp.success);
    }

    #[test]
    fn test_token_usage_accumulate_both_some() {
        let mut total = TokenUsage {
            prompt_tokens: Some(10),
            completion_tokens: Some(5),
            total_tokens: Some(15),
            cached_tokens: None,
        };
        let round = TokenUsage {
            prompt_tokens: Some(20),
            completion_tokens: Some(10),
            total_tokens: Some(30),
            cached_tokens: None,
        };
        total.accumulate(&round);
        assert_eq!(total.prompt_tokens, Some(30));
        assert_eq!(total.completion_tokens, Some(15));
        assert_eq!(total.total_tokens, Some(45));
    }

    #[test]
    fn test_token_usage_accumulate_from_default() {
        let mut total = TokenUsage::default();
        let round = TokenUsage {
            prompt_tokens: Some(10),
            completion_tokens: None,
            total_tokens: Some(10),
            cached_tokens: None,
        };
        total.accumulate(&round);
        assert_eq!(total.prompt_tokens, Some(10));
        assert_eq!(total.completion_tokens, None);
        assert_eq!(total.total_tokens, Some(10));
    }

    #[test]
    fn test_token_usage_accumulate_both_none() {
        let mut total = TokenUsage::default();
        let round = TokenUsage::default();
        total.accumulate(&round);
        assert!(total.prompt_tokens.is_none());
        assert!(total.completion_tokens.is_none());
        assert!(total.total_tokens.is_none());
    }
}
