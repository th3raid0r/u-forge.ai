use futures::StreamExt;
use rig::agent::MultiTurnStreamItem;
use rig::completion::message::ToolResultContent;
use rig::streaming::{StreamedAssistantContent, StreamedUserContent, StreamingPrompt};
use tokio::sync::mpsc;
use u_forge_core::queue::CancellationToken;

use crate::agent::{AgentParams, GraphAgent, HistoryMessage};
use crate::budget;

/// Compatibility name for the shared event contract consumed above direct
/// HTTP and Rig adapters.
pub type AgentStreamEvent = u_forge_core::lemonade::ChatEvent;

impl GraphAgent {
    /// Run the agent loop with streaming output.
    ///
    /// Returns a [`mpsc::Receiver`] that yields [`AgentStreamEvent`]s as the
    /// agent streams text, calls tools, and receives tool results. The channel
    /// closes after a terminal `Finished` or `FatalError` event.
    pub async fn prompt_stream(
        &self,
        model_id: &str,
        user_message: &str,
        history: &[HistoryMessage],
        reasoning_enabled: bool,
    ) -> mpsc::Receiver<AgentStreamEvent> {
        self.prompt_stream_with_params(
            model_id,
            user_message,
            history,
            reasoning_enabled,
            self.params.clone(),
        )
        .await
    }

    /// Stream using the complete effective profile for the selected model.
    ///
    /// This keeps picker changes coherent: model, context/generation limits,
    /// sampling controls, reasoning, and tool-loop ceiling change together.
    pub async fn prompt_stream_with_params(
        &self,
        model_id: &str,
        user_message: &str,
        history: &[HistoryMessage],
        reasoning_enabled: bool,
        params: AgentParams,
    ) -> mpsc::Receiver<AgentStreamEvent> {
        self.prompt_stream_with_profile(
            model_id,
            user_message,
            history,
            if reasoning_enabled {
                u_forge_core::ReasoningPolicy::Enabled
            } else {
                u_forge_core::ReasoningPolicy::Disabled
            },
            params,
            None,
            false,
        )
        .await
    }

    /// Stream while retaining runtime and device coordination in the producer.
    #[allow(clippy::too_many_arguments)]
    pub async fn prompt_stream_with_profile(
        &self,
        model_id: &str,
        user_message: &str,
        history: &[HistoryMessage],
        reasoning: u_forge_core::ReasoningPolicy,
        params: AgentParams,
        runtime_lease: Option<u_forge_core::LemonadeRuntimeLease>,
        uses_gpu: bool,
    ) -> mpsc::Receiver<AgentStreamEvent> {
        self.prompt_stream_with_profile_and_cancellation(
            model_id,
            user_message,
            history,
            reasoning,
            params,
            runtime_lease,
            uses_gpu,
            CancellationToken::new(),
        )
        .await
    }

