//! Built-in tool for searching conversation history.
//!
//! Allows the LLM to search past conversation summaries stored in the
//! history log. This is the "read" side of the cold data layer — history
//! entries are written during consolidation and searched on demand here.

use std::sync::{Arc, RwLock};

use anyhow::Result;
use async_trait::async_trait;

use crate::agent::SharedMemory;

use super::BuiltinTool;

/// Default maximum number of search results.
const DEFAULT_LIMIT: usize = 10;

/// Maximum allowed limit to prevent excessive output.
const MAX_LIMIT: usize = 50;

/// Updatable handle to `SharedMemory` that survives session rebuilds.
///
/// When a session is rebuilt (e.g., `/new`, MCP attach), the `SharedMemory`
/// changes. This wrapper allows the tool to always access the latest memory
/// without re-registering the tool.
pub type MemoryHandle = Arc<RwLock<SharedMemory>>;

/// Create a new `MemoryHandle` wrapping the given `SharedMemory`.
pub fn new_memory_handle(memory: SharedMemory) -> MemoryHandle {
    Arc::new(RwLock::new(memory))
}

/// Search past conversation history by keyword.
///
/// This tool is registered dynamically when the memory strategy supports
/// history search (currently only `SlidingWindowMemory`). It gives the
/// LLM the ability to recall past conversations that have been consolidated
/// out of the active message window.
pub struct RecallHistoryTool {
    /// Updatable handle to the shared memory.
    /// Uses `RwLock<SharedMemory>` so the inner `SharedMemory` can be
    /// swapped when a session is rebuilt, without re-registering the tool.
    memory_handle: MemoryHandle,
}

impl RecallHistoryTool {
    /// Create a new recall history tool with the given memory handle.
    pub fn new(memory_handle: MemoryHandle) -> Self {
        Self { memory_handle }
    }
}

#[async_trait]
impl BuiltinTool for RecallHistoryTool {
    fn name(&self) -> &str {
        "recall_history"
    }

    fn description(&self) -> &str {
        "Search past conversation history by keyword. Returns matching summaries from \
         previous conversations that have been consolidated into the history log. \
         Use this when you need to recall what was discussed in earlier conversations \
         or find past decisions, solutions, or context that is no longer in the \
         active conversation window."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The keyword or phrase to search for in past conversation summaries and keywords. Case-insensitive."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results to return. Defaults to 10, maximum 50."
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String> {
        let query = arguments
            .get("query")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: 'query'"))?;

        let limit = arguments
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| (v as usize).min(MAX_LIMIT))
            .unwrap_or(DEFAULT_LIMIT);

        tracing::info!(
            query = %query,
            limit = limit,
            "Searching conversation history"
        );

        // Read the current SharedMemory from the handle, then lock the memory.
        let shared_memory = self.memory_handle.read()
            .map_err(|e| anyhow::anyhow!("Failed to read memory handle: {}", e))?
            .clone();
        let mem = shared_memory.lock().await;
        let results = mem.search_history(&query, Some(limit));

        if results.is_empty() {
            Ok(format!(
                "No history entries found matching '{}'. \
                 History entries are created when conversations are consolidated \
                 (after approximately {} messages).",
                query, 100 // default consolidation_threshold
            ))
        } else {
            let mut output = format!(
                "Found {} history entries matching '{}':\n\n",
                results.len(),
                query,
            );
            for entry in &results {
                output.push_str(entry);
                output.push('\n');
            }
            Ok(output)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tool() -> RecallHistoryTool {
        use std::sync::Arc;
        use tokio::sync::Mutex;
        use crate::memory::SlidingWindowFactory;
        use crate::memory::MemoryFactory;

        let factory = SlidingWindowFactory::new();
        let memory = factory.create_memory("test");
        let shared: SharedMemory = Arc::new(Mutex::new(memory));
        let handle = new_memory_handle(shared);
        RecallHistoryTool::new(handle)
    }

    #[test]
    fn test_recall_history_tool_schema() {
        let tool = make_tool();
        assert_eq!(tool.name(), "recall_history");
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["query"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::json!("query")));
    }

    #[test]
    fn test_to_openai_json() {
        let tool = make_tool();
        let json = tool.to_openai_json();
        assert_eq!(json["type"], "function");
        assert_eq!(json["function"]["name"], "recall_history");
    }

    #[tokio::test]
    async fn test_execute_empty_history() {
        let tool = make_tool();
        let args = serde_json::json!({"query": "rust"});
        let result = tool.execute(args).await.unwrap();
        assert!(result.contains("No history entries found"));
    }

    #[tokio::test]
    async fn test_execute_missing_query() {
        let tool = make_tool();
        let args = serde_json::json!({});
        let result = tool.execute(args).await;
        assert!(result.is_err());
    }
}
