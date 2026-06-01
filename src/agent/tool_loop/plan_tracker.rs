//! Task plan tracking for structured multi-step execution.
//!
//! Provides a `PlanManager` that tracks the lifecycle of a task plan:
//! create → track progress → update steps → complete.
//!
//! The plan state is stored in a `SharedPlan` (`Arc<Mutex<PlanManager>>`),
//! which is session-scoped and injected into the tool loop, CLI commands,
//! and both `CreatePlanTool` / `UpdatePlanTool` via dependency injection.
//! Each round, the active plan is injected into session metadata so the
//! LLM always knows what step it's on.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// Shared plan state accessible by both the plan tools and the tool loop.
///
/// The `Arc<Mutex<PlanManager>>` is shared between `CreatePlanTool` /
/// `UpdatePlanTool` and `run_tool_loop`, which injects the active plan
/// into session metadata each round.
pub type SharedPlan = Arc<Mutex<PlanManager>>;

/// Create a new empty shared plan manager.
pub fn new_shared_plan() -> SharedPlan {
    Arc::new(Mutex::new(PlanManager::new()))
}

/// Status of a single plan step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepStatus {
    /// Not yet started.
    Pending,
    /// Currently being worked on.
    InProgress,
    /// Successfully completed.
    Done,
    /// Skipped by user or agent.
    Skipped,
    /// Failed during execution.
    Failed,
}

impl std::fmt::Display for StepStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "⏳ pending"),
            Self::InProgress => write!(f, "🔄 in_progress"),
            Self::Done => write!(f, "✅ done"),
            Self::Skipped => write!(f, "⏭️ skipped"),
            Self::Failed => write!(f, "❌ failed"),
        }
    }
}

impl std::str::FromStr for StepStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "pending" => Ok(Self::Pending),
            "in_progress" | "inprogress" | "in-progress" => Ok(Self::InProgress),
            "done" | "completed" | "complete" => Ok(Self::Done),
            "skipped" | "skip" => Ok(Self::Skipped),
            "failed" | "fail" | "error" => Ok(Self::Failed),
            _ => Err(format!(
                "Invalid step status: '{}'. Expected: pending, in_progress, done, skipped, failed",
                s
            )),
        }
    }
}

/// A single step in a task plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    /// Step number (1-based).
    pub number: usize,
    /// Description of what this step does.
    pub description: String,
    /// Current status.
    pub status: StepStatus,
}

/// A structured task plan with tracked progress.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPlan {
    /// Plan title/goal.
    pub title: String,
    /// Ordered steps.
    pub steps: Vec<PlanStep>,
}

impl TaskPlan {
    /// Create a new plan from a title and step descriptions.
    pub fn new(title: String, step_descriptions: Vec<String>) -> Self {
        let steps = step_descriptions
            .into_iter()
            .enumerate()
            .map(|(i, desc)| PlanStep {
                number: i + 1,
                description: desc,
                status: StepStatus::Pending,
            })
            .collect();
        Self { title, steps }
    }

