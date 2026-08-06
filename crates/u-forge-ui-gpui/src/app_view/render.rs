use std::time::Instant;

use gpui::{
    AnyView, App, ClickEvent, Context, Corner, Focusable, MouseButton, MouseDownEvent, Render,
    StyleRefinement, Window, anchored, canvas, deferred, div, point, prelude::*, px, relative, rgb,
    rgba,
};

use crate::{
    ClearData, ClearSchema, DetailsCloseTab, DetailsNextTab, DetailsPreviousTab, ExportData,
    FitGraph, FocusNextRegion, FocusPreviousRegion, ImportData, ImportSchema, OpenSettings,
    SaveActiveItem, SaveAllItems, ToggleDetailsPanel, ToggleFocusedPanelZoom, TogglePerfOverlay,
    ToggleRightPanel, ToggleSidebar,
};

use super::{
    AppView, MENU_BAR_H, ResizeEditorCanvas, ResizeRightPanel, ResizeSidebar, STATUS_BAR_H,
};
use crate::dock_state::RESIZE_HANDLE_SIZE;
use crate::panel_contracts::{DockPosition, PanelId, WorkspaceItemId, WorldCanvasViewId};
use crate::startup::StartupMilestone;
use crate::ui::components::{Button, ButtonStyle, Dialog, Tab as UiTab};
use crate::ui::theme::UiTheme;

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(px(self.ui_font_size));
        let theme = *UiTheme::get(cx);

        // Capture frame start time. The canvas element appended at the end of the
        // tree records elapsed time in its paint closure — after GPUI's full layout
        // pass — giving an honest measure of frame cost rather than render() call
        // frequency.
        let frame_start = Instant::now();
        let timing_entity = cx.entity().clone();

        let file_menu_open = self.file_menu_open;
        let view_menu_open = self.view_menu_open;
        let setup_open = self.setup_open;
        let settings_open = self.settings_open;
        let show_advanced_controls = self.show_advanced_controls;
        let sidebar_open = self.dock_state.is_open(DockPosition::Left);
        let sidebar_tab = self.dock_state.active_panel(DockPosition::Left);
        let right_panel_open = self.dock_state.is_open(DockPosition::Right);
        let details_open = self.dock_state.is_open(DockPosition::Bottom);
        let zoomed_panel = self.dock_state.zoomed_panel();
        let left_zoomed = matches!(zoomed_panel, Some(PanelId::World | PanelId::Search));
        let assistant_zoomed = zoomed_panel == Some(PanelId::Assistant);
        let details_zoomed = zoomed_panel == Some(PanelId::Details);
        let any_zoomed = zoomed_panel.is_some();
        let sidebar_width = self.dock_state.size(DockPosition::Left);
        let details_height = self.dock_state.size(DockPosition::Bottom);
        let right_panel_width = self.dock_state.size(DockPosition::Right);
        let embedding_status = self.state.embedding_status.clone();
        let perf_enabled = self.perf_enabled;
        let startup = self.startup.clone();
        let app_first_paint_pending = !startup.contains(StartupMilestone::AppFirstPaint);

        // Build perf overlay text when enabled.
        // `last_frame_cost_us` is populated by the timing canvas at the bottom of
        // the tree — it captures render-tree build + full GPUI layout pass + paint
        // start, which is the actual frame cost the user perceives.
        let perf_text: Option<String> = if perf_enabled {
            let frame_ms = self.last_frame_cost_us as f32 / 1_000.0;
            let avg_ms = self
                .frame_times_us
                .average()
                .map(|us| us as f32 / 1_000.0)
                .unwrap_or(frame_ms);
            let chat_us = self.chat_panel.read(cx).last_render_us;
            let chat_ms = chat_us as f32 / 1_000.0;
            Some(format!(
                "frame:{frame_ms:.1}ms  avg:{avg_ms:.1}ms  chat:{chat_ms:.2}ms"
            ))
        } else {
            None
        };

        // Read graph stats for the status bar and menu grey-out logic.
        let snap = self.state.snapshot.read();
        let node_count = snap.nodes.len();
        let edge_count = snap.edges.len();
        let has_data = node_count > 0;
        let has_schema = self.state.schema_loaded;
        drop(snap);

        // Weak handle used by drag-move closures to update panel sizes.
        let handle = cx.weak_entity();

        div()
            .id("app-root")
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.colors.app_surface)
            // Handle actions dispatched from native menu or keybindings.
            .on_action(cx.listener(|this, _: &SaveActiveItem, _window, cx| {
                this.do_save_active(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SaveAllItems, _window, cx| {
                this.do_save_all(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, _window, cx| {
                this.open_settings(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleFocusedPanelZoom, window, cx| {
                this.toggle_focused_panel_zoom(window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusNextRegion, window, cx| {
                this.cycle_workspace_focus(false, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusPreviousRegion, window, cx| {
                this.cycle_workspace_focus(true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &DetailsNextTab, _window, cx| {
                this.node_editor
                    .update(cx, |editor, cx| editor.activate_relative_tab(false, cx));
            }))
            .on_action(cx.listener(|this, _: &DetailsPreviousTab, _window, cx| {
                this.node_editor
                    .update(cx, |editor, cx| editor.activate_relative_tab(true, cx));
            }))
            .on_action(cx.listener(|this, _: &DetailsCloseTab, _window, cx| {
                this.request_close_active_editor_tab(cx);
            }))
            .on_action(cx.listener(|this, _: &FitGraph, _window, cx| {
                this.graph_canvas
                    .update(cx, |canvas, cx| canvas.fit_graph(cx));
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, window, cx| {
                this.dock_state.toggle_panel(PanelId::World);
                if this.dock_state.is_panel_active(PanelId::World) {
                    this.node_panel.read(cx).focus_handle(cx).focus(window);
                }
                this.schedule_workspace_persist(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleRightPanel, window, cx| {
                if this.dock_state.is_panel_active(PanelId::Assistant) {
                    this.chat_panel
                        .update(cx, |panel, _cx| panel.last_render_us = 0);
                }
                this.dock_state.toggle_panel(PanelId::Assistant);
                this.sync_dock_presentational_state(cx);
                if this.dock_state.is_panel_active(PanelId::Assistant) {
                    this.chat_panel.read(cx).focus_handle(cx).focus(window);
                }
                this.schedule_workspace_persist(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleDetailsPanel, window, cx| {
                this.dock_state.toggle_panel(PanelId::Details);
                if this.dock_state.is_panel_active(PanelId::Details) {
                    this.node_editor.read(cx).focus_handle(cx).focus(window);
                }
                this.schedule_workspace_persist(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ClearData, _window, cx| {
                this.request_clear_data(cx);
            }))
            .on_action(cx.listener(|this, _: &ClearSchema, _window, cx| {
                this.request_clear_schema(cx);
            }))
            .on_action(cx.listener(|this, _: &ImportData, window, cx| {
                this.do_import_data_picker(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ImportSchema, window, cx| {
                this.do_import_schema_picker(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ExportData, window, cx| {
                this.do_export_data_picker(window, cx);
            }))
            .on_action(cx.listener(|this, _: &TogglePerfOverlay, _window, cx| {
                this.perf_enabled = !this.perf_enabled;
                if !this.perf_enabled {
                    this.frame_times_us.clear();
                }
                cx.notify();
            }))
            // ── Menu bar ──────────────────────────────────────────────────────
            .child(
                div()
                    .id("menu-bar")
                    .flex()
                    .flex_none()
                    .h(px(MENU_BAR_H))
                    .w_full()
                    .bg(theme.colors.panel_surface)
                    .border_b_1()
                    .border_color(theme.colors.border_subtle)
                    .items_center()
                    .child(
                        // "File" menu button
                        div()
                            .id("file-btn")
                            .flex()
                            .items_center()
                            .h_full()
                            .px_3()
                            .text_color(theme.colors.text)
                            .text_xs()
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
                            .text_color(theme.colors.text)
                            .text_xs()
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
                                view.dock_state.resize_horizontal(
                                    DockPosition::Left,
                                    new_width,
                                    body_w,
                                );
                                view.schedule_workspace_persist(cx);
                                cx.notify();
                            })
                            .ok();
                    })
                    // Handle right panel resize drags
                    .on_drag_move::<ResizeRightPanel>(move |event, _window, cx: &mut App| {
                        let mouse_x = f32::from(event.event.position.x);
                        let body_right =
                            f32::from(event.bounds.origin.x) + f32::from(event.bounds.size.width);
                        let body_w = f32::from(event.bounds.size.width);
                        let new_width = body_right - mouse_x;
                        handle_right
                            .update(cx, |view, cx| {
                                view.dock_state.resize_horizontal(
                                    DockPosition::Right,
                                    new_width,
                                    body_w,
                                );
                                view.schedule_workspace_persist(cx);
                                cx.notify();
                            })
                            .ok();
                    });
                body.style().flex_grow = Some(1.0);
                body.style().flex_shrink = Some(1.0);
                body.style().flex_basis = Some(relative(0.).into());

                // World and Search remain mounted even while inactive or while
                // the left dock is closed. Each cached inner view keeps stable
                // bounds; zero-sized outer layers remove inactive hitboxes.
                let left_open_width = if sidebar_open && !any_zoomed {
                    sidebar_width
                } else {
                    0.0
                };
                let left_handle_width = if sidebar_open && !any_zoomed {
                    RESIZE_HANDLE_SIZE
                } else {
                    0.0
                };
                body = body.child(
                    div()
                        .id("left-dock-container")
                        .relative()
                        .flex_none()
                        .when(left_zoomed, |dock| dock.w_full())
                        .when(!left_zoomed, |dock| dock.w(px(left_open_width)))
                        .h_full()
                        .overflow_hidden()
                        .child({
                            let active = sidebar_tab == PanelId::World;
                            div()
                                .id("world-panel-mount")
                                .absolute()
                                .top_0()
                                .left_0()
                                .when(left_zoomed && active, |mount| mount.w_full())
                                .when(!left_zoomed || !active, |mount| {
                                    mount.w(px(if active { sidebar_width } else { 0.0 }))
                                })
                                .h_full()
                                .overflow_hidden()
                                .child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .left_0()
                                        .when(left_zoomed, |inner| inner.w_full())
                                        .when(!left_zoomed, |inner| inner.w(px(sidebar_width)))
                                        .h_full()
                                        .child(
                                            AnyView::from(self.node_panel.clone())
                                                .cached(StyleRefinement::default().size_full()),
                                        ),
                                )
                        })
                        .child({
                            let active = sidebar_tab == PanelId::Search;
                            div()
                                .id("search-panel-mount")
                                .absolute()
                                .top_0()
                                .left_0()
                                .when(left_zoomed && active, |mount| mount.w_full())
                                .when(!left_zoomed || !active, |mount| {
                                    mount.w(px(if active { sidebar_width } else { 0.0 }))
                                })
                                .h_full()
                                .overflow_hidden()
                                .child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .left_0()
                                        .when(left_zoomed, |inner| inner.w_full())
                                        .when(!left_zoomed, |inner| inner.w(px(sidebar_width)))
                                        .h_full()
                                        .child(
                                            AnyView::from(self.search_panel.clone())
                                                .cached(StyleRefinement::default().size_full()),
                                        ),
                                )
                        }),
                );

                let handle_reset = handle.clone();
                let mut left_resize_handle = div()
                    .id("sidebar-resize-handle")
                    .flex_none()
                    .w(px(left_handle_width))
                    .h_full()
                    .overflow_hidden();
                if sidebar_open {
                    left_resize_handle = left_resize_handle
                        .cursor_col_resize()
                        .hover(|s: StyleRefinement| s.bg(rgba(0x45475a66)))
                        .on_drag(ResizeSidebar, |_, _, _, cx: &mut App| {
                            cx.new(|_| ResizeSidebar)
                        })
                        .on_click(move |event: &ClickEvent, _window, cx: &mut App| {
                            if event.click_count() == 2 {
                                handle_reset
                                    .update(cx, |view, cx| {
                                        view.dock_state.reset_size(DockPosition::Left);
                                        view.schedule_workspace_persist(cx);
                                        cx.notify();
                                    })
                                    .ok();
                            }
                        });
                }
                body = body.child(left_resize_handle);

                // Main workspace: World Canvas with an optional bottom Details dock.
                let handle_editor = handle.clone();
                let mut workspace = div()
                    .id("workspace")
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    // Handle Details/World Canvas resize drags.
                    .on_drag_move::<ResizeEditorCanvas>(move |event, _window, cx: &mut App| {
                        let mouse_y = f32::from(event.event.position.y);
                        let ws_bottom =
                            f32::from(event.bounds.origin.y) + f32::from(event.bounds.size.height);
                        let ws_h = f32::from(event.bounds.size.height);
                        if ws_h > 0.0 {
                            let requested = ws_bottom - mouse_y;
                            handle_editor
                                .update(cx, |view, cx| {
                                    view.dock_state.resize_bottom(requested, ws_h);
                                    view.schedule_workspace_persist(cx);
                                    cx.notify();
                                })
                                .ok();
                        }
                    });
                workspace.style().flex_grow = Some(1.0);
                workspace.style().flex_shrink = Some(1.0);
                workspace.style().flex_basis = Some(relative(0.).into());
                if assistant_zoomed || left_zoomed {
                    workspace = workspace.w(px(0.0));
                    workspace.style().flex_grow = Some(0.0);
                    workspace.style().flex_shrink = Some(0.0);
                }

                // Details/World Canvas resize handle — zero-height when closed.
                let handle_editor_reset = handle.clone();
                let mut editor_canvas_handle = div()
                    .id("editor-canvas-resize-handle")
                    .flex_none()
                    .w_full()
                    .h(px(if details_open && !details_zoomed {
                        RESIZE_HANDLE_SIZE
                    } else {
                        0.0
                    }))
                    .overflow_hidden();
                if details_open && !details_zoomed {
                    editor_canvas_handle = editor_canvas_handle
                        .cursor_row_resize()
                        .hover(|s: StyleRefinement| s.bg(rgba(0x45475a66)))
                        .on_drag(ResizeEditorCanvas, |_, _, _, cx: &mut App| {
                            cx.new(|_| ResizeEditorCanvas)
                        })
                        .on_click(move |event: &ClickEvent, _window, cx: &mut App| {
                            if event.click_count() == 2 {
                                handle_editor_reset
                                    .update(cx, |view, cx| {
                                        view.dock_state.reset_size(DockPosition::Bottom);
                                        view.schedule_workspace_persist(cx);
                                        cx.notify();
                                    })
                                    .ok();
                            }
                        });
                }

                // World Canvas — Connections is today's center view. The tab
                // boundary is deliberate: Timeline and Map can become sibling
                // center views later without renaming the workspace again.
                let mut graph_pane = div()
                    .id("world-canvas")
                    .flex()
                    .flex_col()
                    .w_full()
                    .min_h_0()
                    .overflow_hidden()
                    .child(
                        div()
                            .id("world-canvas-tab-bar")
                            .flex()
                            .flex_none()
                            .items_center()
                            .h(px(theme.metrics.panel_header_height))
                            .px_3()
                            .gap(px(theme.metrics.space_4))
                            .bg(theme.colors.panel_surface)
                            .border_b_1()
                            .border_color(theme.colors.border_subtle)
                            .text_sm()
                            .text_color(theme.colors.text)
                            .child(WorkspaceItemId::WorldCanvas.title())
                            .child(
                                UiTab::new(
                                    "connections-tab",
                                    WorldCanvasViewId::Connections.title(),
                                    true,
                                )
                                .tooltip("Show relationships between world items"),
                            ),
                    )
                    .child({
                        let mut canvas = div()
                            .id("connections-view")
                            .min_h_0()
                            .overflow_hidden()
                            .child(self.graph_canvas.clone());
                        canvas.style().flex_grow = Some(1.0);
                        canvas.style().flex_shrink = Some(1.0);
                        canvas.style().flex_basis = Some(relative(0.).into());
                        canvas
                    });
                graph_pane.style().flex_grow = Some(1.0);
                graph_pane.style().flex_shrink = Some(1.0);
                graph_pane.style().flex_basis = Some(relative(0.).into());
                if details_zoomed {
                    graph_pane = graph_pane.h(px(0.0));
                    graph_pane.style().flex_grow = Some(0.0);
                    graph_pane.style().flex_shrink = Some(0.0);
                }

                // Details remains mounted with stable inner bounds. Closing the
                // dock clips the cached editor to a zero-height outer wrapper.
                let details_open_height = if details_open && !details_zoomed {
                    details_height
                } else {
                    0.0
                };
                let mut editor = div()
                    .id("details-dock-container")
                    .relative()
                    .flex_none()
                    .w_full()
                    .overflow_hidden();
                if details_zoomed {
                    editor = editor.h_full();
                    editor.style().flex_grow = Some(1.0);
                    editor.style().flex_shrink = Some(1.0);
                } else {
                    editor = editor.h(px(details_open_height));
                }
                editor = editor.child({
                    let inner = div()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .w_full()
                        .overflow_hidden();
                    let inner = if details_zoomed {
                        inner.h_full()
                    } else {
                        inner.h(px(details_height))
                    };
                    inner.child(
                        AnyView::from(self.node_editor.clone())
                            .cached(StyleRefinement::default().size_full()),
                    )
                });

                body = body.child(
                    workspace
                        .child(graph_pane)
                        .child(editor_canvas_handle)
                        .child(editor),
                );

                // Right panel + resize handle.
                //
                // The chat panel is always mounted in the tree so its
                // element_state survives toggles. An outer wrapper sets the
                // visible width (0 when closed, right_panel_width when open)
                // with overflow_hidden; an inner absolute-positioned div
                // always lays out at right_panel_width so the cached chat
                // panel sees stable bounds across toggles.
                //
                // The AnyView::cached wrapper lets GPUI skip the chat panel's
                // layout + paint on frames where no ChatPanel / ChatMessageView
                // entity was notified (e.g. sidebar drags, status bar ticks).
                let right_open_w = if right_panel_open && !any_zoomed {
                    right_panel_width
                } else {
                    0.0
                };
                let handle_w = if right_panel_open && !any_zoomed {
                    RESIZE_HANDLE_SIZE
                } else {
                    0.0
                };

                // Resize handle — present but zero-width when closed so its
                // mouse hitbox disappears.
                let handle_right_reset = handle.clone();
                let mut resize_handle = div()
                    .id("right-panel-resize-handle")
                    .flex_none()
                    .w(px(handle_w))
                    .h_full()
                    .overflow_hidden();
                if right_panel_open {
                    resize_handle = resize_handle
                        .cursor_col_resize()
                        .hover(|s: StyleRefinement| s.bg(rgba(0x45475a66)))
                        .on_drag(ResizeRightPanel, |_, _, _, cx: &mut App| {
                            cx.new(|_| ResizeRightPanel)
                        })
                        .on_click(move |event: &ClickEvent, _window, cx: &mut App| {
                            if event.click_count() == 2 {
                                handle_right_reset
                                    .update(cx, |view, cx| {
                                        view.dock_state.reset_size(DockPosition::Right);
                                        view.schedule_workspace_persist(cx);
                                        cx.notify();
                                    })
                                    .ok();
                            }
                        });
                }
                body = body.child(resize_handle);

                body = body.child({
                    let outer = div()
                        .id("right-panel-container")
                        .relative()
                        .flex_none()
                        .h_full()
                        .overflow_hidden();
                    let outer = if assistant_zoomed {
                        outer.w_full()
                    } else {
                        outer.w(px(right_open_w))
                    };
                    outer.child({
                        let inner = div()
                            .id("right-panel-inner")
                            .absolute()
                            .top_0()
                            .left_0()
                            .h_full()
                            .overflow_hidden();
                        let inner = if assistant_zoomed {
                            inner.w_full()
                        } else {
                            inner.w(px(right_panel_width))
                        };
                        inner.child(
                            AnyView::from(self.chat_panel.clone())
                                .cached(StyleRefinement::default().size_full()),
                        )
                    })
                });

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
                    .bg(theme.colors.panel_surface)
                    .border_t_1()
                    .border_color(theme.colors.border_subtle)
                    .items_center()
                    .text_sm()
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
                                    .text_color(if sidebar_open && sidebar_tab == PanelId::World {
                                        rgba(0xcdd6f4ff)
                                    } else {
                                        rgba(0x6c7086ff)
                                    })
                                    .when(sidebar_open && sidebar_tab == PanelId::World, |el| {
                                        el.bg(rgba(0x45475a88))
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                            this.dock_state.toggle_panel(PanelId::World);
                                            if this.dock_state.is_panel_active(PanelId::World) {
                                                this.node_panel
                                                    .read(cx)
                                                    .focus_handle(cx)
                                                    .focus(window);
                                            }
                                            this.schedule_workspace_persist(cx);
                                            cx.notify();
                                        }),
                                    )
                                    .child(PanelId::World.title()),
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
                                    .text_color(if sidebar_open && sidebar_tab == PanelId::Search {
                                        rgba(0xcdd6f4ff)
                                    } else {
                                        rgba(0x6c7086ff)
                                    })
                                    .when(sidebar_open && sidebar_tab == PanelId::Search, |el| {
                                        el.bg(rgba(0x45475a88))
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                            this.dock_state.toggle_panel(PanelId::Search);
                                            if this.dock_state.is_panel_active(PanelId::Search) {
                                                this.search_panel
                                                    .read(cx)
                                                    .focus_handle(cx)
                                                    .focus(window);
                                            }
                                            this.schedule_workspace_persist(cx);
                                            cx.notify();
                                        }),
                                    )
                                    .child(PanelId::Search.title()),
                            ),
                    )
                    // ── Center: graph stats + operation status ────────────────
                    .child({
                        let data_status = self.state.data_status.clone();
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
                            "{} world items  ·  {} relationships",
                            node_count, edge_count
                        ));
                        if let Some(msg) = data_status {
                            center = center.child(div().text_color(rgba(0xa6e3a1ff)).child(msg));
                        }
                        if let Some(msg) = embedding_status {
                            center = center.child(div().text_color(rgba(0xf9e2afff)).child(msg));
                        }
                        if let Some(perf) = perf_text {
                            center = center.child(div().text_color(rgba(0xa6e3a1ff)).child(perf));
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
                                    .id("status-details-btn")
                                    .flex()
                                    .items_center()
                                    .px_2()
                                    .h(px(STATUS_BAR_H - 4.0))
                                    .cursor_pointer()
                                    .text_color(if details_open {
                                        rgba(0xcdd6f4ff)
                                    } else {
                                        rgba(0x6c7086ff)
                                    })
                                    .when(details_open, |el| el.bg(rgba(0x45475a88)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                            this.dock_state.toggle_panel(PanelId::Details);
                                            if this.dock_state.is_panel_active(PanelId::Details) {
                                                this.node_editor
                                                    .read(cx)
                                                    .focus_handle(cx)
                                                    .focus(window);
                                            }
                                            this.schedule_workspace_persist(cx);
                                            cx.notify();
                                        }),
                                    )
                                    .child(PanelId::Details.title()),
                            )
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
                                        cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                            if this.dock_state.is_panel_active(PanelId::Assistant) {
                                                this.chat_panel.update(cx, |panel, _cx| {
                                                    panel.last_render_us = 0
                                                });
                                            }
                                            this.dock_state.toggle_panel(PanelId::Assistant);
                                            this.sync_dock_presentational_state(cx);
                                            if this.dock_state.is_panel_active(PanelId::Assistant) {
                                                this.chat_panel
                                                    .read(cx)
                                                    .focus_handle(cx)
                                                    .focus(window);
                                            }
                                            this.schedule_workspace_persist(cx);
                                            cx.notify();
                                        }),
                                    )
                                    .child(PanelId::Assistant.title()),
                            )
                            .when(show_advanced_controls, |status| {
                                status.child(
                                    div()
                                        .id("status-perf-btn")
                                        .flex()
                                        .items_center()
                                        .px_2()
                                        .h(px(STATUS_BAR_H - 4.0))
                                        .cursor_pointer()
                                        .text_color(if perf_enabled {
                                            rgba(0xa6e3a1ff)
                                        } else {
                                            rgba(0x6c7086ff)
                                        })
                                        .when(perf_enabled, |el| el.bg(rgba(0x45475a88)))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                                this.perf_enabled = !this.perf_enabled;
                                                if !this.perf_enabled {
                                                    this.frame_times_us.clear();
                                                }
                                                cx.notify();
                                            }),
                                        )
                                        .child("Perf"),
                                )
                            }),
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
                                        .text_xs()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(rgba(0x45475a88)))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                                this.do_save_active(cx);
                                                this.file_menu_open = false;
                                                cx.notify();
                                            }),
                                        )
                                        .child("Save Changes        Ctrl+S"),
                                )
                                .child(
                                    div()
                                        .id("save-all-item")
                                        .flex()
                                        .items_center()
                                        .h(px(28.0))
                                        .px_3()
                                        .text_color(rgba(0xcdd6f4ff))
                                        .text_xs()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(rgba(0x45475a88)))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                                this.do_save_all(cx);
                                                this.file_menu_open = false;
                                                cx.notify();
                                            }),
                                        )
                                        .child("Save All       Ctrl+Shift+S"),
                                )
                                // ── separator ──
                                .child(div().h(px(1.0)).w_full().bg(rgb(0x45475a)))
                                .child(
                                    div()
                                        .id("lemonade-setup-item")
                                        .flex()
                                        .items_center()
                                        .h(px(28.0))
                                        .px_3()
                                        .text_color(rgba(0xcdd6f4ff))
                                        .text_xs()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(rgba(0x45475a88)))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                                this.file_menu_open = false;
                                                this.setup_open = true;
                                                this.do_refresh_lemonade_setup(cx);
                                                cx.notify();
                                            }),
                                        )
                                        .child("Lemonade AI Setup…"),
                                )
                                // ── separator ──
                                .child(div().h(px(1.0)).w_full().bg(rgb(0x45475a)))
                                // Import Schema… — always enabled
                                .child(
                                    div()
                                        .id("import-schema-item")
                                        .flex()
                                        .items_center()
                                        .h(px(28.0))
                                        .px_3()
                                        .text_color(rgba(0xcdd6f4ff))
                                        .text_xs()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(rgba(0x45475a88)))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                                this.file_menu_open = false;
                                                this.do_import_schema_picker(window, cx);
                                            }),
                                        )
                                        .child("Import Schema…"),
                                )
                                // Import Data… — greyed when no schema loaded
                                .child({
                                    let el = div()
                                        .id("import-data-item")
                                        .flex()
                                        .items_center()
                                        .h(px(28.0))
                                        .px_3()
                                        .text_xs();
                                    if has_schema {
                                        el.text_color(rgba(0xcdd6f4ff))
                                            .cursor_pointer()
                                            .hover(|s| s.bg(rgba(0x45475a88)))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(
                                                    |this, _: &MouseDownEvent, window, cx| {
                                                        this.file_menu_open = false;
                                                        this.do_import_data_picker(window, cx);
                                                    },
                                                ),
                                            )
                                    } else {
                                        el.text_color(rgba(0xcdd6f450))
                                    }
                                    .child("Import Data…")
                                })
                                // Export Data… — greyed when no data
                                .child({
                                    let el = div()
                                        .id("export-data-item")
                                        .flex()
                                        .items_center()
                                        .h(px(28.0))
                                        .px_3()
                                        .text_xs();
                                    if has_data {
                                        el.text_color(rgba(0xcdd6f4ff))
                                            .cursor_pointer()
                                            .hover(|s| s.bg(rgba(0x45475a88)))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(
                                                    |this, _: &MouseDownEvent, window, cx| {
                                                        this.file_menu_open = false;
                                                        this.do_export_data_picker(window, cx);
                                                    },
                                                ),
                                            )
                                    } else {
                                        el.text_color(rgba(0xcdd6f450))
                                    }
                                    .child("Export Data…")
                                })
                                // ── separator ──
                                .child(div().h(px(1.0)).w_full().bg(rgb(0x45475a)))
                                // Clear Schema — greyed when no schemas
                                .child({
                                    let el = div()
                                        .id("clear-schema-item")
                                        .flex()
                                        .items_center()
                                        .h(px(28.0))
                                        .px_3()
                                        .text_xs();
                                    if has_schema {
                                        el.text_color(rgba(0xf38ba8ff))
                                            .cursor_pointer()
                                            .hover(|s| s.bg(rgba(0x45475a88)))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(
                                                    |this, _: &MouseDownEvent, _window, cx| {
                                                        this.file_menu_open = false;
                                                        this.request_clear_schema(cx);
                                                    },
                                                ),
                                            )
                                    } else {
                                        el.text_color(rgba(0xf38ba850))
                                    }
                                    .child("Clear Schema")
                                })
                                // Clear Data — greyed when no data
                                .child({
                                    let el = div()
                                        .id("clear-data-item")
                                        .flex()
                                        .items_center()
                                        .h(px(28.0))
                                        .px_3()
                                        .text_xs();
                                    if has_data {
                                        el.text_color(rgba(0xf38ba8ff))
                                            .cursor_pointer()
                                            .hover(|s| s.bg(rgba(0x45475a88)))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(
                                                    |this, _: &MouseDownEvent, _window, cx| {
                                                        this.file_menu_open = false;
                                                        this.request_clear_data(cx);
                                                    },
                                                ),
                                            )
                                    } else {
                                        el.text_color(rgba(0xf38ba850))
                                    }
                                    .child("Clear Data")
                                }),
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
                                        .text_xs()
                                        .cursor_pointer()
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                                this.dock_state.toggle_panel(PanelId::World);
                                                if this.dock_state.is_panel_active(PanelId::World) {
                                                    this.node_panel
                                                        .read(cx)
                                                        .focus_handle(cx)
                                                        .focus(window);
                                                }
                                                this.schedule_workspace_persist(cx);
                                                this.view_menu_open = false;
                                                cx.notify();
                                            }),
                                        )
                                        .child(if sidebar_open && sidebar_tab == PanelId::World {
                                            "  World            Ctrl+B"
                                        } else {
                                            "    World            Ctrl+B"
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
                                        .text_xs()
                                        .cursor_pointer()
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                                if this
                                                    .dock_state
                                                    .is_panel_active(PanelId::Assistant)
                                                {
                                                    this.chat_panel.update(cx, |panel, _cx| {
                                                        panel.last_render_us = 0;
                                                    });
                                                }
                                                this.dock_state.toggle_panel(PanelId::Assistant);
                                                this.sync_dock_presentational_state(cx);
                                                if this
                                                    .dock_state
                                                    .is_panel_active(PanelId::Assistant)
                                                {
                                                    this.chat_panel
                                                        .read(cx)
                                                        .focus_handle(cx)
                                                        .focus(window);
                                                }
                                                this.schedule_workspace_persist(cx);
                                                this.view_menu_open = false;
                                                cx.notify();
                                            }),
                                        )
                                        .child(if right_panel_open {
                                            "  Assistant        Ctrl+J"
                                        } else {
                                            "    Assistant        Ctrl+J"
                                        }),
                                )
                                // Bottom Details toggle
                                .child(
                                    div()
                                        .id("toggle-details-item")
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .h(px(28.0))
                                        .px_3()
                                        .text_color(rgba(0xcdd6f4ff))
                                        .text_xs()
                                        .cursor_pointer()
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                                this.dock_state.toggle_panel(PanelId::Details);
                                                if this.dock_state.is_panel_active(PanelId::Details)
                                                {
                                                    this.node_editor
                                                        .read(cx)
                                                        .focus_handle(cx)
                                                        .focus(window);
                                                }
                                                this.schedule_workspace_persist(cx);
                                                this.view_menu_open = false;
                                                cx.notify();
                                            }),
                                        )
                                        .child(if details_open {
                                            "  Details      Ctrl+Shift+J"
                                        } else {
                                            "    Details      Ctrl+Shift+J"
                                        }),
                                )
                                .child(div().h(px(1.0)).w_full().bg(theme.colors.border))
                                .child(
                                    div()
                                        .id("open-settings-item")
                                        .flex()
                                        .items_center()
                                        .h(px(28.0))
                                        .px_3()
                                        .text_color(theme.colors.text)
                                        .text_xs()
                                        .cursor_pointer()
                                        .hover(|style| style.bg(theme.colors.selected))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                                this.view_menu_open = false;
                                                this.open_settings(cx);
                                            }),
                                        )
                                        .child("Settings…               Ctrl+,"),
                                ),
                        ),
                ))
            })
            // ── Path picker modal ─────────────────────────────────────────────
            .when(self.path_picker.is_some(), |root| {
                root.child(self.path_picker.as_ref().unwrap().1.clone())
            })
            // ── Destructive-action confirmation ──────────────────────────────
            .when(self.confirmation.is_some(), |root| {
                root.child(self.confirmation.as_ref().unwrap().clone())
            })
            // ── User-facing UI settings ─────────────────────────────────────
            .when(settings_open, |root| {
                let decrease = handle.clone();
                let increase = handle.clone();
                let toggle_advanced = handle.clone();
                let cancel = handle.clone();
                let save = handle.clone();
                let body = div()
                    .flex()
                    .flex_col()
                    .gap(px(theme.metrics.space_6))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child("Text size")
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(theme.metrics.space_2))
                                    .child(
                                        Button::new("settings-font-smaller", "Smaller").on_click(
                                            move |_, _, cx| {
                                                decrease
                                                    .update(cx, |view, cx| {
                                                        view.settings_draft_font_size =
                                                            (view.settings_draft_font_size - 1.0)
                                                                .max(10.0);
                                                        cx.notify();
                                                    })
                                                    .ok();
                                            },
                                        ),
                                    )
                                    .child(format!("{:.0} pt", self.settings_draft_font_size))
                                    .child(Button::new("settings-font-larger", "Larger").on_click(
                                        move |_, _, cx| {
                                            increase
                                                .update(cx, |view, cx| {
                                                    view.settings_draft_font_size =
                                                        (view.settings_draft_font_size + 1.0)
                                                            .min(28.0);
                                                    cx.notify();
                                                })
                                                .ok();
                                        },
                                    )),
                            ),
                    )
                    .child(
                        Button::new(
                            "settings-advanced",
                            if self.settings_draft_advanced {
                                "Advanced controls shown"
                            } else {
                                "Advanced controls hidden"
                            },
                        )
                        .selected(self.settings_draft_advanced)
                        .tooltip("Show technical diagnostics and model-tuning controls")
                        .on_click(move |_, _, cx| {
                            toggle_advanced
                                .update(cx, |view, cx| {
                                    view.settings_draft_advanced = !view.settings_draft_advanced;
                                    cx.notify();
                                })
                                .ok();
                        }),
                    )
                    .child(div().text_xs().text_color(theme.colors.text_muted).child(
                        if show_advanced_controls {
                            "Advanced diagnostics are currently available."
                        } else {
                            "Everyday worldbuilding features remain visible."
                        },
                    ));
                let dialog = Dialog::new("Settings", body)
                    .action(
                        Button::new("settings-cancel", "Cancel").on_click(move |_, _, cx| {
                            cancel
                                .update(cx, |view, cx| {
                                    view.settings_open = false;
                                    cx.notify();
                                })
                                .ok();
                        }),
                    )
                    .action(
                        Button::new("settings-save", "Save Settings")
                            .style(ButtonStyle::Filled)
                            .on_click(move |_, _, cx| {
                                save.update(cx, |view, cx| view.save_settings(cx)).ok();
                            }),
                    );
                root.child(
                    div()
                        .id("settings-overlay")
                        .absolute()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(theme.colors.overlay)
                        .child(dialog),
                )
            })
            // ── Reopenable Lemonade setup ───────────────────────────────────
            .when(setup_open, |root| root.child(self.setup_panel.clone()))
            // ── Startup first-paint milestone ────────────────────────────────
            .when(app_first_paint_pending, |root| {
                root.child(
                    canvas(
                        |_, _, _| {},
                        move |_, (), _, cx| {
                            if startup.milestone(StartupMilestone::AppFirstPaint)
                                && startup.should_exit_after(StartupMilestone::AppFirstPaint)
                            {
                                cx.quit();
                            }
                        },
                    )
                    .absolute()
                    .top_0()
                    .left_0()
                    .w(px(1.0))
                    .h(px(1.0)),
                )
            })
            // ── Frame-cost timing canvas ──────────────────────────────────────
            // Zero-size absolute element; its paint closure fires after GPUI
            // completes the full layout pass, making elapsed() an honest measure
            // of frame cost (tree build + layout + paint start).
            .when(perf_enabled, |root| {
                root.child(
                    canvas(
                        |_, _, _| {},
                        move |_, (), _, cx| {
                            let elapsed_us = frame_start.elapsed().as_micros() as u64;
                            timing_entity.update(cx, |this, _cx| {
                                this.last_frame_cost_us = elapsed_us;
                                this.frame_times_us.push(elapsed_us);
                            });
                        },
                    )
                    .absolute()
                    .top_0()
                    .left_0()
                    .w(px(1.0))
                    .h(px(1.0)),
                )
            })
    }
}
