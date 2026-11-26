#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::sync::Arc;

use futures::StreamExt;
use rig::agent::{MultiTurnStreamItem, StreamingError, StreamingResult};
use rig::completion::{PromptError, message::ToolResultContent};
use rig::streaming::{StreamedAssistantContent, StreamedUserContent, StreamingPrompt};
use u_forge_core::ReasoningPolicy;
use u_forge_core::lemonade::{
    ChatEvent, ChatEventSink, ChatRunContext, ChatRunError, ChatRunHandle, ChatRunOutcome,
    ChatUsage, GpuResourceManager, LlmGuard,
};

use crate::agent::{AgentParams, GraphAgent, HistoryMessage};
use crate::budget::{BudgetController, BudgetTermination};

pub struct AgentRunRequest {
    pub model_id: String,
    pub prompt: String,
    pub history: Vec<HistoryMessage>,
    pub reasoning: ReasoningPolicy,
    pub params: AgentParams,
    pub context: ChatRunContext,
}

impl GraphAgent {
    /// Start the whole Rig operation, including profile acquisition, on Tokio.
    pub fn start_run(self: Arc<Self>, request: AgentRunRequest) -> ChatRunHandle {
        let parent = request.context.cancellation.clone();
        request.context.start(move |sink, gpu| async move {
            let (budget, history) =
                self.prepare_budget(&request.prompt, &request.history, &request.params);
            let agent = self.build_agent_with_params(
                &request.model_id,
                request.reasoning,
                &request.params,
                parent,
                budget.clone(),
            );
            let rig_history: Vec<_> = history
                .iter()
                .map(|m| match m.role.as_str() {
                    "assistant" => rig::completion::message::Message::assistant(&m.content),
                    "system" => rig::completion::message::Message::system(&m.content),
                    _ => rig::completion::message::Message::user(&m.content),
                })
                .collect();
            let gpu = gpu.as_ref();
            let guard = acquire_gpu(gpu).await;
            let stream = agent
                .stream_prompt(&request.prompt)
                .history(rig_history)
                .max_turns(request.params.max_tool_turns)
                .await;
            drive_turns(stream, &sink, &budget, gpu, guard).await
        })
    }
}

async fn acquire_gpu(gpu: Option<&Arc<GpuResourceManager>>) -> Option<LlmGuard> {
    match gpu {
        Some(gpu) => Some(gpu.begin_llm().await),
        None => None,
    }
}

fn agent_error(
    error: StreamingError,
    stop: Option<BudgetTermination>,
    diagnostics: u_forge_core::lemonade::AgentBudgetDiagnostics,
) -> ChatRunError {
    // Only a deliberate hook stop can turn a Rig error into a policy outcome.
    // A transport/tool failure must not inherit a pending repeat observation.
    if matches!(&error, StreamingError::Prompt(e) if matches!(**e, PromptError::PromptCancelled { .. }))
    {
        match stop {
            Some(BudgetTermination::Budget(reason)) => {
                return ChatRunError::ContextOverflow {
                    reason,
                    diagnostics,
                };
            }
            Some(BudgetTermination::Repeat(reason)) => {
                return ChatRunError::RepeatStop {
                    reason,
                    diagnostics,
                };
            }
            None => (),
        }
    }
    match error {
        StreamingError::Completion(_) => ChatRunError::Provider(error.to_string()),
        StreamingError::Prompt(ref e) if matches!(**e, PromptError::CompletionError(_)) => {
            ChatRunError::Provider(error.to_string())
        }
        _ => ChatRunError::Agent(error.to_string()),
    }
}

async fn drive_turns<R>(
    mut stream: StreamingResult<R>,
    sink: &ChatEventSink,
    budget: &BudgetController,
    gpu: Option<&Arc<GpuResourceManager>>,
    mut guard: Option<LlmGuard>,
) -> ChatRunOutcome {
    let mut pending_tools = HashSet::new();
    loop {
        // A ready stream may yield multiple items in one executor poll. Do not
        // poll Rig into a write tool after the consumer has gone away.
        sink.check_open()?;
        let item = stream
            .next()
            .await
            .ok_or(ChatRunError::Incomplete("Agent"))?
            .map_err(|e| agent_error(e, budget.termination(), budget.diagnostics()))?;
        match item {
            MultiTurnStreamItem::StreamAssistantItem(content) => match content {
                StreamedAssistantContent::Text(t) => {
                    sink.send(ChatEvent::TextDelta(t.text)).await?
                }
                StreamedAssistantContent::Reasoning(r) => {
                    for chunk in &r.content {
                        if let rig::completion::message::ReasoningContent::Text { text, .. } = chunk
                        {
                            sink.send(ChatEvent::ReasoningDelta(text.clone())).await?;
                        }
                    }
                }
                StreamedAssistantContent::ReasoningDelta { reasoning, .. } => {
                    sink.send(ChatEvent::ReasoningDelta(reasoning)).await?
                }
                StreamedAssistantContent::ToolCall {
                    tool_call,
                    internal_call_id,
                } => {
                    // Rig executes the batch on subsequent polls. No LLM guard
                    // may be held while an embedding tool waits for the GPU.
                    guard.take();
                    pending_tools.insert(internal_call_id.clone());
                    let args_display = serde_json::to_string_pretty(&tool_call.function.arguments)
                        .unwrap_or_else(|_| tool_call.function.arguments.to_string());
                    sink.send(ChatEvent::ToolCallStart {
                        internal_id: internal_call_id,
                        name: tool_call.function.name,
                        args_display,
                    })
                    .await?;
                }
                _ => (),
            },
            MultiTurnStreamItem::StreamUserItem(StreamedUserContent::ToolResult {
                tool_result,
                internal_call_id,
            }) => {
                let content = tool_result
                    .content
                    .iter()
                    .filter_map(ToolResultContent::as_text)
                    .collect::<Vec<_>>()
                    .join("\n");
                let batch_done =
                    pending_tools.remove(&internal_call_id) && pending_tools.is_empty();
                sink.send(ChatEvent::ToolResult {
                    internal_id: internal_call_id,
                    content,
                })
                .await?;
                if batch_done {
                    guard = acquire_gpu(gpu).await;
                }
            }
            MultiTurnStreamItem::FinalResponse(response) => {
                let usage = response.usage();
                if usage.total_tokens > 0 {
                    sink.send(ChatEvent::Usage(ChatUsage {
                        prompt_tokens: usage.input_tokens.min(u32::MAX as u64) as u32,
                        completion_tokens: usage.output_tokens.min(u32::MAX as u64) as u32,
                        total_tokens: usage.total_tokens.min(u32::MAX as u64) as u32,
                    }))
                    .await?;
                }
                sink.send(ChatEvent::AgentDiagnostics(budget.diagnostics()))
                    .await?;
                return Ok(Some(response.output().to_string()));
            }
            _ => (),
        }
    }
}
