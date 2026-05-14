//! Built-in tools for task plan management.
//!
//! Provides `CreatePlanTool` and `UpdatePlanTool` that allow the LLM to
//! create structured execution plans and update step progress. The plan
//! state is stored in `GLOBAL_PLAN` (a process-wide singleton), ensuring
//! the active plan is visible to the tool loop, CLI commands, and both tools.
//!
//! This avoids threading a `SharedPlan` through the middleware pipeline —
//! the global singleton is simpler and sufficient since only one session
//! is active at a time in the CLI.

use anyhow::Result;
use async_trait::async_trait;

use super::BuiltinTool;
use crate::agent::tool_loop::plan_tracker::{GLOBAL_PLAN, StepStatus};

/// Built-in tool that creates a structured task plan.
///
/// When the LLM encounters a complex multi-step task, it can call this
/// tool to create a plan that will be tracked and injected into context
/// metadata each round, preventing goal drift.
pub struct CreatePlanTool;

impl CreatePlanTool {
    /// Create a new create_plan tool.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl BuiltinTool for CreatePlanTool {
    fn name(&self) -> &str {
        "create_plan"
    }

    fn description(&self) -> &str {
        "Create a structured execution plan for a complex multi-step task. \
         The plan will be tracked and displayed in every subsequent round, \
         preventing goal drift during long-running tasks. Use when the task \
         has 3 or more distinct steps. Only one plan can be active at a time; \
         creating a new plan archives the previous one."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "A concise title describing the overall goal of the plan."
                },
                "steps": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Ordered list of step descriptions. Each step should be a concrete, actionable task."
                }
            },
            "required": ["title", "steps"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String> {
        let title = arguments
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: 'title'"))?;

        let raw_steps = arguments
            .get("steps")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: 'steps' (must be an array)"))?;

        let steps: Vec<String> = raw_steps
            .iter()
            .map(|v| {
                v.as_str()
                    .map(|s| s.to_string())
                    .ok_or_else(|| anyhow::anyhow!("Each step must be a string, got: {}", v))
            })
            .collect::<Result<Vec<_>>>()?;

        if steps.is_empty() {
            return Ok("Plan must have at least one step.".to_string());
        }

        if steps.len() > 20 {
            return Ok("Plan has too many steps (max 20). Consider grouping related steps.".to_string());
        }

        let plan_summary = {
            let mut mgr = GLOBAL_PLAN
                .lock()
                .map_err(|_| anyhow::anyhow!("Failed to acquire plan lock"))?;
            let plan = mgr.create_plan(title.to_string(), steps);

            tracing::info!(
                title = %plan.title,
                steps = plan.total_count(),
                "Task plan created"
            );

            plan.format_for_context()
        };

        Ok(format!("Plan created successfully.\n\n{}", plan_summary))
    }
}

/// Built-in tool that updates the status of a plan step.
///
/// The LLM calls this after completing (or failing) a step to keep
/// the plan state accurate. The updated state is automatically
/// reflected in the next round's context metadata.
pub struct UpdatePlanTool;

impl UpdatePlanTool {
    /// Create a new update_plan tool.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl BuiltinTool for UpdatePlanTool {
    fn name(&self) -> &str {
        "update_plan"
    }

    fn description(&self) -> &str {
        "Update the status of a step in the active plan. Call this after \
         completing, starting, or failing a step to keep the plan state \
         accurate. The updated plan will be visible in subsequent rounds."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "step": {
                    "type": "integer",
                    "description": "The step number to update (1-based)."
                },
                "status": {
                    "type": "string",
                    "enum": ["in_progress", "done", "failed", "skipped"],
                    "description": "The new status for the step."
                }
            },
            "required": ["step", "status"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String> {
        let step_number = arguments
            .get("step")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: 'step' (must be an integer)"))?
            as usize;

        let status_str = arguments
            .get("status")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: 'status'"))?;

        let status: StepStatus = status_str
            .parse()
            .map_err(|e: String| anyhow::anyhow!("{}", e))?;

        let mut mgr = GLOBAL_PLAN
            .lock()
            .map_err(|_| anyhow::anyhow!("Failed to acquire plan lock"))?;

        match mgr.update_step(step_number, status) {
            Ok(msg) => {
                tracing::debug!(step = step_number, message = %msg, "Plan step updated");
                Ok(msg)
            }
            Err(e) => Ok(format!("Error: {}", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize all tests that use GLOBAL_PLAN to avoid parallel interference.
    /// Tests run in parallel by default, and since GLOBAL_PLAN is a process-wide
    /// singleton, concurrent tests can corrupt each other's state.
    static TEST_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Reset the global plan state before each test to avoid interference.
    fn reset_global_plan() {
        crate::agent::tool_loop::plan_tracker::reset_global_plan();
    }

    #[tokio::test]
    async fn test_create_plan_basic() {
        let _guard = TEST_MUTEX.lock().unwrap();
        reset_global_plan();
        let tool = CreatePlanTool::new();

        let result = tool
            .execute(serde_json::json!({
                "title": "Refactor auth module",
                "steps": ["Analyze current code", "Design new interface", "Implement changes"]
            }))
            .await
            .unwrap();

        assert!(result.contains("Plan created"));
        assert!(result.contains("Refactor auth module"));
    }

    #[tokio::test]
    async fn test_create_plan_empty_steps() {
        let _guard = TEST_MUTEX.lock().unwrap();
        reset_global_plan();
        let tool = CreatePlanTool::new();

        let result = tool
            .execute(serde_json::json!({
                "title": "Empty",
                "steps": []
            }))
            .await
            .unwrap();

        assert!(result.contains("at least one step"));
    }

    #[tokio::test]
    async fn test_create_plan_missing_title() {
        let _guard = TEST_MUTEX.lock().unwrap();
        reset_global_plan();
        let tool = CreatePlanTool::new();

        let result = tool
            .execute(serde_json::json!({
                "steps": ["A"]
            }))
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_update_plan_basic() {
        let _guard = TEST_MUTEX.lock().unwrap();
        reset_global_plan();
        {
            let mut mgr = GLOBAL_PLAN.lock().unwrap();
            mgr.create_plan("Test".to_string(), vec!["A".to_string(), "B".to_string()]);
        }

        let tool = UpdatePlanTool::new();

        let result = tool
            .execute(serde_json::json!({
                "step": 1,
                "status": "in_progress"
            }))
            .await
            .unwrap();

        assert!(result.contains("updated"));

        let result = tool
            .execute(serde_json::json!({
                "step": 1,
                "status": "done"
            }))
            .await
            .unwrap();

        assert!(result.contains("updated"));
    }

    #[tokio::test]
    async fn test_update_plan_no_active() {
        let _guard = TEST_MUTEX.lock().unwrap();
        reset_global_plan();
        let tool = UpdatePlanTool::new();

        let result = tool
            .execute(serde_json::json!({
                "step": 1,
                "status": "done"
            }))
            .await
            .unwrap();

        assert!(result.contains("Error"));
    }

    #[tokio::test]
    async fn test_update_plan_invalid_status() {
        let _guard = TEST_MUTEX.lock().unwrap();
        reset_global_plan();
        {
            let mut mgr = GLOBAL_PLAN.lock().unwrap();
            mgr.create_plan("Test".to_string(), vec!["A".to_string()]);
        }

        let tool = UpdatePlanTool::new();

        let result = tool
            .execute(serde_json::json!({
                "step": 1,
                "status": "invalid_status"
            }))
            .await;

        assert!(result.is_err());
    }
}