    /// Count of completed steps (Done + Skipped).
    pub fn completed_count(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| s.status == StepStatus::Done || s.status == StepStatus::Skipped)
            .count()
    }

    /// Count of total steps.
    pub fn total_count(&self) -> usize {
        self.steps.len()
    }

    /// Whether all steps are terminal (Done, Skipped, or Failed).
    pub fn is_complete(&self) -> bool {
        self.steps.iter().all(|s| {
            matches!(
                s.status,
                StepStatus::Done | StepStatus::Skipped | StepStatus::Failed
            )
        })
    }

    /// Find the current step (first InProgress, or first Pending if none in progress).
    #[allow(dead_code)]
    pub fn current_step(&self) -> Option<&PlanStep> {
        self.steps
            .iter()
            .find(|s| s.status == StepStatus::InProgress)
            .or_else(|| self.steps.iter().find(|s| s.status == StepStatus::Pending))
    }

    /// Format the plan for injection into LLM context metadata.
    pub fn format_for_context(&self) -> String {
        let mut out = format!("[Active Plan: \"{}\"]\n", self.title);
        for step in &self.steps {
            let icon = match step.status {
                StepStatus::Pending => "⏳",
                StepStatus::InProgress => "🔄",
                StepStatus::Done => "✅",
                StepStatus::Skipped => "⏭️",
                StepStatus::Failed => "❌",
            };
            let current_marker = if step.status == StepStatus::InProgress {
                "  ← current"
            } else {
                ""
            };
            out.push_str(&format!(
                "  {}. {} {}{}\n",
                step.number, icon, step.description, current_marker
            ));
        }
        out.push_str(&format!(
            "[Progress: {}/{} done]",
            self.completed_count(),
            self.total_count()
        ));
        out
    }

    /// Format the plan for CLI display (with ANSI colors).
    pub fn format_for_display(&self) -> Vec<String> {
        let mut lines = Vec::new();
        lines.push(format!("  Plan: {}", self.title));
        lines.push(String::new());
        for step in &self.steps {
            let icon = match step.status {
                StepStatus::Pending => "⏳",
                StepStatus::InProgress => "🔄",
                StepStatus::Done => "✅",
                StepStatus::Skipped => "⏭️",
                StepStatus::Failed => "❌",
            };
            let current_marker = if step.status == StepStatus::InProgress {
                "  ← current"
            } else {
                ""
            };
            lines.push(format!(
                "    {}. {} {}{}",
                step.number, icon, step.description, current_marker
            ));
        }
        lines.push(String::new());
        lines.push(format!(
            "  Progress: {}/{} completed",
            self.completed_count(),
            self.total_count()
        ));
        lines
    }
}

/// Manages the active plan for the current session.
///
/// Only one plan can be active at a time. Creating a new plan replaces
/// the previous one (which is moved to the archive).
pub struct PlanManager {
    /// The current active plan (only one at a time).
    active_plan: Option<TaskPlan>,
    /// Archived (completed) plans from this session.
    archived_plans: Vec<TaskPlan>,
}

impl PlanManager {
    /// Create a new empty plan manager.
    pub fn new() -> Self {
        Self {
            active_plan: None,
            archived_plans: Vec::new(),
        }
    }

    /// Create a new plan, archiving any existing active plan.
    pub fn create_plan(&mut self, title: String, steps: Vec<String>) -> &TaskPlan {
        // Archive the current plan if one exists
        if let Some(old_plan) = self.active_plan.take() {
            self.archived_plans.push(old_plan);
        }
        self.active_plan = Some(TaskPlan::new(title, steps));
        self.active_plan.as_ref().unwrap()
    }

    /// Get the active plan, if any.
    pub fn active_plan(&self) -> Option<&TaskPlan> {
        self.active_plan.as_ref()
    }

    /// Update the status of a step in the active plan.
    ///
    /// Returns an error message if no plan is active or the step number is invalid.
    ///
    /// Enforces single `InProgress` constraint: when setting a step to `InProgress`,
    /// any other step currently `InProgress` is automatically reset to `Pending`.
    pub fn update_step(&mut self, step_number: usize, status: StepStatus) -> Result<String, String> {
        let plan = self
            .active_plan
            .as_mut()
            .ok_or_else(|| "No active plan. Use create_plan first.".to_string())?;

        let total_steps = plan.steps.len();

        // Validate step number exists before enforcing constraints
        if !plan.steps.iter().any(|s| s.number == step_number) {
            return Err(format!(
                "Step {} not found. Plan has {} steps.",
                step_number, total_steps
            ));
        }

        // Enforce single in_progress constraint (inspired by Claude Code's TodoWrite):
        // Only one step can be InProgress at a time. When marking a new step as
        // InProgress, automatically reset any other InProgress step to Pending.
        if status == StepStatus::InProgress {
            for step in plan.steps.iter_mut() {
                if step.number != step_number && step.status == StepStatus::InProgress {
                    step.status = StepStatus::Pending;
                }
            }
        }

        let step = plan
            .steps
            .iter_mut()
            .find(|s| s.number == step_number)
            .unwrap(); // Safe: we validated existence above

        let old_status = step.status.clone();
        step.status = status.clone();

        let msg = format!(
            "Step {} updated: {} → {}",
            step_number, old_status, status
        );

        // Auto-archive if all steps are terminal
        if plan.is_complete() {
            let completed_plan = self.active_plan.take().unwrap();
            let summary = format!(
                "{}. Plan \"{}\" completed ({}/{} done).",
                msg,
                completed_plan.title,
                completed_plan.completed_count(),
                completed_plan.total_count()
            );
            self.archived_plans.push(completed_plan);
            return Ok(summary);
        }

        Ok(msg)
    }

