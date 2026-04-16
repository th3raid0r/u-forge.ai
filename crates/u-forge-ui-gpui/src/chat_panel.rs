use std::sync::Arc;

use gpui::{
    div, prelude::*, px, rgb, rgba, relative, Context, Entity, MouseButton, MouseDownEvent,
    ScrollHandle, Window,
};
use u_forge_core::{
    lemonade::{LemonadeChatProvider, SelectedModel},
    ChatMessage, ChatRequest, StreamToken,
};

use crate::text_field::{TextFieldView, TextSubmit};

// ── Chat message types ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum ChatRole {
    User,
    Assistant,
    /// Model thinking/reasoning — rendered separately from content.
    Thinking,
}

#[derive(Debug, Clone)]
struct ChatEntry {
    role: ChatRole,
    text: String,
}

// ── ChatPanel ───────────────────────────────────────────────────────────────

pub(crate) struct ChatPanel {
    /// The text input field for composing messages.
    input_field: Entity<TextFieldView>,
    /// When true, pressing Enter submits; Shift+Enter inserts a newline.
    /// When false, Enter inserts a newline; Shift+Enter (or button) submits.
    enter_to_submit: bool,
    /// Chat message history.
    messages: Vec<ChatEntry>,
    /// Whether a response is currently streaming.
    streaming: bool,
    /// Available LLM models (populated after Lemonade init).
    available_models: Vec<AvailableModel>,
    /// Index into `available_models` for the currently selected model.
    selected_model_idx: usize,
    /// Whether the model selector dropdown is open.
    model_dropdown_open: bool,
    /// The active chat provider (None until Lemonade is discovered).
    chat_provider: Option<LemonadeChatProvider>,
    /// System prompt from config.
    system_prompt: String,
    /// Max history turns to keep in context.
    max_history_turns: usize,
    /// Tokio runtime for async chat calls.
    tokio_rt: Arc<tokio::runtime::Runtime>,
    /// Subscription for the Enter-submit event from the input field.
    #[allow(dead_code)]
    submit_sub: gpui::Subscription,
    /// Scroll handle for the message area.
    scroll_handle: ScrollHandle,
}

/// A simplified model entry for the UI dropdown.
#[derive(Debug, Clone)]
pub(crate) struct AvailableModel {
    pub(crate) model_id: String,
    pub(crate) recipe: String,
    pub(crate) backend: Option<String>,
}

impl From<&SelectedModel> for AvailableModel {
    fn from(sel: &SelectedModel) -> Self {
        Self {
            model_id: sel.model_id.clone(),
            recipe: sel.recipe.clone(),
            backend: sel.backend.clone(),
        }
    }
}

impl ChatPanel {
    pub(crate) fn new(
        system_prompt: String,
        max_history_turns: usize,
        tokio_rt: Arc<tokio::runtime::Runtime>,
        cx: &mut Context<Self>,
    ) -> Self {
        let input_field = cx.new(|cx| {
            let mut field = TextFieldView::new(true, "Type a message...", cx);
            field.submit_on_enter = true;
            field
        });

        let submit_sub = cx.subscribe(&input_field, |this: &mut Self, _, _ev: &TextSubmit, cx| {
            this.do_send(cx);
        });

        Self {
            input_field,
            enter_to_submit: true,
            messages: Vec::new(),
            streaming: false,
            available_models: Vec::new(),
            selected_model_idx: 0,
            model_dropdown_open: false,
            chat_provider: None,
            system_prompt,
            max_history_turns,
            tokio_rt,
            submit_sub,
            scroll_handle: ScrollHandle::new(),
        }
    }

    /// Called from AppView after Lemonade init discovers LLM models.
    pub(crate) fn set_provider(
        &mut self,
        provider: LemonadeChatProvider,
        models: Vec<AvailableModel>,
    ) {
        self.chat_provider = Some(provider);
        self.available_models = models;
        self.selected_model_idx = 0;
    }

