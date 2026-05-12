//! Built-in middleware implementations.
//!
//! These are the standard middleware that ship with Daedalus.
//! Each can be enabled/disabled via configuration.
//!
//! ## Turn Middleware
//!
//! - [`tracing::TracingTurnMiddleware`] — Trace + span lifecycle management
//! - [`logging::LoggingTurnMiddleware`] — Request/response structured logging
//! - [`memory::MemoryTurnMiddleware`] — User/assistant message storage and retrieval
//! - [`cost::CostTurnMiddleware`] — Cumulative token usage accounting
//! - [`metrics::MetricsTurnMiddleware`] — Turn timing and round counting
//!
//! ## Tool Middleware
//!
//! - [`tracing::TracingToolMiddleware`] — Tool call span creation
//! - [`permission::PermissionToolMiddleware`] — Tool call authorization
//! - [`confirmation::ConfirmationToolMiddleware`] — Interactive user approval for sensitive tools
//! - [`logging::LoggingToolMiddleware`] — Tool call structured logging
//! - [`event::EventToolMiddleware`] — CLI event emission (ToolCallStart/Complete)

pub mod tracing;
pub mod logging;
pub mod memory;
pub mod event;
pub mod permission;
pub mod permission_rules;
pub mod confirmation;
pub mod cost;
pub mod metrics;
