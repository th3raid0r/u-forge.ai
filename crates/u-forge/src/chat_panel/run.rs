//! Transport-neutral presentation reducer for one Assistant run.
//!
//! Agent and direct-provider loops normalize their events into
//! [`ChatRunEvent`]. This module is the only place that mutates incremental
//! response rows or performs terminal cleanup and persistence.

use super::*;
use u_forge_core::{
    lemonade::{ChatEvent, ChatRunError, ChatRunOutcome},
    queue::InferenceError,
};

#[derive(Debug)]
pub(super) enum ChatRunEvent {
    ReasoningDelta(String),
    TextDelta(String),
    ToolCallStart {
        internal_id: String,
        name: String,
        args_display: String,
    },
    ToolResult {
        internal_id: String,
        content: String,
    },
    Terminal(ChatRunTerminal),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ChatRunTerminal {
    Success {
        full_text: Option<String>,
    },
    BudgetStop {
        reason: String,
        model_calls: usize,
        request_tokens: usize,
        tool_output_tokens: usize,
    },
    RepeatStop {
        reason: String,
        model_calls: usize,
    },
    Cancelled,
    Superseded,
    RuntimeFailure(String),
    AgentFailure(String),
    ProviderFailure(String),
    Unavailable,
    ReceiverClosed,
    ChannelClosed {
        transport: &'static str,
    },
}

impl ChatRunEvent {
    pub(super) fn from_producer(event: ChatEvent) -> Option<Self> {
        Some(match event {
            ChatEvent::ReasoningDelta(text) => Self::ReasoningDelta(text),
            ChatEvent::TextDelta(text) => Self::TextDelta(text),
            ChatEvent::ToolCallStart {
                internal_id,
                name,
                args_display,
            } => Self::ToolCallStart {
                internal_id,
                name,
                args_display,
            },
            ChatEvent::ToolResult {
                internal_id,
                content,
            } => Self::ToolResult {
                internal_id,
                content,
            },
            ChatEvent::Usage(usage) => {
                tracing::debug!(total_tokens = usage.total_tokens, "Chat usage");
                return None;
            }
            ChatEvent::AgentDiagnostics(stats) => {
                tracing::debug!(?stats, "Agent request completed");
                return None;
            }
        })
    }
}

impl From<ChatRunOutcome> for ChatRunTerminal {
    fn from(outcome: ChatRunOutcome) -> Self {
        match outcome {
            Ok(full_text) => Self::Success { full_text },
            Err(ChatRunError::ContextOverflow {
                reason,
                diagnostics,
            }) => Self::BudgetStop {
                reason,
                model_calls: diagnostics.model_calls,
                request_tokens: diagnostics.request_tokens,
                tool_output_tokens: diagnostics.tool_output_tokens,
            },
            Err(ChatRunError::RepeatStop {
                reason,
                diagnostics,
            }) => Self::RepeatStop {
                reason,
                model_calls: diagnostics.model_calls,
            },
            Err(ChatRunError::Interrupted(InferenceError::Cancelled)) => Self::Cancelled,
            Err(ChatRunError::Interrupted(InferenceError::Superseded)) => Self::Superseded,
            Err(ChatRunError::Interrupted(e)) => Self::ProviderFailure(e.to_string()),
            Err(ChatRunError::Runtime(e)) => Self::RuntimeFailure(e),
            Err(ChatRunError::Agent(e)) => Self::AgentFailure(e),
            Err(ChatRunError::Provider(e) | ChatRunError::DriverLost(e)) => {
                Self::ProviderFailure(e)
            }
            Err(ChatRunError::Incomplete(transport)) => Self::ChannelClosed { transport },
            Err(ChatRunError::ReceiverClosed) => Self::ReceiverClosed,
        }
    }
}

impl ChatRunTerminal {
    fn assistant_text(&self) -> Option<String> {
        match self {
            Self::BudgetStop {
                reason,
                model_calls,
                request_tokens,
                tool_output_tokens,
            } => Some(format!(
                "\n[Agent budget stop: {reason} Used {model_calls} model call(s), \
                 {request_tokens} request tokens, and {tool_output_tokens} tool-output tokens.]"
            )),
            Self::RepeatStop {
                reason,
                model_calls,
            } => Some(format!(
                "\n[Agent repeat stop: {reason} Used {model_calls} model call(s).]"
            )),
            Self::Cancelled => Some("[Cancelled]".to_string()),
            Self::Superseded => Some("[Superseded]".to_string()),
            Self::RuntimeFailure(error) => Some(format!("Runtime profile reload failed: {error}")),
            Self::AgentFailure(error) => Some(format!("\n[Agent error: {error}]")),
            Self::ProviderFailure(error) => Some(format!("\n[Error: {error}]")),
            Self::Unavailable => {
                Some("Chat unavailable — Lemonade Server not connected.".to_string())
            }
            Self::ReceiverClosed => Some("[Chat receiver closed]".into()),
            Self::ChannelClosed { transport } => Some(format!(
                "\n[{transport} stream closed without a terminal event]"
            )),
            Self::Success { .. } => None,
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct ChatRunReducer {
    pub(super) generation: u64,
    terminal: bool,
}

impl ChatRunReducer {
    pub(super) fn begin(&mut self) {
        self.generation += 1;
        self.terminal = false;
    }

    fn admit(&mut self, event: ChatRunEvent) -> Option<ChatRunEvent> {
        if self.terminal {
            return None;
        }
        if matches!(event, ChatRunEvent::Terminal(_)) {
            self.terminal = true;
        }
        Some(event)
    }
}

impl ChatPanel {
    pub(crate) fn shutdown_run(&mut self, cx: &mut App) {
        let parent = self.stream_cancellation.take();
        if let Some(parent) = &parent {
            parent.cancel();
        }
        // No GPUI work is needed for producer termination. Quit observers
        // have a short polling deadline, so join before returning to GPUI.
        for completion in self
            .retired_runs
            .drain(..)
            .chain(self.stream_completion.take())
        {
            let _ = self.tokio_rt.block_on(completion);
        }
        if parent.is_some()
            && let Some(ChatRunEvent::Terminal(terminal)) = self
                .run_reducer
                .admit(ChatRunEvent::Terminal(ChatRunTerminal::Cancelled))
        {
            self.apply_run_terminal(terminal, cx);
        }
    }

    pub(super) fn apply_generation_event(
        &mut self,
        generation: u64,
        event: ChatRunEvent,
        cx: &mut Context<Self>,
    ) {
        if self.run_reducer.generation == generation {
            self.apply_run_event(event, cx);
        }
    }

    pub(super) fn apply_run_event(&mut self, event: ChatRunEvent, cx: &mut Context<Self>) -> bool {
        let Some(event) = self.run_reducer.admit(event) else {
            return true;
        };
        match event {
            ChatRunEvent::ReasoningDelta(delta) => {
                self.take_pending_assistant(false, cx);
                let message = self.streaming_thinking.clone().unwrap_or_else(|| {
                    let message =
                        self.push_text_message(ChatMessageRole::Thinking, String::new(), cx);
                    self.streaming_thinking = Some(message.clone());
                    message
                });
                message.update(cx, |message, cx| message.append_text(&delta, cx));
                false
            }
            ChatRunEvent::TextDelta(delta) => {
                if delta.is_empty() {
                    return false;
                }
                self.streaming_thinking = None;
                let message = self.ensure_assistant_message(cx);
                message.update(cx, |message, cx| message.append_text(&delta, cx));
                false
            }
            ChatRunEvent::ToolCallStart {
                internal_id,
                name,
                args_display,
            } => {
                self.take_pending_assistant(false, cx);
                let message =
                    self.push_tool_call_message(internal_id.clone(), name, args_display, cx);
                self.streaming_tool_calls.insert(internal_id, message);
                // Text after a tool call must create a row after the tool row.
                self.streaming_assistant = None;
                self.streaming_thinking = None;
                cx.notify();
                false
            }
            ChatRunEvent::ToolResult {
                internal_id,
                content,
            } => {
                if let Some(message) = self.streaming_tool_calls.get(&internal_id).cloned() {
                    message.update(cx, |message, cx| message.set_tool_result(content, cx));
                }
                false
            }
            ChatRunEvent::Terminal(terminal) => {
                self.apply_run_terminal(terminal, cx);
                cx.notify();
                true
            }
        }
    }

    fn ensure_assistant_message(&mut self, cx: &mut App) -> Entity<ChatMessageView> {
        // A later reasoning block splits the row, but does not erase the
        // fact that assistant text was already delivered in this tool turn.
        self.streaming_assistant
            .clone()
            .filter(|row| Some(row) == self.messages.last())
            .unwrap_or_else(|| {
                let message = self.take_pending_assistant(true, cx).unwrap_or_else(|| {
                    self.push_text_message(ChatMessageRole::Assistant, String::new(), cx)
                });
                self.streaming_assistant = Some(message.clone());
                message
            })
    }

    fn apply_run_terminal(&mut self, terminal: ChatRunTerminal, cx: &mut App) {
        if let ChatRunTerminal::RuntimeFailure(error) = &terminal {
            self.profile_reload_state = ProfileReloadState::Failed(format!(
                "Could not load the selected Assistant model: {error}"
            ));
        }
        if let ChatRunTerminal::Success { full_text } = &terminal {
            let fallback = full_text
                .as_ref()
                .filter(|text| !text.trim().is_empty())
                .cloned();
            let used_fallback = self.streaming_assistant.is_none() && fallback.is_some();
            if self.streaming_assistant.is_none()
                && let Some(text) = fallback
            {
                let message = self.ensure_assistant_message(cx);
                message.update(cx, |message, cx| message.replace_text(text, cx));
            }
            tracing::info!(
                model = self
                    .available_models
                    .get(self.selected_model_idx)
                    .map(|model| model.model_id.as_str())
                    .unwrap_or("unknown"),
                backend = self
                    .available_models
                    .get(self.selected_model_idx)
                    .and_then(|model| model.backend.as_deref())
                    .unwrap_or("implicit"),
                streamed_text = !used_fallback && self.streaming_assistant.is_some(),
                terminal_fallback = used_fallback,
                "Assistant stream reached a successful terminal event"
            );
        } else if let Some(text) = terminal.assistant_text() {
            let message = self.ensure_assistant_message(cx);
            message.update(cx, |message, cx| match terminal {
                ChatRunTerminal::AgentFailure(_)
                | ChatRunTerminal::ProviderFailure(_)
                | ChatRunTerminal::RuntimeFailure(_) => message.append_error(&text, cx),
                ChatRunTerminal::Cancelled | ChatRunTerminal::Superseded
                    if message.text().is_empty() =>
                {
                    message.replace_text(text, cx);
                }
                ChatRunTerminal::Cancelled | ChatRunTerminal::Superseded => {
                    message.append_text(&format!("\n{text}"), cx);
                }
                _ => message.append_text(&text, cx),
            });
        }
        self.finalize_run(cx);
    }

    fn finalize_run(&mut self, cx: &mut App) {
        if let Some(pending) = self.pending_assistant.take() {
            pending.update(cx, |message, cx| {
                message.replace_text("No response was received.", cx);
            });
        }
        self.pending_animation_task.take();
        self.streaming = false;
        self.stream_task = None;
        self.stream_completion = None;
        self.queued_send = None;
        self.stream_cancellation = None;
        self.streaming_thinking = None;
        self.streaming_assistant = None;
        self.streaming_tool_calls.clear();
        self.save_current_session(cx);
    }

    /// Consume the pending row. Text responses can reuse it; reasoning and
    /// tool events remove it so chronological message ordering stays exact.
    fn take_pending_assistant(
        &mut self,
        reuse_for_text: bool,
        cx: &mut App,
    ) -> Option<Entity<ChatMessageView>> {
        let pending = self.pending_assistant.take();
        self.pending_animation_task.take();
        let pending = pending?;
        if reuse_for_text {
            pending.update(cx, |message, cx| message.replace_text("", cx));
            return Some(pending);
        }
        if let Some(index) = self
            .messages
            .iter()
            .position(|message| message.entity_id() == pending.entity_id())
        {
            self.messages.remove(index);
            self.list_state.reset(self.messages.len());
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reducer_admits_exactly_one_terminal_event() {
        let mut reducer = ChatRunReducer::default();
        reducer.begin();
        assert!(
            reducer
                .admit(ChatRunEvent::Terminal(ChatRunTerminal::Cancelled))
                .is_some()
        );
        assert!(
            reducer
                .admit(ChatRunEvent::Terminal(ChatRunTerminal::Superseded))
                .is_none()
        );
        assert!(
            reducer
                .admit(ChatRunEvent::TextDelta("late".to_string()))
                .is_none()
        );
    }

    #[test]
    fn terminal_reasons_remain_distinguishable() {
        let reasons = [
            ChatRunTerminal::Success { full_text: None },
            ChatRunTerminal::BudgetStop {
                reason: "budget".to_string(),
                model_calls: 1,
                request_tokens: 2,
                tool_output_tokens: 3,
            },
            ChatRunTerminal::RepeatStop {
                reason: "repeat".to_string(),
                model_calls: 1,
            },
            ChatRunTerminal::Cancelled,
            ChatRunTerminal::Superseded,
            ChatRunTerminal::RuntimeFailure("runtime".to_string()),
            ChatRunTerminal::AgentFailure("agent".to_string()),
            ChatRunTerminal::ProviderFailure("provider".to_string()),
            ChatRunTerminal::Unavailable,
            ChatRunTerminal::ReceiverClosed,
            ChatRunTerminal::ChannelClosed { transport: "Agent" },
        ];
        for left in 0..reasons.len() {
            for right in 0..reasons.len() {
                assert_eq!(reasons[left] == reasons[right], left == right);
            }
        }
    }

    #[test]
    fn reducer_preserves_tool_event_order() {
        let mut reducer = ChatRunReducer::default();
        reducer.begin();
        let start = reducer
            .admit(ChatRunEvent::ToolCallStart {
                internal_id: "1".to_string(),
                name: "search".to_string(),
                args_display: "{}".to_string(),
            })
            .unwrap();
        let result = reducer
            .admit(ChatRunEvent::ToolResult {
                internal_id: "1".to_string(),
                content: "done".to_string(),
            })
            .unwrap();
        assert!(matches!(start, ChatRunEvent::ToolCallStart { .. }));
        assert!(matches!(result, ChatRunEvent::ToolResult { .. }));
    }
}
