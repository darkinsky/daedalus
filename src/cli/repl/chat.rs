//! Chat turn execution — sends user input to the agent and renders the response.
//!
//! Both plain-text and multimodal (image) chat flows share the same setup/teardown
//! logic via `ChatTurnContext`.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::agent::AgentMode;
use crate::llm::ChatResponse;
use crate::middleware::builtin::confirmation::ConfirmationReceiver;
use crate::middleware::builtin::cost::SharedSessionCost;
use crate::tools::ToolEventCallback;

use super::confirmation::prompt_user_confirmation;
use super::streaming::{build_tool_event_callback, StreamingState, TurnStatsCollector};
use super::super::render;

/// Shared setup state for a single chat turn (text or multimodal).
struct ChatTurnContext {
    spinner: Arc<indicatif::ProgressBar>,
    start: Instant,
    stats_collector: Arc<Mutex<TurnStatsCollector>>,
    streaming_state: Arc<Mutex<StreamingState>>,
    tool_callback: ToolEventCallback,
    confirm_handle: tokio::task::JoinHandle<()>,
}

impl ChatTurnContext {
    /// Set up everything needed for a chat turn: spinner, callbacks, confirmation handler.
    fn new(
        _agent: &dyn AgentMode,
        confirm_rx: &Arc<tokio::sync::Mutex<ConfirmationReceiver>>,
    ) -> (Self, ToolEventCallback) {
        let spinner = Arc::new(render::spinner());
        let start = Instant::now();
        let stats_collector = Arc::new(Mutex::new(TurnStatsCollector::default()));
        let streaming_state = Arc::new(Mutex::new(StreamingState::default()));
        let tool_callback = build_tool_event_callback(&spinner, &stats_collector, &streaming_state);

        // Spawn background confirmation handler
        let confirm_rx_clone = Arc::clone(confirm_rx);
        let confirm_spinner = Arc::clone(&spinner);
        let confirm_handle = tokio::spawn(async move {
            let mut rx = confirm_rx_clone.lock().await;
            while let Some(request) = rx.recv().await {
                confirm_spinner.finish_and_clear();
                tokio::task::spawn_blocking(move || {
                    let decision = prompt_user_confirmation(&request);
                    let _ = request.response_tx.send(decision);
                }).await.ok();
                confirm_spinner.reset_elapsed();
                confirm_spinner.set_message("Thinking\u{2026}");
                confirm_spinner.enable_steady_tick(std::time::Duration::from_millis(80));
            }
        });

        let cb_clone = Arc::clone(&tool_callback);

        let ctx = Self {
            spinner,
            start,
            stats_collector,
            streaming_state,
            tool_callback,
            confirm_handle,
        };
        (ctx, cb_clone)
    }

    /// Process the result of a chat turn (success or error) and render output.
    async fn finish(
        self,
        result: Result<ChatResponse, anyhow::Error>,
        agent: &mut dyn AgentMode,
        cost: &SharedSessionCost,
    ) {
        match result {
            Ok(chat_result) => {
                let elapsed = self.start.elapsed().as_secs_f64();
                self.spinner.finish_and_clear();
                self.confirm_handle.abort();

                agent.set_subagent_event_callback(None);

                let was_streamed = self.streaming_state
                    .lock()
                    .map(|s| s.content_was_streamed)
                    .unwrap_or(false);
                let reasoning_was_streamed = self.streaming_state
                    .lock()
                    .map(|s| s.reasoning_was_streamed)
                    .unwrap_or(false);

                if !was_streamed {
                    if !reasoning_was_streamed {
                        if let Some(ref reasoning) = chat_result.reasoning_content {
                            if !reasoning.is_empty() {
                                render::reasoning_content(reasoning);
                            }
                        }
                    }
                    render::response(&chat_result.content);
                }

                // Persist memory to disk after each successful turn
                agent.persist_memory().await;

                // Trigger Stop lifecycle hooks
                if let Some(hooks_config) = agent.hooks_config() {
                    let session_id = agent.session_id().to_string();
                    crate::hooks::run_stop_hooks(hooks_config, &session_id).await;
                }

                // Collect subagent stats and render turn summary
                let subagent_stats = self.stats_collector
                    .lock()
                    .map(|s| s.subagents.clone())
                    .unwrap_or_default();

                if subagent_stats.is_empty() {
                    render::response_footer(chat_result.usage.as_ref(), elapsed, agent.context_window());
                } else {
                    render::turn_summary(
                        chat_result.usage.as_ref(),
                        elapsed,
                        &subagent_stats
                            .iter()
                            .map(|s| render::SubagentUsageSummary {
                                agent_name: s.agent_name.clone(),
                                success: s.success,
                                tool_rounds: s.tool_rounds,
                                usage: s.usage.clone(),
                                elapsed_secs: s.elapsed_ms as f64 / 1000.0,
                            })
                            .collect::<Vec<_>>(),
                        agent.context_window(),
                    );

                    if let Ok(mut c) = cost.lock() {
                        for s in &subagent_stats {
                            c.add_subagent_usage(s.usage.as_ref());
                        }
                    }
                }
                println!();
            }
            Err(e) => {
                self.spinner.finish_and_clear();
                self.confirm_handle.abort();
                agent.set_subagent_event_callback(None);

                tracing::error!("Agent error: {}", e);
                render::error(&e);
            }
        }
    }
}

/// Send user input to the agent and render the response.
pub(crate) async fn handle_chat(
    input: &str,
    agent: &mut dyn AgentMode,
    cost: &SharedSessionCost,
    confirm_rx: &Arc<tokio::sync::Mutex<ConfirmationReceiver>>,
) {
    tracing::debug!("User input: {}", input);

    let (ctx, cb_clone) = ChatTurnContext::new(agent, confirm_rx);
    agent.set_subagent_event_callback(Some(Arc::clone(&ctx.tool_callback)));

    let result = agent.chat(input, Some(&cb_clone)).await;
    ctx.finish(result, agent, cost).await;
}

/// Handle a chat with a pre-built multimodal message.
pub(crate) async fn handle_chat_with_message(
    message: crate::llm::ChatMessage,
    agent: &mut dyn AgentMode,
    cost: &SharedSessionCost,
    confirm_rx: &Arc<tokio::sync::Mutex<ConfirmationReceiver>>,
) {
    let text_preview = if message.content.len() > 50 {
        format!("{}...", &message.content[..50])
    } else {
        message.content.clone()
    };
    tracing::debug!("User input (multimodal): {}", text_preview);

    let (ctx, cb_clone) = ChatTurnContext::new(agent, confirm_rx);
    agent.set_subagent_event_callback(Some(Arc::clone(&ctx.tool_callback)));

    let result = agent.chat_with_message(message, Some(&cb_clone)).await;
    ctx.finish(result, agent, cost).await;
}