    /// Send the current input as a user message and start streaming the response.
    fn do_send(&mut self, cx: &mut Context<Self>) {
        let text = self.input_field.read(cx).content.trim().to_string();
        if text.is_empty() || self.streaming {
            return;
        }

        let provider = match &self.chat_provider {
            Some(p) => p.clone(),
            None => {
                self.messages.push(ChatEntry {
                    role: ChatRole::Assistant,
                    text: "Chat unavailable — Lemonade Server not connected.".to_string(),
                });
                cx.notify();
                return;
            }
        };

        // Clear input.
        self.input_field.update(cx, |field, cx| {
            field.set_content("", cx);
        });

        // Add user message to history.
        self.messages.push(ChatEntry {
            role: ChatRole::User,
            text: text.clone(),
        });
        self.streaming = true;
        cx.notify();

        // Build the message list for the API.
        let mut api_messages = Vec::new();
        if !self.system_prompt.is_empty() {
            api_messages.push(ChatMessage::system(&self.system_prompt));
        }

        // Include recent history (respecting max_history_turns).
        let history_entries: Vec<&ChatEntry> = self
            .messages
            .iter()
            .filter(|e| matches!(e.role, ChatRole::User | ChatRole::Assistant))
            .collect();
        let skip = if history_entries.len() > self.max_history_turns * 2 {
            history_entries.len() - self.max_history_turns * 2
        } else {
            0
        };
        for entry in history_entries.into_iter().skip(skip) {
            match entry.role {
                ChatRole::User => api_messages.push(ChatMessage::user(&entry.text)),
                ChatRole::Assistant => api_messages.push(ChatMessage::assistant(&entry.text)),
                _ => {}
            }
        }

        // Determine model override if user selected a different model.
        let model_override = if !self.available_models.is_empty() {
            Some(self.available_models[self.selected_model_idx].model_id.clone())
        } else {
            None
        };

        let mut req = ChatRequest::new(api_messages).with_thinking(true);
        if let Some(model) = model_override {
            req = req.with_model(model);
        }

        let tokio_rt = self.tokio_rt.clone();

        // Spawn a background task to drive the stream.
        cx.spawn(async move |this, cx| {
            let rx = cx
                .background_executor()
                .spawn(async move {
                    tokio_rt.block_on(async { provider.complete_stream(req) })
                })
                .await;

            // Add placeholder entries for thinking and content.
            this.update(cx, |view: &mut ChatPanel, cx| {
                view.messages.push(ChatEntry {
                    role: ChatRole::Thinking,
                    text: String::new(),
                });
                view.messages.push(ChatEntry {
                    role: ChatRole::Assistant,
                    text: String::new(),
                });
                cx.notify();
            })
            .ok();

            // Consume tokens from the stream.
            let mut rx = rx;
            loop {
                // Poll the receiver on the background executor so the main thread stays free.
                let token = cx
                    .background_executor()
                    .spawn(async move {
                        let result = rx.recv().await;
                        (rx, result)
                    })
                    .await;
                rx = token.0;
                let result = token.1;

                match result {
                    Some(Ok(StreamToken::Content(text))) => {
                        this.update(cx, |view: &mut ChatPanel, cx| {
                            if let Some(entry) = view
                                .messages
                                .iter_mut()
                                .rev()
                                .find(|e| matches!(e.role, ChatRole::Assistant))
                            {
                                entry.text.push_str(&text);
                            }
                            cx.notify();
                        })
                        .ok();
                    }
                    Some(Ok(StreamToken::Thinking(text))) => {
                        this.update(cx, |view: &mut ChatPanel, cx| {
                            if let Some(entry) = view
                                .messages
                                .iter_mut()
                                .rev()
                                .find(|e| matches!(e.role, ChatRole::Thinking))
                            {
                                entry.text.push_str(&text);
                            }
                            cx.notify();
                        })
                        .ok();
                    }
                    Some(Err(e)) => {
                        this.update(cx, |view: &mut ChatPanel, cx| {
                            if let Some(entry) = view
                                .messages
                                .iter_mut()
                                .rev()
                                .find(|e| matches!(e.role, ChatRole::Assistant))
                            {
                                entry.text.push_str(&format!("\n[Error: {e}]"));
                            }
                            view.streaming = false;
                            cx.notify();
                        })
                        .ok();
                        break;
                    }
                    None => {
                        // Stream finished.
                        this.update(cx, |view: &mut ChatPanel, cx| {
                            // Remove empty thinking entry if model didn't produce any.
                            view.messages.retain(|e| {
                                !matches!(e.role, ChatRole::Thinking) || !e.text.is_empty()
                            });
                            view.streaming = false;
                            cx.notify();
                        })
                        .ok();
                        break;
                    }
                }
            }
        })
        .detach();
    }