    /// Skip the current in-progress or next pending step.
    ///
    /// Returns a description of what was skipped, or an error if nothing to skip.
    /// If skipping the last non-terminal step completes the plan, it is
    /// automatically archived (same behaviour as `update_step`).
    pub fn skip_current(&mut self) -> Result<String, String> {
        let plan = self
            .active_plan
            .as_mut()
            .ok_or_else(|| "No active plan.".to_string())?;

        // Find the current step index (in_progress first, then first pending)
        let idx = plan
            .steps
            .iter()
            .position(|s| s.status == StepStatus::InProgress)
            .or_else(|| {
                plan.steps
                    .iter()
                    .position(|s| s.status == StepStatus::Pending)
            });

        match idx {
            Some(i) => {
                let desc = plan.steps[i].description.clone();
                let num = plan.steps[i].number;
                plan.steps[i].status = StepStatus::Skipped;

                let msg = format!("Skipped step {}: {}", num, desc);

                // Auto-archive if all steps are now terminal
                if plan.is_complete() {
                    let completed_plan = self.active_plan.take().unwrap();
                    let summary = format!(
                        "{}. Plan \"{}\" completed ({}/{} done).",
                        msg,
                        completed_plan.title,
                        completed_plan.completed_count(),
                        completed_plan.total_count()
                    );
                    self.archived_plans.push(completed_plan);
                    return Ok(summary);
                }

                Ok(msg)
            }
            None => Err("No pending or in-progress steps to skip.".to_string()),
        }
    }