    /// Stream a complete agent/tool operation under one parent token.
    #[allow(clippy::too_many_arguments)]
    pub async fn prompt_stream_with_profile_and_cancellation(
        &self,
        model_id: &str,
        user_message: &str,
        history: &[HistoryMessage],
        reasoning: u_forge_core::ReasoningPolicy,
        params: AgentParams,
        runtime_lease: Option<u_forge_core::LemonadeRuntimeLease>,
        uses_gpu: bool,
        cancellation: CancellationToken,
    ) -> mpsc::Receiver<AgentStreamEvent> {
        let (tx, rx) = mpsc::channel(64);

        let (budget, selected_history) = self.prepare_budget(user_message, history, &params);
        let agent = self.build_agent_with_params(
            model_id,
            reasoning,
            &params,
            cancellation.clone(),
            budget.clone(),
        );
        let max_turns = params.max_tool_turns;
        let gpu = uses_gpu.then(|| self.gpu.clone()).flatten();

        let user_message = user_message.to_string();
        // Convert HistoryMessage → rig::completion::message::Message.
        let rig_history: Vec<rig::completion::message::Message> = selected_history
            .iter()
            .map(|m| match m.role.as_str() {
                "assistant" => rig::completion::message::Message::assistant(&m.content),
                "system" => rig::completion::message::Message::system(&m.content),
                _ => rig::completion::message::Message::user(&m.content),
            })
            .collect();

        tokio::spawn(async move {
            let _runtime_lease = runtime_lease;
            let mut gpu_guard = match &gpu {
                Some(gpu) => tokio::select! {
                    _ = cancellation.cancelled() => return,
                    guard = gpu.begin_llm() => Some(guard),
                },
                None => None,
            };
            let mut stream = agent
                .stream_prompt(&user_message)
                .history(rig_history)
                .max_turns(max_turns)
                .await;

            let mut final_text = String::new();

            // `'stream` labels the outer loop so every send-on-error can break it directly.
            // If the receiver is dropped (user closed the chat panel, app exited) we stop
            // driving the rig stream instead of burning LLM inference and potentially
            // running write tools the caller will never observe.
            'stream: loop {
                let item = tokio::select! {
                    _ = cancellation.cancelled() => break 'stream,
                    item = stream.next() => item,
                };
                let Some(item) = item else { break 'stream };
                match item {
                    Ok(MultiTurnStreamItem::StreamAssistantItem(content)) => match content {
                        StreamedAssistantContent::Text(t) => {
                            final_text.push_str(&t.text);
                            if tx.send(AgentStreamEvent::TextDelta(t.text)).await.is_err() {
                                break 'stream;
                            }
                        }
                        StreamedAssistantContent::ToolCall {
                            tool_call,
                            internal_call_id,
                        } => {
                            // Rig executes the tool on the next poll. Release
                            // the device now so embedding tools cannot deadlock
                            // behind this LLM turn's GPU guard.
                            gpu_guard.take();
                            let args_display =
                                serde_json::to_string_pretty(&tool_call.function.arguments)
                                    .unwrap_or_else(|_| tool_call.function.arguments.to_string());
                            if tx
                                .send(AgentStreamEvent::ToolCallStart {
                                    internal_id: internal_call_id,
                                    name: tool_call.function.name,
                                    args_display,
                                })
                                .await
                                .is_err()
                            {
                                break 'stream;
                            }
                            // FinalResponse is scoped to the post-tool turn.
                            // Keep the fallback buffer scoped the same way so
                            // terminal-only text never repeats pre-tool prose.
                            final_text.clear();
                        }
                        StreamedAssistantContent::Reasoning(r) => {
                            // Full reasoning block (some providers emit this instead of deltas).
                            for chunk in &r.content {
                                if let rig::completion::message::ReasoningContent::Text {
                                    text, ..
                                } = chunk
                                    && tx
                                        .send(AgentStreamEvent::ReasoningDelta(text.clone()))
                                        .await
                                        .is_err()
                                {
                                    break 'stream;
                                }
                            }
                        }
                        StreamedAssistantContent::ReasoningDelta { reasoning, .. } => {
                            if tx
                                .send(AgentStreamEvent::ReasoningDelta(reasoning))
                                .await
                                .is_err()
                            {
                                break 'stream;
                            }
                        }
                        // Final(R) and ToolCallDelta are ignored — text arrives via TextDelta.
                        _ => {}
                    },
                    Ok(MultiTurnStreamItem::StreamUserItem(content)) => match content {
                        StreamedUserContent::ToolResult {
                            tool_result,
                            internal_call_id,
                        } => {
                            let result_text = tool_result
                                .content
                                .iter()
                                .filter_map(|c| {
                                    if let ToolResultContent::Text(t) = c {
                                        Some(t.text.as_str())
                                    } else {
                                        None
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            if tx
                                .send(AgentStreamEvent::ToolResult {
                                    internal_id: internal_call_id,
                                    content: result_text,
                                })
                                .await
                                .is_err()
                            {
                                break 'stream;
                            }
                            // The next poll begins the following LLM turn.
                            if let Some(gpu) = &gpu {
                                gpu_guard = tokio::select! {
                                    _ = cancellation.cancelled() => break 'stream,
                                    guard = gpu.begin_llm() => Some(guard),
                                };
                            }
                        }
                    },
                    Ok(MultiTurnStreamItem::FinalResponse(resp)) => {
                        // FinalResponse carries the full aggregated text for the
                        // last turn. Use it if we didn't accumulate via TextDelta.
                        let text = if final_text.is_empty() {
                            resp.output().to_string()
                        } else {
                            final_text.clone()
                        };
                        let usage = resp.usage();
                        if usage.total_tokens > 0
                            && tx
                                .send(AgentStreamEvent::Usage(u_forge_core::lemonade::ChatUsage {
                                    prompt_tokens: usage.input_tokens.min(u32::MAX as u64) as u32,
                                    completion_tokens: usage.output_tokens.min(u32::MAX as u64)
                                        as u32,
                                    total_tokens: usage.total_tokens.min(u32::MAX as u64) as u32,
                                }))
                                .await
                                .is_err()
                        {
                            break 'stream;
                        }
                        let diagnostics = budget.diagnostics();
                        tracing::info!(
                            model_calls = diagnostics.model_calls,
                            request_tokens = diagnostics.request_tokens,
                            assistant_output_tokens = diagnostics.assistant_output_tokens,
                            tool_argument_tokens = diagnostics.tool_argument_tokens,
                            tool_output_tokens = diagnostics.tool_output_tokens,
                            estimation_fallback = diagnostics.estimation_fallback,
                            "Agent request completed"
                        );
                        if tx
                            .send(AgentStreamEvent::AgentDiagnostics(diagnostics))
                            .await
                            .is_err()
                        {
                            break 'stream;
                        }
                        let _ = tx
                            .send(AgentStreamEvent::Finished {
                                reason: u_forge_core::lemonade::ChatTerminalReason::AgentComplete,
                                full_text: Some(text),
                            })
                            .await;
                        break 'stream;
                    }
                    Ok(_) => {
                        // Non-exhaustive: ignore any new MultiTurnStreamItem variants.
                    }
                    Err(e) => {
                        let diagnostics = budget.diagnostics();
                        match budget.termination() {
                            Some(budget::BudgetTermination::Budget(reason)) => {
                                tracing::warn!(
                                    %reason,
                                    model_calls = diagnostics.model_calls,
                                    request_tokens = diagnostics.request_tokens,
                                    tool_output_tokens = diagnostics.tool_output_tokens,
                                    estimation_fallback = diagnostics.estimation_fallback,
                                    "Agent request stopped by budget"
                                );
                                let _ = tx
                                    .send(AgentStreamEvent::BudgetTerminated {
                                        reason,
                                        diagnostics,
                                    })
                                    .await;
                            }
                            Some(budget::BudgetTermination::Repeat(reason)) => {
                                tracing::warn!(
                                    %reason,
                                    model_calls = diagnostics.model_calls,
                                    request_tokens = diagnostics.request_tokens,
                                    tool_output_tokens = diagnostics.tool_output_tokens,
                                    "Agent request stopped by repeat guard"
                                );
                                let _ = tx
                                    .send(AgentStreamEvent::RepeatTerminated {
                                        reason,
                                        diagnostics,
                                    })
                                    .await;
                            }
                            None => {
                                let _ = tx.send(AgentStreamEvent::FatalError(e.to_string())).await;
                            }
                        }
                        break 'stream;
                    }
                }
            }
        });

        rx
    }
}
