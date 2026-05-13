//! Hooks tool middleware — integrates hooks into the tool pipeline.
//!
//! This middleware runs PreToolUse hooks before tool execution and
//! PostToolUse hooks after. PreToolUse hooks can block execution
//! by returning a non-zero exit code.

use std::sync::Arc;

use async_trait::async_trait;

use crate::llm::ToolResponse;
use crate::middleware::{ToolMiddleware, ToolNext, ToolRequest};

use super::config::{HookEvent, HooksConfig};
use super::executor;

/// Tool-level hooks middleware.
///
/// Runs user-configured shell commands before and after tool execution.
/// PreToolUse hooks can block execution by returning non-zero exit codes.
pub struct HooksToolMiddleware {
    config: Arc<HooksConfig>,
    session_id: String,
}

impl HooksToolMiddleware {
    /// Create a new hooks middleware with the given configuration.
    pub fn new(config: Arc<HooksConfig>, session_id: String) -> Self {
        Self { config, session_id }
    }
}

#[async_trait]
impl ToolMiddleware for HooksToolMiddleware {
    async fn handle(
        &self,
        request: ToolRequest,
        next: &dyn ToolNext,
    ) -> ToolResponse {
        // Check for matching hooks BEFORE cloning to avoid unnecessary allocations.
        // We use the function_name reference from the request (not yet moved).
        let has_pre_hooks = !self.config.matching_hooks(
            HookEvent::PreToolUse, Some(&request.call.function_name)
        ).is_empty();
        let has_post_hooks = !self.config.matching_hooks(
            HookEvent::PostToolUse, Some(&request.call.function_name)
        ).is_empty();

        // Fast path: no hooks match this tool at all — skip all cloning
        if !has_pre_hooks && !has_post_hooks {
            return next.run(request).await;
        }

        // Clone values needed after request is moved into next.run()
        let tool_name = request.call.function_name.clone();
        let tool_input = request.call.arguments.clone();
        let call_id = request.call.call_id.clone();

        // ── PreToolUse hooks ──
        if has_pre_hooks {
            let pre_hooks = self.config.matching_hooks(HookEvent::PreToolUse, Some(&tool_name));
            let env = executor::pre_tool_env(&tool_name, &tool_input, &self.session_id);

            for hook in &pre_hooks {
                let result = executor::execute_hook(hook, &env).await;

                if !result.success {
                    tracing::info!(
                        tool = %tool_name,
                        hook_command = %hook.command,
                        exit_code = ?result.exit_code,
                        "PreToolUse hook blocked tool execution"
                    );
                    return ToolResponse::error(
                        &call_id,
                        format!(
                            "Tool call blocked by PreToolUse hook.\nHook: {}\nOutput: {}",
                            hook.command,
                            result.output.trim()
                        ),
                    );
                }

                tracing::debug!(
                    tool = %tool_name,
                    hook_command = %hook.command,
                    "PreToolUse hook passed"
                );
            }
        }

        // ── Delegate to next middleware / core ──
        let response = next.run(request).await;

        // ── PostToolUse hooks ──
        if has_post_hooks {
            let post_hooks = self.config.matching_hooks(HookEvent::PostToolUse, Some(&tool_name));
            let env = executor::post_tool_env(
                &tool_name,
                &tool_input,
                &response.content,
                response.success,
                &self.session_id,
            );

            for hook in &post_hooks {
                let result = executor::execute_hook(hook, &env).await;

                if !result.success {
                    tracing::warn!(
                        tool = %tool_name,
                        hook_command = %hook.command,
                        exit_code = ?result.exit_code,
                        output = %result.output,
                        "PostToolUse hook failed (non-blocking)"
                    );
                } else {
                    tracing::debug!(
                        tool = %tool_name,
                        hook_command = %hook.command,
                        "PostToolUse hook completed"
                    );
                }
            }
        }

        response
    }

    fn name(&self) -> &str {
        "hooks"
    }
}