    /// Get the number of archived plans.
    #[allow(dead_code)]
    pub fn archived_count(&self) -> usize {
        self.archived_plans.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_plan() {
        let mut mgr = PlanManager::new();
        let plan = mgr.create_plan(
            "Test Plan".to_string(),
            vec!["Step A".to_string(), "Step B".to_string(), "Step C".to_string()],
        );
        assert_eq!(plan.title, "Test Plan");
        assert_eq!(plan.total_count(), 3);
        assert_eq!(plan.completed_count(), 0);
        assert!(!plan.is_complete());
    }

    #[test]
    fn test_update_step() {
        let mut mgr = PlanManager::new();
        mgr.create_plan(
            "Test".to_string(),
            vec!["A".to_string(), "B".to_string()],
        );

        let result = mgr.update_step(1, StepStatus::InProgress);
        assert!(result.is_ok());

        let result = mgr.update_step(1, StepStatus::Done);
        assert!(result.is_ok());

        let plan = mgr.active_plan().unwrap();
        assert_eq!(plan.completed_count(), 1);
    }

    #[test]
    fn test_auto_archive_on_complete() {
        let mut mgr = PlanManager::new();
        mgr.create_plan("Test".to_string(), vec!["A".to_string()]);

        let result = mgr.update_step(1, StepStatus::Done);
        assert!(result.is_ok());
        assert!(result.unwrap().contains("completed"));

        // Plan should be archived
        assert!(mgr.active_plan().is_none());
        assert_eq!(mgr.archived_count(), 1);
    }

    #[test]
    fn test_skip_current() {
        let mut mgr = PlanManager::new();
        mgr.create_plan(
            "Test".to_string(),
            vec!["A".to_string(), "B".to_string()],
        );

        // Skip first pending step
        let result = mgr.skip_current();
        assert!(result.is_ok());
        assert!(result.unwrap().contains("step 1: A"));

        let plan = mgr.active_plan().unwrap();
        assert_eq!(plan.steps[0].status, StepStatus::Skipped);
    }

    #[test]
    fn test_skip_current_auto_archives_on_complete() {
        let mut mgr = PlanManager::new();
        mgr.create_plan("Test".to_string(), vec!["A".to_string()]);

        let result = mgr.skip_current();
        assert!(result.is_ok());
        let msg = result.unwrap();
        assert!(msg.contains("completed"));

        // Plan should be archived
        assert!(mgr.active_plan().is_none());
        assert_eq!(mgr.archived_count(), 1);
    }

    #[test]
    fn test_create_plan_archives_old() {
        let mut mgr = PlanManager::new();
        mgr.create_plan("Plan 1".to_string(), vec!["A".to_string()]);
        mgr.create_plan("Plan 2".to_string(), vec!["B".to_string()]);

        assert_eq!(mgr.active_plan().unwrap().title, "Plan 2");
        assert_eq!(mgr.archived_count(), 1);
    }

    #[test]
    fn test_format_for_context() {
        let mut mgr = PlanManager::new();
        mgr.create_plan(
            "Refactor auth".to_string(),
            vec!["Analyze".to_string(), "Implement".to_string(), "Test".to_string()],
        );
        mgr.update_step(1, StepStatus::Done).unwrap();
        mgr.update_step(2, StepStatus::InProgress).unwrap();

        let plan = mgr.active_plan().unwrap();
        let ctx = plan.format_for_context();
        assert!(ctx.contains("Active Plan"));
        assert!(ctx.contains("✅"));
        assert!(ctx.contains("🔄"));
        assert!(ctx.contains("← current"));
        assert!(ctx.contains("1/3 done"));
    }

    #[test]
    fn test_step_status_from_str() {
        assert_eq!("done".parse::<StepStatus>().unwrap(), StepStatus::Done);
        assert_eq!("in_progress".parse::<StepStatus>().unwrap(), StepStatus::InProgress);
        assert_eq!("pending".parse::<StepStatus>().unwrap(), StepStatus::Pending);
        assert_eq!("skipped".parse::<StepStatus>().unwrap(), StepStatus::Skipped);
        assert_eq!("failed".parse::<StepStatus>().unwrap(), StepStatus::Failed);
        assert!("invalid".parse::<StepStatus>().is_err());
    }

    #[test]
    fn test_update_nonexistent_step() {
        let mut mgr = PlanManager::new();
        mgr.create_plan("Test".to_string(), vec!["A".to_string()]);
        let result = mgr.update_step(99, StepStatus::Done);
        assert!(result.is_err());
    }

    #[test]
    fn test_update_no_plan() {
        let mut mgr = PlanManager::new();
        let result = mgr.update_step(1, StepStatus::Done);
        assert!(result.is_err());
    }

    #[test]
    fn test_skip_no_plan() {
        let mut mgr = PlanManager::new();
        let result = mgr.skip_current();
        assert!(result.is_err());
    }

    #[test]
    fn test_reset() {
        let mut mgr = PlanManager::new();
        mgr.create_plan("Test".to_string(), vec!["A".to_string()]);
        assert!(mgr.active_plan().is_some());

        mgr = PlanManager::new();
        assert!(mgr.active_plan().is_none());
        assert_eq!(mgr.archived_count(), 0);
    }

    #[test]
    fn test_single_in_progress_constraint() {
        let mut mgr = PlanManager::new();
        mgr.create_plan(
            "Test".to_string(),
            vec!["A".to_string(), "B".to_string(), "C".to_string()],
        );

        // Set step 1 to in_progress
        mgr.update_step(1, StepStatus::InProgress).unwrap();
        assert_eq!(mgr.active_plan().unwrap().steps[0].status, StepStatus::InProgress);

        // Set step 2 to in_progress — step 1 should be reset to Pending
        mgr.update_step(2, StepStatus::InProgress).unwrap();
        assert_eq!(mgr.active_plan().unwrap().steps[0].status, StepStatus::Pending);
        assert_eq!(mgr.active_plan().unwrap().steps[1].status, StepStatus::InProgress);

        // Set step 3 to in_progress — step 2 should be reset to Pending
        mgr.update_step(3, StepStatus::InProgress).unwrap();
        assert_eq!(mgr.active_plan().unwrap().steps[1].status, StepStatus::Pending);
        assert_eq!(mgr.active_plan().unwrap().steps[2].status, StepStatus::InProgress);
    }
}
