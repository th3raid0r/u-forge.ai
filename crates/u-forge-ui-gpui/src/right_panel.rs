use gpui::{div, prelude::*, rgb, rgba, Context, Window};

/// Placeholder right panel (future chat / AI assistant).
pub(crate) struct RightPanel;

impl RightPanel {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl Render for RightPanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("right-panel-content")
            .flex()
            .flex_col()
            .w_full()
            .h_full()
            .min_h_0()
            .bg(rgb(0x181825))
            .border_l_1()
            .border_color(rgb(0x313244))
            .items_center()
            .justify_center()
            .text_color(rgba(0x6c7086ff))
            .text_sm()
            .child("Chat — coming soon")
    }
}