    /// Label for the currently selected model (or a placeholder).
    fn selected_model_label(&self) -> String {
        if self.available_models.is_empty() {
            return "No models".to_string();
        }
        let m = &self.available_models[self.selected_model_idx];
        let device = match m.recipe.as_str() {
            "flm" => "NPU",
            "llamacpp" => match m.backend.as_deref() {
                Some("rocm") | Some("vulkan") | Some("metal") => "GPU",
                _ => "CPU",
            },
            _ => "",
        };
        if device.is_empty() {
            m.model_id.clone()
        } else {
            format!("{} ({})", m.model_id, device)
        }
    }
}

impl Render for ChatPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let enter_to_submit = self.enter_to_submit;
        let streaming = self.streaming;
        let model_dropdown_open = self.model_dropdown_open;
        let has_provider = self.chat_provider.is_some();
        let model_label = self.selected_model_label();

        // Build message elements.
        let message_elements: Vec<_> = self
            .messages
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.text.is_empty())
            .map(|(i, entry)| {
                let (bg, label_color, label, text_color) = match entry.role {
                    ChatRole::User => (
                        rgb(0x313244),
                        rgba(0x89b4faff), // blue
                        "You",
                        rgba(0xcdd6f4ff),
                    ),
                    ChatRole::Assistant => (
                        rgb(0x1e1e2e),
                        rgba(0xa6e3a1ff), // green
                        "Assistant",
                        rgba(0xcdd6f4ff),
                    ),
                    ChatRole::Thinking => (
                        rgb(0x181825),
                        rgba(0xf9e2afff), // yellow
                        "Thinking",
                        rgba(0x6c7086ff), // dimmed
                    ),
                };

                div()
                    .id(gpui::ElementId::Name(format!("msg-{i}").into()))
                    .flex()
                    .flex_col()
                    .w_full()
                    .px_2()
                    .py_1()
                    .bg(bg)
                    .child(
                        div()
                            .text_xs()
                            .text_color(label_color)
                            .pb(px(2.0))
                            .child(label),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(text_color)
                            .child(entry.text.clone()),
                    )
            })
            .collect();

        // Streaming indicator.
        let streaming_indicator = if streaming {
            Some(
                div()
                    .id("streaming-indicator")
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(rgba(0xf9e2afff))
                    .child("Generating…"),
            )
        } else {
            None
        };

        // Model dropdown items.
        let model_items: Vec<_> = if model_dropdown_open {
            self.available_models
                .iter()
                .enumerate()
                .map(|(idx, m)| {
                    let is_selected = idx == self.selected_model_idx;
                    let device = match m.recipe.as_str() {
                        "flm" => " (NPU)",
                        "llamacpp" => match m.backend.as_deref() {
                            Some("rocm") | Some("vulkan") | Some("metal") => " (GPU)",
                            _ => " (CPU)",
                        },
                        _ => "",
                    };
                    let label = format!("{}{}", m.model_id, device);
                    div()
                        .id(gpui::ElementId::Name(format!("model-{idx}").into()))
                        .flex()
                        .items_center()
                        .h(px(24.0))
                        .px_2()
                        .text_xs()
                        .text_color(if is_selected {
                            rgba(0xcdd6f4ff)
                        } else {
                            rgba(0xa6adc8ff)
                        })
                        .when(is_selected, |el| el.bg(rgba(0x45475a88)))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgba(0x45475a66)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                                this.selected_model_idx = idx;
                                this.model_dropdown_open = false;
                                cx.notify();
                            }),
                        )
                        .child(label)
                })
                .collect()
        } else {
            Vec::new()
        };

        div()
            .id("chat-panel")
            .flex()
            .flex_col()
            .w_full()
            .h_full()
            .min_h_0()
            .bg(rgb(0x181825))
            .border_l_1()
            .border_color(rgb(0x313244))
            // ── Header: model selector ───────────────────────────────────────
            .child(
                div()
                    .id("chat-header")
                    .flex()
                    .flex_col()
                    .flex_none()
                    .w_full()
                    .border_b_1()
                    .border_color(rgb(0x313244))
                    .px_2()
                    .py_1()
                    // Model selector row
                    .child(
                        div()
                            .id("model-selector-row")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgba(0x6c7086ff))
                                    .child("Model:"),
                            )
                            .child(
                                div()
                                    .id("model-selector-btn")
                                    .flex()
                                    .items_center()
                                    .px_2()
                                    .h(px(22.0))
                                    .bg(rgb(0x313244))
                                    .border_1()
                                    .border_color(rgb(0x45475a))
                                    .rounded(px(3.0))
                                    .text_xs()
                                    .text_color(if has_provider {
                                        rgba(0xcdd6f4ff)
                                    } else {
                                        rgba(0x6c7086ff)
                                    })
                                    .cursor_pointer()
                                    .overflow_x_hidden()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(
                                            |this, _: &MouseDownEvent, _window, cx| {
                                                this.model_dropdown_open =
                                                    !this.model_dropdown_open;
                                                cx.notify();
                                            },
                                        ),
                                    )
                                    .child(model_label),
                            ),
                    )
                    // Model dropdown (conditional)
                    .when(model_dropdown_open, |header| {
                        header.child(
                            div()
                                .id("model-dropdown")
                                .flex()
                                .flex_col()
                                .w_full()
                                .bg(rgb(0x313244))
                                .border_1()
                                .border_color(rgb(0x45475a))
                                .rounded(px(3.0))
                                .mt_1()
                                .max_h(px(200.0))
                                .overflow_y_scroll()
                                .children(model_items),
                        )
                    }),
            )
            // ── Message area ─────────────────────────────────────────────────
            .child({
                let mut msg_area = div()
                    .id("chat-messages")
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .children(message_elements);
                if let Some(indicator) = streaming_indicator {
                    msg_area = msg_area.child(indicator);
                }
                msg_area.style().flex_grow = Some(1.0);
                msg_area.style().flex_shrink = Some(1.0);
                msg_area.style().flex_basis = Some(relative(0.).into());
                msg_area
            })
            // ── Input area ───────────────────────────────────────────────────
            .child(
                div()
                    .id("chat-input-area")
                    .flex()
                    .flex_col()
                    .flex_none()
                    .w_full()
                    .border_t_1()
                    .border_color(rgb(0x313244))
                    .px_2()
                    .py_1()
                    .gap(px(4.0))
                    // Enter-to-submit toggle row
                    .child(
                        div()
                            .id("submit-toggle-row")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .id("enter-toggle")
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .w(px(14.0))
                                    .h(px(14.0))
                                    .border_1()
                                    .border_color(rgb(0x45475a))
                                    .rounded(px(2.0))
                                    .cursor_pointer()
                                    .when(enter_to_submit, |el| {
                                        el.bg(rgba(0x89b4faff))
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(
                                            |this, _: &MouseDownEvent, _window, cx| {
                                                this.enter_to_submit = !this.enter_to_submit;
                                                this.input_field.update(cx, |field, _cx| {
                                                    field.submit_on_enter = this.enter_to_submit;
                                                });
                                                cx.notify();
                                            },
                                        ),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgba(0x6c7086ff))
                                    .child("Enter to submit"),
                            ),
                    )
                    // Input field + send button
                    .child(
                        div()
                            .id("input-row")
                            .flex()
                            .flex_row()
                            .items_end()
                            .gap(px(4.0))
                            .child({
                                let mut field_container = div()
                                    .flex()
                                    .flex_col()
                                    .min_w_0()
                                    .child(self.input_field.clone());
                                field_container.style().flex_grow = Some(1.0);
                                field_container.style().flex_shrink = Some(1.0);
                                field_container.style().flex_basis = Some(relative(0.).into());
                                field_container
                            })
                            .child(
                                div()
                                    .id("send-btn")
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .justify_center()
                                    .w(px(56.0))
                                    .h(px(28.0))
                                    .bg(if streaming {
                                        rgb(0x45475a)
                                    } else {
                                        rgb(0x89b4fa)
                                    })
                                    .rounded(px(3.0))
                                    .text_xs()
                                    .text_color(if streaming {
                                        rgba(0x6c7086ff)
                                    } else {
                                        rgba(0x1e1e2eff)
                                    })
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(
                                            |this, _: &MouseDownEvent, _window, cx| {
                                                this.do_send(cx);
                                            },
                                        ),
                                    )
                                    .child("Send"),
                            ),
                    ),
            )
    }
}
