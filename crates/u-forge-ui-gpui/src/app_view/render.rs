use gpui::{
    anchored, deferred, div, point, prelude::*, px, relative, rgb, rgba, Context, Corner,
    MouseButton, MouseDownEvent, Render, Window,
};

use crate::{ClearData, ImportData, SaveLayout, ToggleRightPanel, ToggleSidebar};

use super::{AppView, MENU_BAR_H, STATUS_BAR_H};

impl Render for AppView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let file_menu_open = self.file_menu_open;
        let view_menu_open = self.view_menu_open;
        let sidebar_open = self.sidebar_open;
        let right_panel_open = self.right_panel_open;

        // Read graph stats for the status bar.
        let snap = self.snapshot.read();
        let node_count = snap.nodes.len();
        let edge_count = snap.edges.len();
        drop(snap);

        div()
            .id("app-root")
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x1e1e2e))
            // Handle actions dispatched from native menu or keybindings.
            .on_action(cx.listener(|this, _: &SaveLayout, _window, cx| {
                this.do_save(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _window, cx| {
                this.sidebar_open = !this.sidebar_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleRightPanel, _window, cx| {
                this.right_panel_open = !this.right_panel_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ClearData, _window, cx| {
                this.do_clear_data(cx);
            }))
            .on_action(cx.listener(|this, _: &ImportData, _window, cx| {
                this.do_import_data(cx);
            }))
            // ── Menu bar ──────────────────────────────────────────────────────
            .child(
                div()
                    .id("menu-bar")
                    .flex()
                    .flex_none()
                    .h(px(MENU_BAR_H))
                    .w_full()
                    .bg(rgb(0x181825))
                    .border_b_1()
                    .border_color(rgb(0x313244))
                    .items_center()
                    .child(
                        // "File" menu button
                        div()
                            .id("file-btn")
                            .flex()
                            .items_center()
                            .h_full()
                            .px_3()
                            .text_color(rgba(0xcdd6f4ff))
                            .text_sm()
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                    this.file_menu_open = !this.file_menu_open;
                                    this.view_menu_open = false;
                                    cx.notify();
                                }),
                            )
                            .child("File"),
                    )
                    .child(
                        // "View" menu button
                        div()
                            .id("view-btn")
                            .flex()
                            .items_center()
                            .h_full()
                            .px_3()
                            .text_color(rgba(0xcdd6f4ff))
                            .text_sm()
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                    this.view_menu_open = !this.view_menu_open;
                                    this.file_menu_open = false;
                                    cx.notify();
                                }),
                            )
                            .child("View"),
                    ),
            )
            // ── Body: optional sidebar + main content ─────────────────────────
            .child({
                let mut body = div()
                    .id("body")
                    .flex()
                    .flex_row()
                    .min_h_0()
                    .overflow_hidden();
                body.style().flex_grow = Some(1.0);
                body.style().flex_shrink = Some(1.0);
                body.style().flex_basis = Some(relative(0.).into());

                // Left sidebar (tree panel)
                if sidebar_open {
                    body = body.child(self.tree_panel.clone());
                }

                // Main workspace: editor 30% + graph 70% (vertical split)
                let mut workspace = div()
                    .id("workspace")
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden();
                workspace.style().flex_grow = Some(1.0);
                workspace.style().flex_shrink = Some(1.0);
                workspace.style().flex_basis = Some(relative(0.).into());

                // Node detail pane — 3 parts out of 10 (30%).
                let mut editor = div()
                    .id("editor-pane")
                    .flex()
                    .flex_col()
                    .w_full()
                    .min_h_0()
                    .child(self.node_editor.clone());
                editor.style().flex_grow = Some(3.0);
                editor.style().flex_shrink = Some(1.0);
                editor.style().flex_basis = Some(relative(0.).into());

                // Graph canvas pane — 7 parts out of 10 (70%).
                let mut graph_pane = div()
                    .w_full()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.graph_canvas.clone());
                graph_pane.style().flex_grow = Some(7.0);
                graph_pane.style().flex_shrink = Some(1.0);
                graph_pane.style().flex_basis = Some(relative(0.).into());

                body = body.child(workspace.child(editor).child(graph_pane));

                // Right panel placeholder (chat)
                if right_panel_open {
                    body = body.child(
                        div()
                            .id("right-panel")
                            .flex()
                            .flex_col()
                            .flex_none()
                            .w(px(280.0))
                            .h_full()
                            .min_h_0()
                            .bg(rgb(0x181825))
                            .border_l_1()
                            .border_color(rgb(0x313244))
                            .items_center()
                            .justify_center()
                            .text_color(rgba(0x6c7086ff))
                            .text_sm()
                            .child("Chat — coming soon"),
                    );
                }

                body
            })
            // ── Status bar ────────────────────────────────────────────────────
            .child(
                div()
                    .id("status-bar")
                    .flex()
                    .flex_none()
                    .flex_row()
                    .h(px(STATUS_BAR_H))
                    .w_full()
                    .bg(rgb(0x181825))
                    .border_t_1()
                    .border_color(rgb(0x313244))
                    .items_center()
                    .text_xs()
                    // ── Left: panel toggle buttons ────────────────────────────
                    .child(
                        div()
                            .id("status-left")
                            .flex()
                            .flex_row()
                            .items_center()
                            .flex_none()
                            .gap(px(2.0))
                            .px_1()
                            .child(
                                div()
                                    .id("status-tree-btn")
                                    .flex()
                                    .items_center()
                                    .px_2()
                                    .h(px(STATUS_BAR_H - 4.0))
                                    .cursor_pointer()
                                    .text_color(if sidebar_open {
                                        rgba(0xcdd6f4ff)
                                    } else {
                                        rgba(0x6c7086ff)
                                    })
                                    .when(sidebar_open, |el| el.bg(rgba(0x45475a88)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                            this.sidebar_open = !this.sidebar_open;
                                            cx.notify();
                                        }),
                                    )
                                    .child("Tree"),
                            ),
                    )
                    // ── Center: graph stats + operation status ────────────────
                    .child({
                        let data_status = self.data_status.clone();
                        let mut center = div()
                            .id("status-center")
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_center()
                            .gap(px(12.0))
                            .text_color(rgba(0xa6adc8ff));
                        center.style().flex_grow = Some(1.0);
                        center = center.child(format!(
                            "{} nodes  ·  {} edges",
                            node_count, edge_count
                        ));
                        if let Some(msg) = data_status {
                            center = center
                                .child(div().text_color(rgba(0xa6e3a1ff)).child(msg));
                        }
                        center
                    })
                    // ── Right: chat toggle button ─────────────────────────────
                    .child(
                        div()
                            .id("status-right")
                            .flex()
                            .flex_row()
                            .items_center()
                            .flex_none()
                            .gap(px(2.0))
                            .px_1()
                            .child(
                                div()
                                    .id("status-chat-btn")
                                    .flex()
                                    .items_center()
                                    .px_2()
                                    .h(px(STATUS_BAR_H - 4.0))
                                    .cursor_pointer()
                                    .text_color(if right_panel_open {
                                        rgba(0xcdd6f4ff)
                                    } else {
                                        rgba(0x6c7086ff)
                                    })
                                    .when(right_panel_open, |el| el.bg(rgba(0x45475a88)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                            this.right_panel_open = !this.right_panel_open;
                                            cx.notify();
                                        }),
                                    )
                                    .child("Chat"),
                            ),
                    ),
            )
            // ── File dropdown overlay ─────────────────────────────────────────
            .when(file_menu_open, |root| {
                root.child(deferred(
                    anchored()
                        .position(point(px(0.0), px(MENU_BAR_H)))
                        .anchor(Corner::TopLeft)
                        .child(
                            div()
                                .id("file-dropdown")
                                .w(px(200.0))
                                .bg(rgb(0x313244))
                                .border_1()
                                .border_color(rgb(0x45475a))
                                .child(
                                    div()
                                        .id("save-item")
                                        .flex()
                                        .items_center()
                                        .h(px(28.0))
                                        .px_3()
                                        .text_color(rgba(0xcdd6f4ff))
                                        .text_sm()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(rgba(0x45475a88)))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(
                                                |this, _: &MouseDownEvent, _window, cx| {
                                                    this.do_save(cx);
                                                    this.file_menu_open = false;
                                                    cx.notify();
                                                },
                                            ),
                                        )
                                        .child("Save                Ctrl+S"),
                                )
                                // ── separator ──
                                .child(div().h(px(1.0)).w_full().bg(rgb(0x45475a)))
                                .child(
                                    div()
                                        .id("import-data-item")
                                        .flex()
                                        .items_center()
                                        .h(px(28.0))
                                        .px_3()
                                        .text_color(rgba(0xcdd6f4ff))
                                        .text_sm()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(rgba(0x45475a88)))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(
                                                |this, _: &MouseDownEvent, _window, cx| {
                                                    this.file_menu_open = false;
                                                    this.do_import_data(cx);
                                                },
                                            ),
                                        )
                                        .child("Import Data"),
                                )
                                .child(
                                    div()
                                        .id("clear-data-item")
                                        .flex()
                                        .items_center()
                                        .h(px(28.0))
                                        .px_3()
                                        .text_color(rgba(0xf38ba8ff))
                                        .text_sm()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(rgba(0x45475a88)))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(
                                                |this, _: &MouseDownEvent, _window, cx| {
                                                    this.file_menu_open = false;
                                                    this.do_clear_data(cx);
                                                },
                                            ),
                                        )
                                        .child("Clear Data"),
                                ),
                        ),
                ))
            })
            // ── View dropdown overlay ─────────────────────────────────────────
            .when(view_menu_open, |root| {
                // Position horizontally after the "File" button (~30px wide + padding).
                root.child(deferred(
                    anchored()
                        .position(point(px(32.0), px(MENU_BAR_H)))
                        .anchor(Corner::TopLeft)
                        .child(
                            div()
                                .id("view-dropdown")
                                .w(px(200.0))
                                .bg(rgb(0x313244))
                                .border_1()
                                .border_color(rgb(0x45475a))
                                // Left Panel toggle
                                .child(
                                    div()
                                        .id("toggle-left-item")
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .h(px(28.0))
                                        .px_3()
                                        .text_color(rgba(0xcdd6f4ff))
                                        .text_sm()
                                        .cursor_pointer()
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(
                                                |this, _: &MouseDownEvent, _window, cx| {
                                                    this.sidebar_open = !this.sidebar_open;
                                                    this.view_menu_open = false;
                                                    cx.notify();
                                                },
                                            ),
                                        )
                                        .child(if sidebar_open {
                                            "  Left Panel       Ctrl+B"
                                        } else {
                                            "    Left Panel       Ctrl+B"
                                        }),
                                )
                                // Right Panel toggle
                                .child(
                                    div()
                                        .id("toggle-right-item")
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .h(px(28.0))
                                        .px_3()
                                        .text_color(rgba(0xcdd6f4ff))
                                        .text_sm()
                                        .cursor_pointer()
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(
                                                |this, _: &MouseDownEvent, _window, cx| {
                                                    this.right_panel_open =
                                                        !this.right_panel_open;
                                                    this.view_menu_open = false;
                                                    cx.notify();
                                                },
                                            ),
                                        )
                                        .child(if right_panel_open {
                                            "  Right Panel      Ctrl+J"
                                        } else {
                                            "    Right Panel      Ctrl+J"
                                        }),
                                ),
                        ),
                ))
            })
    }
}
