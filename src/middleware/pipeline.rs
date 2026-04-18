//! Pipeline execution engine — chains middleware into onion-model pipelines.
//!
//! Two pipeline types correspond to the two middleware granularities:
//!
//! - [`TurnPipeline`]: processes a full user-input → response cycle.
//! - [`ToolPipeline`]: processes a single tool call.
//!
//! ## Construction
//!
//! ```ignore
//! let pipeline = TurnPipeline::new(core_handler)
//!     .with(memory_middleware)       // innermost — runs last before core
//!     .with(logging_middleware)      // middle
//!     .with(tracing_middleware);     // outermost — runs first
//! ```
//!
//! ## Execution order (onion model)
//!
//! ```text
//! tracing.before → logging.before → memory.before → CORE → memory.after → logging.after → tracing.after
//! ```
//!
//! The last middleware added via `.with()` becomes the outermost layer
//! (first to handle the request, last to see the response).

use async_trait::async_trait;

use super::{
    TurnMiddleware, TurnNext, TurnRequest, TurnResponse,
    ToolMiddleware, ToolNext, ToolRequest,
};
use crate::llm::ToolResponse;

// ════════════════════════════════════════════════════════════
// Turn Pipeline
// ════════════════════════════════════════════════════════════

/// A composed turn pipeline that chains middleware into a single callable unit.
///
/// The pipeline owns a core handler (the actual LLM call logic) and a stack
/// of middleware layers. On `execute()`, it builds a recursive chain from
/// inside out and runs it.
pub struct TurnPipeline {
    /// Middleware stack. Last element = outermost (runs first).
    layers: Vec<Box<dyn TurnMiddleware>>,
    /// The innermost handler (actual LLM call + tool loop).
    core: Box<dyn TurnNext>,
}

impl TurnPipeline {
    /// Create a new pipeline with only the core handler (no middleware).
    pub fn new(core: Box<dyn TurnNext>) -> Self {
        Self {
            layers: Vec::new(),
            core,
        }
    }

    /// Add a middleware layer. Last added = outermost (runs first).
    ///
    /// ```ignore
    /// pipeline
    ///     .with(memory_mw)     // inner
    ///     .with(logging_mw)    // middle
    ///     .with(tracing_mw);   // outer (runs first)
    /// ```
    pub fn with(mut self, middleware: Box<dyn TurnMiddleware>) -> Self {
        self.layers.push(middleware);
        self
    }

    /// Execute the pipeline, passing the request through all middleware
    /// layers in onion order, then through the core handler.
    pub async fn execute(&self, request: TurnRequest<'_>) -> anyhow::Result<TurnResponse> {
        // Build the chain from inside out:
        // Start with core, then wrap with each middleware from first (inner) to last (outer).
        let chain = TurnChainNode::Core(&*self.core);
        let chain = self.layers.iter().fold(chain, |inner, mw| {
            TurnChainNode::Layer {
                middleware: &**mw,
                next: Box::new(inner),
            }
        });
        chain.run(request).await
    }

    /// Return the names of all middleware layers (outermost first).
    #[allow(dead_code)]
    pub fn layer_names(&self) -> Vec<&str> {
        self.layers.iter().rev().map(|mw| mw.name()).collect()
    }

    /// Return the number of middleware layers.
    #[allow(dead_code)]
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }
}

/// Internal recursive chain node for the turn pipeline.
///
/// This is a stack-allocated linked list that avoids heap allocation
/// in the hot path. Each node is either the core handler or a middleware
/// layer wrapping an inner chain.
enum TurnChainNode<'a> {
    /// The innermost node: the core handler.
    Core(&'a dyn TurnNext),
    /// A middleware layer wrapping the rest of the chain.
    Layer {
        middleware: &'a dyn TurnMiddleware,
        next: Box<TurnChainNode<'a>>,
    },
}

