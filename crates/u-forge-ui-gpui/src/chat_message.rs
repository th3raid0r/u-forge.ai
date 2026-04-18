//! Per-message view entity for the chat panel.
//!
//! Each chat message is its own `Entity<ChatMessageView>`. Streaming token
//! deltas update only the target entity, so the parent `ChatPanel`'s header,
//! input field, dropdowns, and sibling messages don't re-render per token.

use gpui::{
    div, prelude::*, px, rgb, rgba, Context, IntoElement, MouseButton, MouseDownEvent,
    ParentElement, Render, SharedString, Styled, Window,
};

use crate::chat_history::StoredMessage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChatMessageRole {
    User,
    Assistant,
    /// Model thinking/reasoning — rendered separately from content.
    Thinking,
    /// A tool call made by the agent during the response loop.
    ToolCall,
}

/// A single message in the chat, backed by its own GPUI entity.
pub(crate) struct ChatMessageView {
    pub(crate) role: ChatMessageRole,
    /// Main text content (or tool name for `ToolCall` entries).
    text: String,
    /// For `ToolCall` entries: the pretty-printed JSON arguments.
    tool_args: Option<String>,
    /// For `ToolCall` entries: the tool result text (filled in when available).
    tool_result: Option<String>,
    /// Stable ID correlating a `ToolCall` entry with its result event.
    tool_internal_id: Option<String>,
    /// Whether the tool call body is collapsed (tool-call entries only).
    collapsed: bool,
}

impl ChatMessageView {
    pub(crate) fn new_text(role: ChatMessageRole, text: String) -> Self {
        Self {
            role,
            text,
            tool_args: None,
            tool_result: None,
            tool_internal_id: None,
            collapsed: false,
        }
    }

    pub(crate) fn new_tool_call(internal_id: String, name: String, args: String) -> Self {
        Self {
            role: ChatMessageRole::ToolCall,
            text: name,
            tool_args: Some(args),
            tool_result: None,
            tool_internal_id: Some(internal_id),
            collapsed: true,
        }
    }

    pub(crate) fn from_stored(msg: StoredMessage) -> Self {
        let role = match msg.role.as_str() {
            "user" => ChatMessageRole::User,
            "assistant" => ChatMessageRole::Assistant,
            "thinking" => ChatMessageRole::Thinking,
            "tool_call" => ChatMessageRole::ToolCall,
            _ => ChatMessageRole::Assistant,
        };
        Self {
            role,
            text: msg.text,
            tool_args: msg.tool_args,
            tool_result: msg.tool_result,
            tool_internal_id: msg.tool_internal_id,
            collapsed: msg.collapsed,
        }
    }

    pub(crate) fn to_stored(&self) -> StoredMessage {
        StoredMessage {
            role: match self.role {
                ChatMessageRole::User => "user".to_string(),
                ChatMessageRole::Assistant => "assistant".to_string(),
                ChatMessageRole::Thinking => "thinking".to_string(),
                ChatMessageRole::ToolCall => "tool_call".to_string(),
            },
            text: self.text.clone(),
            tool_args: self.tool_args.clone(),
            tool_result: self.tool_result.clone(),
            tool_internal_id: self.tool_internal_id.clone(),
            collapsed: self.collapsed,
        }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Hot path: called on every streamed token. Only this entity notifies.
    pub(crate) fn append_text(&mut self, delta: &str, cx: &mut Context<Self>) {
        self.text.push_str(delta);
        cx.notify();
    }

    pub(crate) fn set_tool_result(&mut self, result: String, cx: &mut Context<Self>) {
        self.tool_result = Some(result);
        cx.notify();
    }

    pub(crate) fn append_error(&mut self, msg: &str, cx: &mut Context<Self>) {
        self.text.push_str(msg);
        cx.notify();
    }

    fn toggle_collapsed(&mut self, cx: &mut Context<Self>) {
        self.collapsed = !self.collapsed;
        cx.notify();
    }

    fn render_text(&self) -> gpui::Div {
        let (bg, label_color, label, text_color) = match self.role {
            ChatMessageRole::User => (
                rgb(0x313244),
                rgba(0x89b4faff),
                "You",
                rgba(0xcdd6f4ff),
            ),
            ChatMessageRole::Assistant => (
                rgb(0x1e1e2e),
                rgba(0xa6e3a1ff),
                "Assistant",
                rgba(0xcdd6f4ff),
            ),
            ChatMessageRole::Thinking => (
                rgb(0x181825),
                rgba(0xf9e2afff),
                "Thinking",
                rgba(0x6c7086ff),
            ),
            ChatMessageRole::ToolCall => unreachable!(),
        };

        let text: SharedString = self.text.clone().into();

        div()
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
            .child(div().text_xs().text_color(text_color).child(text))
    }

    fn render_tool_call(&self, cx: &mut Context<Self>) -> gpui::Div {
        let collapsed = self.collapsed;
        let has_result = self.tool_result.is_some();
        let chevron = if collapsed { "▶" } else { "▼" };
        let header_label = format!("{chevron} ⚙ {}", self.text);

        let mut el = div()
            .flex()
            .flex_col()
            .w_full()
            .px_2()
            .py_1()
            .bg(rgb(0x1e1e2e))
            .border_l_1()
            .border_color(rgba(0xcba6f7aa))
            .child(
                div()
                    .id("tc-hdr")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.0))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                            this.toggle_collapsed(cx);
                        }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgba(0xcba6f7ff))
                            .child(SharedString::from(header_label)),
                    )
                    .when(has_result, |row| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(rgba(0xa6e3a188))
                                .child("✓"),
                        )
                    }),
            );

        if !collapsed {
            let tool_args: SharedString =
                self.tool_args.clone().unwrap_or_default().into();
            el = el.child(
                div()
                    .mt_1()
                    .px_2()
                    .py_1()
                    .bg(rgb(0x181825))
                    .rounded(px(3.0))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgba(0x6c7086ff))
                            .child("Args:"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgba(0xcdd6f4cc))
                            .child(tool_args),
                    ),
            );
            if let Some(result) = self.tool_result.clone() {
                let result: SharedString = result.into();
                el = el.child(
                    div()
                        .mt_1()
                        .px_2()
                        .py_1()
                        .bg(rgb(0x181825))
                        .rounded(px(3.0))
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgba(0x6c7086ff))
                                .child("Result:"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgba(0xa6e3a1cc))
                                .child(result),
                        ),
                );
            }
        }
        el
    }
}

impl Render for ChatMessageView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.role {
            ChatMessageRole::ToolCall => self.render_tool_call(cx),
            _ => self.render_text(),
        }
    }
}
