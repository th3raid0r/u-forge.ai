use gpui::{
    Context, EventEmitter, MouseButton, MouseDownEvent, Render, Window, deferred, div, prelude::*,
    px, rgb, rgba,
};

pub(crate) struct ConfirmationAccepted;
pub(crate) struct ConfirmationAlternative;
pub(crate) struct ConfirmationCancelled;

pub(crate) struct ConfirmationModal {
    title: String,
    message: String,
    confirm_label: String,
    alternative_label: Option<String>,
    destructive: bool,
}

impl EventEmitter<ConfirmationAccepted> for ConfirmationModal {}
impl EventEmitter<ConfirmationAlternative> for ConfirmationModal {}
impl EventEmitter<ConfirmationCancelled> for ConfirmationModal {}

impl ConfirmationModal {
    pub(crate) fn new(title: String, message: String, confirm_label: String) -> Self {
        Self {
            title,
            message,
            confirm_label,
            alternative_label: None,
            destructive: true,
        }
    }

    pub(crate) fn with_alternative(mut self, label: impl Into<String>) -> Self {
        self.alternative_label = Some(label.into());
        self
    }

    pub(crate) fn non_destructive(mut self) -> Self {
        self.destructive = false;
        self
    }
}

impl Render for ConfirmationModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        deferred(
            div()
                .id("confirmation-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x0000008c))
                .child(
                    div()
                        .id("confirmation-dialog")
                        .w(px(440.0))
                        .bg(rgb(0x313244))
                        .border_1()
                        .border_color(rgb(0x45475a))
                        .rounded(px(6.0))
                        .child(
                            div()
                                .h(px(36.0))
                                .px_3()
                                .flex()
                                .items_center()
                                .bg(rgb(0x1e1e2e))
                                .border_b_1()
                                .border_color(rgb(0x45475a))
                                .text_sm()
                                .text_color(rgba(0xcdd6f4ff))
                                .child(self.title.clone()),
                        )
                        .child(
                            div()
                                .px_3()
                                .py_4()
                                .text_sm()
                                .text_color(rgba(0xa6adc8ff))
                                .child(self.message.clone()),
                        )
                        .child(
                            div()
                                .h(px(48.0))
                                .px_3()
                                .flex()
                                .flex_row()
                                .items_center()
                                .justify_end()
                                .gap(px(8.0))
                                .border_t_1()
                                .border_color(rgb(0x45475a))
                                .child(
                                    div()
                                        .id("confirmation-alternative")
                                        .h(px(28.0))
                                        .px_3()
                                        .flex()
                                        .items_center()
                                        .border_1()
                                        .border_color(rgb(0xf38ba8))
                                        .rounded(px(4.0))
                                        .text_sm()
                                        .text_color(rgba(0xf38ba8ff))
                                        .when(self.alternative_label.is_none(), |button| {
                                            button.hidden()
                                        })
                                        .when(self.alternative_label.is_some(), |button| {
                                            button
                                                .cursor_pointer()
                                                .hover(|style| style.bg(rgba(0xf38ba822)))
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(
                                                        |_this, _: &MouseDownEvent, _window, cx| {
                                                            cx.stop_propagation();
                                                            cx.emit(ConfirmationAlternative);
                                                        },
                                                    ),
                                                )
                                        })
                                        .child(self.alternative_label.clone().unwrap_or_default()),
                                )
                                .child(
                                    div()
                                        .id("confirmation-cancel")
                                        .h(px(28.0))
                                        .px_3()
                                        .flex()
                                        .items_center()
                                        .border_1()
                                        .border_color(rgb(0x45475a))
                                        .rounded(px(4.0))
                                        .text_sm()
                                        .text_color(rgba(0xcdd6f4ff))
                                        .cursor_pointer()
                                        .hover(|style| style.bg(rgb(0x45475a)))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(
                                                |_this, _: &MouseDownEvent, _window, cx| {
                                                    cx.stop_propagation();
                                                    cx.emit(ConfirmationCancelled);
                                                },
                                            ),
                                        )
                                        .child("Cancel"),
                                )
                                .child(
                                    div()
                                        .id("confirmation-accept")
                                        .h(px(28.0))
                                        .px_3()
                                        .flex()
                                        .items_center()
                                        .bg(if self.destructive {
                                            rgb(0xf38ba8)
                                        } else {
                                            rgb(0x89b4fa)
                                        })
                                        .rounded(px(4.0))
                                        .text_sm()
                                        .text_color(rgba(0x1e1e2eff))
                                        .cursor_pointer()
                                        .hover(|style| style.bg(rgb(0xeba0ac)))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(
                                                |_this, _: &MouseDownEvent, _window, cx| {
                                                    cx.stop_propagation();
                                                    cx.emit(ConfirmationAccepted);
                                                },
                                            ),
                                        )
                                        .child(self.confirm_label.clone()),
                                ),
                        ),
                ),
        )
    }
}