#[async_trait]
impl<'a> TurnNext for TurnChainNode<'a> {
    async fn run(&self, request: TurnRequest<'_>) -> anyhow::Result<TurnResponse> {
        match self {
            TurnChainNode::Core(core) => core.run(request).await,
            TurnChainNode::Layer { middleware, next } => {
                middleware.handle(request, &**next).await
            }
        }
    }
}

// ════════════════════════════════════════════════════════════
// Tool Pipeline
// ════════════════════════════════════════════════════════════

/// A composed tool pipeline that chains tool middleware into a single callable unit.
///
/// Works identically to `TurnPipeline` but for individual tool calls.
pub struct ToolPipeline {
    /// Middleware stack. Last element = outermost (runs first).
    layers: Vec<Box<dyn ToolMiddleware>>,
    /// The innermost handler (actual tool execution).
    core: Box<dyn ToolNext>,
}

impl ToolPipeline {
    /// Create a new pipeline with only the core handler (no middleware).
    pub fn new(core: Box<dyn ToolNext>) -> Self {
        Self {
            layers: Vec::new(),
            core,
        }
    }

    /// Add a middleware layer. Last added = outermost (runs first).
    pub fn with(mut self, middleware: Box<dyn ToolMiddleware>) -> Self {
        self.layers.push(middleware);
        self
    }

    /// Execute the pipeline for a single tool call.
    pub async fn execute(&self, request: ToolRequest) -> ToolResponse {
        let chain = ToolChainNode::Core(&*self.core);
        let chain = self.layers.iter().fold(chain, |inner, mw| {
            ToolChainNode::Layer {
                middleware: &**mw,
                next: Box::new(inner),
            }
        });
        chain.run(request).await
    }

    /// Return the names of all middleware layers (outermost first).
    #[allow(dead_code)]
    pub fn layer_names(&self) -> Vec<&str> {
        self.layers.iter().rev().map(|mw| mw.name()).collect()
    }

    /// Return the number of middleware layers.
    #[allow(dead_code)]
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }
}

/// Internal recursive chain node for the tool pipeline.
enum ToolChainNode<'a> {
    Core(&'a dyn ToolNext),
    Layer {
        middleware: &'a dyn ToolMiddleware,
        next: Box<ToolChainNode<'a>>,
    },
}

#[async_trait]
impl<'a> ToolNext for ToolChainNode<'a> {
    async fn run(&self, request: ToolRequest) -> ToolResponse {
        match self {
            ToolChainNode::Core(core) => core.run(request).await,
            ToolChainNode::Layer { middleware, next } => {
                middleware.handle(request, &**next).await
            }
        }
    }
}

