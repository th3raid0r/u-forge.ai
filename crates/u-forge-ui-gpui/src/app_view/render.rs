use gpui::{
    anchored, deferred, div, point, prelude::*, px, relative, rgb, rgba, App, ClickEvent, Context,
    Corner, MouseButton, MouseDownEvent, Render, StyleRefinement, Window,
};

use crate::{ClearData, ImportData, SaveLayout, ToggleRightPanel, ToggleSidebar};

use super::{
    AppView, SidebarTab, DEFAULT_EDITOR_RATIO, DEFAULT_RIGHT_PANEL_W, DEFAULT_SIDEBAR_W,
    MAX_PANE_RATIO, MENU_BAR_H, MIN_PANEL_W, MIN_PANE_RATIO, MIN_WORKSPACE_W,
    RESIZE_HANDLE_SIZE, ResizeEditorCanvas, ResizeRightPanel, ResizeSidebar, STATUS_BAR_H,
};

impl Render for AppView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let file_menu_open = self.file_menu_open;
        let view_menu_open = self.view_menu_open;
        let sidebar_open = self.sidebar_open;
        let sidebar_tab = self.sidebar_tab;
        let right_panel_open = self.right_panel_open;
        let sidebar_width = self.sidebar_width;
        let editor_ratio = self.editor_ratio;
        let right_panel_width = self.right_panel_width;
        let embedding_status = self.embedding_status.clone();

        // Read graph stats for the status bar.
        let snap = self.snapshot.read();
        let node_count = snap.nodes.len();
        let edge_count = snap.edges.len();
        drop(snap);

        // Weak handle used by drag-move closures to update panel sizes.
        let handle = cx.weak_entity();

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
                // Clone handles for the drag-move closures.
                let handle_sidebar = handle.clone();
                let handle_right = handle.clone();

                let mut body = div()
                    .id("body")
                    .flex()
                    .flex_row()
                    .min_h_0()
                    .overflow_hidden()
                    // Dismiss open menu dropdowns on any click in the body area.
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                            if this.file_menu_open || this.view_menu_open {
                                this.file_menu_open = false;
                                this.view_menu_open = false;
                                cx.notify();
                            }
                        }),
                    )
                    // Handle sidebar resize drags
                    .on_drag_move::<ResizeSidebar>(move |event, _window, cx: &mut App| {
                        let mouse_x = f32::from(event.event.position.x);
                        let body_left = f32::from(event.bounds.origin.x);
                        let body_w = f32::from(event.bounds.size.width);
                        let new_width = mouse_x - body_left;
                        handle_sidebar
                            .update(cx, |view, cx| {
                                let right_w = if view.right_panel_open {
                                    view.right_panel_width + RESIZE_HANDLE_SIZE
                                } else {
                                    0.0
                                };
                                let max_w = (body_w - MIN_WORKSPACE_W - right_w).max(MIN_PANEL_W);
                                view.sidebar_width = new_width.clamp(MIN_PANEL_W, max_w);
                                cx.notify();
                            })
                            .ok();
                    })
                    // Handle right panel resize drags
                    .on_drag_move::<ResizeRightPanel>(move |event, _window, cx: &mut App| {
                        let mouse_x = f32::from(event.event.position.x);
                        let body_right = f32::from(event.bounds.origin.x)
                            + f32::from(event.bounds.size.width);
                        let body_w = f32::from(event.bounds.size.width);
                        let new_width = body_right - mouse_x;
                        handle_right
                            .update(cx, |view, cx| {
                                let sidebar_w = if view.sidebar_open {
                                    view.sidebar_width + RESIZE_HANDLE_SIZE
                                } else {
                                    0.0
                                };
                                let max_w =
                                    (body_w - MIN_WORKSPACE_W - sidebar_w).max(MIN_PANEL_W);
                                view.right_panel_width = new_width.clamp(MIN_PANEL_W, max_w);
                                cx.notify();
                            })
                            .ok();
                    });
                body.style().flex_grow = Some(1.0);
                body.style().flex_shrink = Some(1.0);
                body.style().flex_basis = Some(relative(0.).into());

                // Left sidebar (node panel) + resize handle
                if sidebar_open {
                    let handle_reset = handle.clone();
                    body = body
                        .child(
                            div()
                                .id("sidebar-container")
                                .flex()
                                .flex_col()
                                .flex_none()
                                .w(px(sidebar_width))
                                .h_full()
                                .min_h_0()
                                .overflow_hidden()
                                .child(match sidebar_tab {
                                    SidebarTab::Nodes => {
                                        self.node_panel.clone().into_any_element()
                                    }
                                    SidebarTab::Search => {
                                        self.search_panel.clone().into_any_element()
                                    }
                                }),
                        )
                        .child(
                            // Sidebar resize handle — 6px wide, full height.
                            div()
                                .id("sidebar-resize-handle")
                                .flex_none()
                                .w(px(RESIZE_HANDLE_SIZE))
                                .h_full()
                                .cursor_col_resize()
                                .hover(|s: StyleRefinement| s.bg(rgba(0x45475a66)))
                                .on_drag(ResizeSidebar, |_, _, _, cx: &mut App| {
                                    cx.new(|_| ResizeSidebar)
                                })
                                .on_click(move |event: &ClickEvent, _window, cx: &mut App| {
                                    if event.click_count() == 2 {
                                        handle_reset
                                            .update(cx, |view, cx| {
                                                view.sidebar_width = DEFAULT_SIDEBAR_W;
                                                cx.notify();
                                            })
                                            .ok();
                                    }
                                }),
                        );
                }

                // Main workspace: editor + canvas (vertical split)
                let handle_editor = handle.clone();
                let mut workspace = div()
                    .id("workspace")
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    // Handle editor/canvas resize drags
                    .on_drag_move::<ResizeEditorCanvas>(move |event, _window, cx: &mut App| {
                        let mouse_y = f32::from(event.event.position.y);
                        let ws_top = f32::from(event.bounds.origin.y);
                        let ws_h = f32::from(event.bounds.size.height);
                        if ws_h > 0.0 {
                            let ratio =
                                ((mouse_y - ws_top) / ws_h).clamp(MIN_PANE_RATIO, MAX_PANE_RATIO);
                            handle_editor
                                .update(cx, |view, cx| {
                                    view.editor_ratio = ratio;
                                    cx.notify();
                                })
                                .ok();
                        }
                    });
                workspace.style().flex_grow = Some(1.0);
                workspace.style().flex_shrink = Some(1.0);
                workspace.style().flex_basis = Some(relative(0.).into());

                // Node detail pane — proportional to editor_ratio.
                let mut editor = div()
                    .id("editor-pane")
                    .flex()
                    .flex_col()
                    .w_full()
                    .min_h_0()
                    .child(self.node_editor.clone());
                editor.style().flex_grow = Some(editor_ratio);
                editor.style().flex_shrink = Some(1.0);
                editor.style().flex_basis = Some(relative(0.).into());

                // Editor/canvas resize handle — full width, 6px tall.
                let handle_editor_reset = handle.clone();
                let editor_canvas_handle = div()
                    .id("editor-canvas-resize-handle")
                    .flex_none()
                    .w_full()
                    .h(px(RESIZE_HANDLE_SIZE))
                    .cursor_row_resize()
                    .hover(|s: StyleRefinement| s.bg(rgba(0x45475a66)))
                    .on_drag(ResizeEditorCanvas, |_, _, _, cx: &mut App| {
                        cx.new(|_| ResizeEditorCanvas)
                    })
                    .on_click(move |event: &ClickEvent, _window, cx: &mut App| {
                        if event.click_count() == 2 {
                            handle_editor_reset
                                .update(cx, |view, cx| {
                                    view.editor_ratio = DEFAULT_EDITOR_RATIO;
                                    cx.notify();
                                })
                                .ok();
                        }
                    });

                // Graph canvas pane — remainder of workspace height.
                let canvas_ratio = 1.0 - editor_ratio;
                let mut graph_pane = div()
                    .w_full()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.graph_canvas.clone());
                graph_pane.style().flex_grow = Some(canvas_ratio);
                graph_pane.style().flex_shrink = Some(1.0);
                graph_pane.style().flex_basis = Some(relative(0.).into());

                body = body.child(
                    workspace
                        .child(editor)
                        .child(editor_canvas_handle)
                        .child(graph_pane),
                );

                // Right panel + resize handle on its left edge.
                if right_panel_open {
                    let handle_right_reset = handle.clone();
                    body = body
                        .child(
                            // Right resize handle
                            div()
                                .id("right-panel-resize-handle")
                                .flex_none()
                                .w(px(RESIZE_HANDLE_SIZE))
                                .h_full()
                                .cursor_col_resize()
                                .hover(|s: StyleRefinement| s.bg(rgba(0x45475a66)))
                                .on_drag(ResizeRightPanel, |_, _, _, cx: &mut App| {
                                    cx.new(|_| ResizeRightPanel)
                                })
                                .on_click(
                                    move |event: &ClickEvent, _window, cx: &mut App| {
                                        if event.click_count() == 2 {
                                            handle_right_reset
                                                .update(cx, |view, cx| {
                                                    view.right_panel_width = DEFAULT_RIGHT_PANEL_W;
                                                    cx.notify();
                                                })
                                                .ok();
                                        }
                                    },
                                ),
                        )
                        .child(
                            div()
                                .id("right-panel-container")
                                .flex()
                                .flex_col()
                                .flex_none()
                                .w(px(right_panel_width))
                                .h_full()
                                .min_h_0()
                                .overflow_hidden()
                                .child(self.chat_panel.clone()),
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
                            // Tree button
                            .child(
                                div()
                                    .id("status-tree-btn")
                                    .flex()
                                    .items_center()
                                    .px_2()
                                    .h(px(STATUS_BAR_H - 4.0))
                                    .cursor_pointer()
                                    .text_color(
                                        if sidebar_open && sidebar_tab == SidebarTab::Nodes {
                                            rgba(0xcdd6f4ff)
                                        } else {
                                            rgba(0x6c7086ff)
                                        },
                                    )
                                    .when(
                                        sidebar_open && sidebar_tab == SidebarTab::Nodes,
                                        |el| el.bg(rgba(0x45475a88)),
                                    )
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                            if this.sidebar_open && this.sidebar_tab == SidebarTab::Nodes {
                                                this.sidebar_open = false;
                                            } else {
                                                this.sidebar_open = true;
                                                this.sidebar_tab = SidebarTab::Nodes;
                                            }
                                            cx.notify();
                                        }),
                                    )
                                    .child("Nodes"),
                            )
                            // Search button
                            .child(
                                div()
                                    .id("status-search-btn")
                                    .flex()
                                    .items_center()
                                    .px_2()
                                    .h(px(STATUS_BAR_H - 4.0))
                                    .cursor_pointer()
                                    .text_color(
                                        if sidebar_open && sidebar_tab == SidebarTab::Search {
                                            rgba(0xcdd6f4ff)
                                        } else {
                                            rgba(0x6c7086ff)
                                        },
                                    )
                                    .when(
                                        sidebar_open && sidebar_tab == SidebarTab::Search,
                                        |el| el.bg(rgba(0x45475a88)),
                                    )
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                            if this.sidebar_open && this.sidebar_tab == SidebarTab::Search {
                                                this.sidebar_open = false;
                                            } else {
                                                this.sidebar_open = true;
                                                this.sidebar_tab = SidebarTab::Search;
                                            }
                                            cx.notify();
                                        }),
                                    )
                                    .child("Search"),
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
                            center = center.child(div().text_color(rgba(0xa6e3a1ff)).child(msg));
                        }
                        if let Some(msg) = embedding_status {
                            center = center.child(div().text_color(rgba(0xf9e2afff)).child(msg));
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
