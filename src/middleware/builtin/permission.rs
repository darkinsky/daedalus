//! Permission middleware — tool call authorization enforcement.
//!
//! This middleware finally gives `PermissionMode` (previously dead_code) a
//! real purpose. It checks whether a tool call is allowed before delegation.

use async_trait::async_trait;

use crate::llm::ToolResponse;

use super::super::{ToolMiddleware, ToolNext, ToolRequest};

/// Permission mode for tool call authorization.
///
/// Determines how the middleware handles tool calls.
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionPolicy {
    /// Allow all tool calls (default).
    Allow,
    /// Deny specific tools by name.
    DenyList(Vec<String>),
    /// Allow only specific tools by name.
    AllowList(Vec<String>),
}

impl Default for PermissionPolicy {
    fn default() -> Self {
        Self::Allow
    }
}

/// Tool-level permission middleware.
///
/// Checks each tool call against the configured policy before allowing
/// execution. Rejected calls return a `ToolResponse::error` without
/// reaching the actual tool executor.
pub struct PermissionToolMiddleware {
    policy: PermissionPolicy,
}

impl PermissionToolMiddleware {
    /// Create with the given permission policy.
    pub fn new(policy: PermissionPolicy) -> Self {
        Self { policy }
    }

    /// Create a middleware that allows everything (no-op pass-through).
    #[allow(dead_code)]
    pub fn allow_all() -> Self {
        Self::new(PermissionPolicy::Allow)
    }

    /// Create a middleware that blocks specific tools.
    #[allow(dead_code)]
    pub fn deny(tools: Vec<String>) -> Self {
        Self::new(PermissionPolicy::DenyList(tools))
    }

    /// Create a middleware that only allows specific tools.
    #[allow(dead_code)]
    pub fn allow_only(tools: Vec<String>) -> Self {
        Self::new(PermissionPolicy::AllowList(tools))
    }

    /// Check if a tool call is permitted under the current policy.
    fn is_permitted(&self, tool_name: &str) -> bool {
        match &self.policy {
            PermissionPolicy::Allow => true,
            PermissionPolicy::DenyList(denied) => {
                !denied.iter().any(|d| d == tool_name)
            }
            PermissionPolicy::AllowList(allowed) => {
                allowed.iter().any(|a| a == tool_name)
            }
        }
    }
}

#[async_trait]
impl ToolMiddleware for PermissionToolMiddleware {
    async fn handle(
        &self,
        request: ToolRequest,
        next: &dyn ToolNext,
    ) -> ToolResponse {
        if !self.is_permitted(&request.call.function_name) {
            tracing::warn!(
                tool = %request.call.function_name,
                "Tool call denied by permission policy"
            );
            return ToolResponse::error(
                request.call.call_id,
                format!(
                    "Permission denied: tool '{}' is not allowed under the current policy",
                    request.call.function_name
                ),
            );
        }

        next.run(request).await
    }

    fn name(&self) -> &str {
        "permission"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allow_all() {
        let mw = PermissionToolMiddleware::allow_all();
        assert!(mw.is_permitted("bash"));
        assert!(mw.is_permitted("read_file"));
    }

    #[test]
    fn test_deny_list() {
        let mw = PermissionToolMiddleware::deny(vec!["bash".to_string(), "write_file".to_string()]);
        assert!(!mw.is_permitted("bash"));
        assert!(!mw.is_permitted("write_file"));
        assert!(mw.is_permitted("read_file"));
    }

    #[test]
    fn test_allow_list() {
        let mw = PermissionToolMiddleware::allow_only(vec!["read_file".to_string()]);
        assert!(!mw.is_permitted("bash"));
        assert!(mw.is_permitted("read_file"));
    }
}