// ════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::Extensions;
    use crate::llm::{ChatResponse, TokenUsage, ToolCall};
    use std::sync::{Arc, Mutex};

    // ── Test helpers ──

    /// Records the order of middleware execution for verification.
    type ExecutionLog = Arc<Mutex<Vec<String>>>;

    /// A core handler that returns a fixed response and logs "core".
    struct MockCore {
        log: ExecutionLog,
    }

    #[async_trait]
    impl TurnNext for MockCore {
        async fn run(&self, _request: TurnRequest<'_>) -> anyhow::Result<TurnResponse> {
            self.log.lock().unwrap().push("core".to_string());
            Ok(TurnResponse {
                chat_response: ChatResponse {
                    content: "Hello from core".to_string(),
                    reasoning_content: None,
                    usage: None,
                    tool_calls: vec![],
                },
                tool_history: vec![],
                usage: TokenUsage::default(),
                extensions: Extensions::new(),
            })
        }
    }

    /// A named middleware that logs "NAME.before" and "NAME.after".
    struct LoggingMw {
        label: String,
        log: ExecutionLog,
    }

    #[async_trait]
    impl TurnMiddleware for LoggingMw {
        async fn handle<'a>(
            &self,
            request: TurnRequest<'a>,
            next: &dyn TurnNext,
        ) -> anyhow::Result<TurnResponse> {
            self.log.lock().unwrap().push(format!("{}.before", self.label));
            let resp = next.run(request).await?;
            self.log.lock().unwrap().push(format!("{}.after", self.label));
            Ok(resp)
        }

        fn name(&self) -> &str {
            &self.label
        }
    }

    /// A middleware that modifies the response content.
    struct PrefixMw {
        prefix: String,
    }

    #[async_trait]
    impl TurnMiddleware for PrefixMw {
        async fn handle<'a>(
            &self,
            request: TurnRequest<'a>,
            next: &dyn TurnNext,
        ) -> anyhow::Result<TurnResponse> {
            let mut resp = next.run(request).await?;
            resp.chat_response.content = format!("{}{}", self.prefix, resp.chat_response.content);
            Ok(resp)
        }

        fn name(&self) -> &str {
            "prefix"
        }
    }

    /// A middleware that short-circuits without calling next.
    struct ShortCircuitMw;

    #[async_trait]
    impl TurnMiddleware for ShortCircuitMw {
        async fn handle<'a>(
            &self,
            _request: TurnRequest<'a>,
            _next: &dyn TurnNext,
        ) -> anyhow::Result<TurnResponse> {
            Ok(TurnResponse {
                chat_response: ChatResponse {
                    content: "short-circuited".to_string(),
                    reasoning_content: None,
                    usage: None,
                    tool_calls: vec![],
                },
                tool_history: vec![],
                usage: TokenUsage::default(),
                extensions: Extensions::new(),
            })
        }

        fn name(&self) -> &str {
            "short-circuit"
        }
    }

    // ── Tool pipeline test helpers ──

    struct MockToolCore {
        log: ExecutionLog,
    }

    #[async_trait]
    impl ToolNext for MockToolCore {
        async fn run(&self, request: ToolRequest) -> ToolResponse {
            self.log.lock().unwrap().push("tool-core".to_string());
            ToolResponse::new(request.call.call_id, "tool result")
        }
    }

    struct ToolLogMw {
        label: String,
        log: ExecutionLog,
    }

    #[async_trait]
    impl ToolMiddleware for ToolLogMw {
        async fn handle(
            &self,
            request: ToolRequest,
            next: &dyn ToolNext,
        ) -> ToolResponse {
            self.log.lock().unwrap().push(format!("{}.before", self.label));
            let resp = next.run(request).await;
            self.log.lock().unwrap().push(format!("{}.after", self.label));
            resp
        }

        fn name(&self) -> &str {
            &self.label
        }
    }

    // ── Turn pipeline tests ──

    #[tokio::test]
    async fn test_pipeline_no_middleware() {
        let log: ExecutionLog = Arc::new(Mutex::new(Vec::new()));
        let pipeline = TurnPipeline::new(Box::new(MockCore { log: log.clone() }));

        let request = TurnRequest {
            user_input: "hello",
            messages: vec![],
            extensions: Extensions::new(),
        };

        let resp = pipeline.execute(request).await.unwrap();
        assert_eq!(resp.chat_response.content, "Hello from core");
        assert_eq!(*log.lock().unwrap(), vec!["core"]);
    }

    #[tokio::test]
    async fn test_pipeline_onion_order() {
        let log: ExecutionLog = Arc::new(Mutex::new(Vec::new()));
        let pipeline = TurnPipeline::new(Box::new(MockCore { log: log.clone() }))
            .with(Box::new(LoggingMw { label: "inner".to_string(), log: log.clone() }))
            .with(Box::new(LoggingMw { label: "outer".to_string(), log: log.clone() }));

        let request = TurnRequest {
            user_input: "test",
            messages: vec![],
            extensions: Extensions::new(),
        };

        pipeline.execute(request).await.unwrap();

        let entries = log.lock().unwrap();
        assert_eq!(
            *entries,
            vec![
                "outer.before",  // outermost runs first
                "inner.before",
                "core",          // core in the middle
                "inner.after",
                "outer.after",   // outermost finishes last
            ]
        );
    }

    #[tokio::test]
    async fn test_pipeline_response_modification() {
        let log: ExecutionLog = Arc::new(Mutex::new(Vec::new()));
        let pipeline = TurnPipeline::new(Box::new(MockCore { log }))
            .with(Box::new(PrefixMw { prefix: "[modified] ".to_string() }));

        let request = TurnRequest {
            user_input: "test",
            messages: vec![],
            extensions: Extensions::new(),
        };

        let resp = pipeline.execute(request).await.unwrap();
        assert_eq!(resp.chat_response.content, "[modified] Hello from core");
    }

    #[tokio::test]
    async fn test_pipeline_short_circuit() {
        let log: ExecutionLog = Arc::new(Mutex::new(Vec::new()));
        let pipeline = TurnPipeline::new(Box::new(MockCore { log: log.clone() }))
            .with(Box::new(LoggingMw { label: "inner".to_string(), log: log.clone() }))
            .with(Box::new(ShortCircuitMw));

        let request = TurnRequest {
            user_input: "test",
            messages: vec![],
            extensions: Extensions::new(),
        };

        let resp = pipeline.execute(request).await.unwrap();
        assert_eq!(resp.chat_response.content, "short-circuited");

        // Core and inner middleware should NOT have been called
        let entries = log.lock().unwrap();
        assert!(entries.is_empty(), "Short-circuit should prevent inner layers from running");
    }

    #[tokio::test]
    async fn test_pipeline_error_propagation() {
        /// A middleware that delegates then always errors after
        struct ErrorAfterMw;

        #[async_trait]
        impl TurnMiddleware for ErrorAfterMw {
            async fn handle<'a>(
                &self,
                request: TurnRequest<'a>,
                next: &dyn TurnNext,
            ) -> anyhow::Result<TurnResponse> {
                let _resp = next.run(request).await?;
                anyhow::bail!("middleware error after core")
            }

            fn name(&self) -> &str { "error-after" }
        }

        let log: ExecutionLog = Arc::new(Mutex::new(Vec::new()));
        let pipeline = TurnPipeline::new(Box::new(MockCore { log: log.clone() }))
            .with(Box::new(ErrorAfterMw));

        let request = TurnRequest {
            user_input: "test",
            messages: vec![],
            extensions: Extensions::new(),
        };

        let result = pipeline.execute(request).await;
        let err = result.err().expect("should be an error");
        assert!(err.to_string().contains("middleware error after core"));
        // Core should still have run
        assert_eq!(*log.lock().unwrap(), vec!["core"]);
    }

    #[tokio::test]
    async fn test_pipeline_extensions_pass_through() {
        /// Middleware that inserts a value into extensions
        struct InsertMw;

        #[async_trait]
        impl TurnMiddleware for InsertMw {
            async fn handle<'a>(
                &self,
                mut request: TurnRequest<'a>,
                next: &dyn TurnNext,
            ) -> anyhow::Result<TurnResponse> {
                request.extensions.insert(42u32);
                next.run(request).await
            }

            fn name(&self) -> &str { "insert" }
        }

        /// Core that reads the extension value
        struct ExtReadCore;

        #[async_trait]
        impl TurnNext for ExtReadCore {
            async fn run(&self, request: TurnRequest<'_>) -> anyhow::Result<TurnResponse> {
                let val = request.extensions.get::<u32>().copied().unwrap_or(0);
                Ok(TurnResponse {
                    chat_response: ChatResponse {
                        content: format!("got: {}", val),
                        reasoning_content: None,
                        usage: None,
                        tool_calls: vec![],
                    },
                    tool_history: vec![],
                    usage: TokenUsage::default(),
                    extensions: Extensions::new(),
                })
            }
        }

        let pipeline = TurnPipeline::new(Box::new(ExtReadCore))
            .with(Box::new(InsertMw));

        let request = TurnRequest {
            user_input: "test",
            messages: vec![],
            extensions: Extensions::new(),
        };

        let resp = pipeline.execute(request).await.unwrap();
        assert_eq!(resp.chat_response.content, "got: 42");
    }

    #[tokio::test]
    async fn test_pipeline_layer_names() {
        let log: ExecutionLog = Arc::new(Mutex::new(Vec::new()));
        let pipeline = TurnPipeline::new(Box::new(MockCore { log }))
            .with(Box::new(LoggingMw { label: "memory".to_string(), log: Arc::new(Mutex::new(Vec::new())) }))
            .with(Box::new(LoggingMw { label: "logging".to_string(), log: Arc::new(Mutex::new(Vec::new())) }))
            .with(Box::new(LoggingMw { label: "tracing".to_string(), log: Arc::new(Mutex::new(Vec::new())) }));

        assert_eq!(pipeline.layer_names(), vec!["tracing", "logging", "memory"]);
        assert_eq!(pipeline.layer_count(), 3);
    }

    // ── Tool pipeline tests ──

    #[tokio::test]
    async fn test_tool_pipeline_onion_order() {
        let log: ExecutionLog = Arc::new(Mutex::new(Vec::new()));
        let pipeline = ToolPipeline::new(Box::new(MockToolCore { log: log.clone() }))
            .with(Box::new(ToolLogMw { label: "inner".to_string(), log: log.clone() }))
            .with(Box::new(ToolLogMw { label: "outer".to_string(), log: log.clone() }));

        let request = ToolRequest {
            call: ToolCall {
                call_id: "c1".to_string(),
                function_name: "read_file".to_string(),
                arguments: serde_json::json!({}),
            },
            source: "built-in".to_string(),
            round: 1,
            extensions: Extensions::new(),
        };

        let resp = pipeline.execute(request).await;
        assert_eq!(resp.content, "tool result");
        assert!(resp.success);

        let entries = log.lock().unwrap();
        assert_eq!(
            *entries,
            vec!["outer.before", "inner.before", "tool-core", "inner.after", "outer.after"]
        );
    }

    #[tokio::test]
    async fn test_tool_pipeline_rejection() {
        /// A tool middleware that rejects calls to "bash"
        struct RejectBashMw;

        #[async_trait]
        impl ToolMiddleware for RejectBashMw {
            async fn handle(
                &self,
                request: ToolRequest,
                next: &dyn ToolNext,
            ) -> ToolResponse {
                if request.call.function_name == "bash" {
                    return ToolResponse::error(request.call.call_id, "bash is not allowed");
                }
                next.run(request).await
            }

            fn name(&self) -> &str { "reject-bash" }
        }

        let log: ExecutionLog = Arc::new(Mutex::new(Vec::new()));
        let pipeline = ToolPipeline::new(Box::new(MockToolCore { log: log.clone() }))
            .with(Box::new(RejectBashMw));

        // Allowed tool
        let request = ToolRequest {
            call: ToolCall { call_id: "c1".to_string(), function_name: "read_file".to_string(), arguments: serde_json::json!({}) },
            source: "built-in".to_string(),
            round: 1,
            extensions: Extensions::new(),
        };
        let resp = pipeline.execute(request).await;
        assert!(resp.success);

        // Rejected tool
        let request = ToolRequest {
            call: ToolCall { call_id: "c2".to_string(), function_name: "bash".to_string(), arguments: serde_json::json!({}) },
            source: "built-in".to_string(),
            round: 1,
            extensions: Extensions::new(),
        };
        let resp = pipeline.execute(request).await;
        assert!(!resp.success);
        assert!(resp.content.contains("not allowed"));

        // Only the allowed call should have reached core
        let entries = log.lock().unwrap();
        assert_eq!(*entries, vec!["tool-core"]);
    }
}
