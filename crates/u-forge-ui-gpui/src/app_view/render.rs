use std::time::Instant;

use gpui::{
    AnyElement, AnyView, App, ClickEvent, Context, Corner, FocusHandle, MouseButton,
    MouseDownEvent, Render, StyleRefinement, Window, anchored, canvas, deferred, div, point,
    prelude::*, px, relative,
};

use crate::actions::{
    ActionContext, ActionDescriptor, ActionId, ActionMenu, ActionTone, StatusSide,
    menu_descriptors, status_descriptors,
};
use crate::{
    ClearData, ClearSchema, DetailsCloseTab, DetailsNextTab, DetailsPreviousTab, ExportData,
    FitGraph, FocusNextRegion, FocusPreviousRegion, ImportData, ImportSchema, OpenLemonadeSetup,
    OpenSettings, SaveActiveItem, SaveAllItems, ToggleDetailsPanel, ToggleFocusedPanelZoom,
    TogglePerfOverlay, ToggleRightPanel, ToggleSearchPanel, ToggleSidebar,
};

use super::{AppView, ResizeEditorCanvas, ResizeRightPanel, ResizeSidebar};
use crate::dock_state::RESIZE_HANDLE_SIZE;
use crate::panel_contracts::{DockPosition, PanelId, WorkspaceItemId, WorldCanvasViewId};
use crate::search_panel::SearchPanelStatus;
use crate::startup::StartupMilestone;
use crate::ui::components::{
    Button, ButtonStyle, Dialog, LabelTone, Menu, MenuItem, StatusItem, StatusTone, Tab as UiTab,
};
use crate::ui::theme::UiTheme;

fn operation_status_tone(message: &str) -> StatusTone {
    let message = message.to_ascii_lowercase();
    if ["failed", "cannot", "error"]
        .iter()
        .any(|marker| message.contains(marker))
    {
        StatusTone::Danger
    } else if ["loading", "importing", "exporting", "checking"]
        .iter()
        .any(|marker| message.contains(marker))
    {
        StatusTone::Warning
    } else {
        StatusTone::Success
    }
}

fn action_menu_entries(
    menu: ActionMenu,
    context: &ActionContext,
    return_focus: FocusHandle,
    handle: gpui::WeakEntity<AppView>,
    theme: UiTheme,
) -> Vec<AnyElement> {
    let mut entries = Vec::new();
    let mut previous_section = None;
    for action_descriptor in menu_descriptors(menu, context) {
        let placement = action_descriptor
            .menu
            .expect("menu descriptors must have a menu placement");
        if previous_section.is_some_and(|section| section != placement.section) {
            entries.push(
                div()
                    .h(px(1.0))
                    .w_full()
                    .bg(theme.colors.border)
                    .into_any_element(),
            );
        }

        let enabled = action_descriptor.is_enabled(context);
        let action = action_descriptor.action();
        let action_handle = handle.clone();
        let action_focus = return_focus.clone();
        let mut item = MenuItem::new(action_descriptor.element_id, action_descriptor.label)
            .disabled(!enabled)
            .selected(action_descriptor.is_selected(context))
            .tooltip(action_descriptor.display_tooltip())
            .tone(match action_descriptor.tone {
                ActionTone::Normal => LabelTone::Primary,
                ActionTone::Danger => LabelTone::Danger,
            });
        if let Some(shortcut) = action_descriptor.shortcut {
            item = item.shortcut(shortcut.display);
        }
        if enabled {
            item = item.on_click(move |_, window, cx| {
                action_handle
                    .update(cx, |view, cx| {
                        match menu {
                            ActionMenu::File => view.file_menu_open = false,
                            ActionMenu::View => view.view_menu_open = false,
                        }
                        cx.notify();
                    })
                    .ok();
                action_focus.focus(window);
                window.dispatch_action(action.boxed_clone(), cx);
            });
        }
        entries.push(item.into_any_element());
        previous_section = Some(placement.section);
    }
    entries
}

fn status_action_item(action_descriptor: &ActionDescriptor, context: &ActionContext) -> StatusItem {
    let placement = action_descriptor
        .status
        .expect("status descriptors must have a status placement");
    let selected = action_descriptor.is_selected(context);
    let mut item = StatusItem::new(placement.element_id, placement.label)
        .active(selected)
        .tooltip(action_descriptor.display_tooltip())
        .boxed_action(action_descriptor.action());
    if let Some(icon) = placement.icon {
        item = item.icon(icon);
    }
    if placement.icon_only {
        item = item.icon_only();
    }
    if action_descriptor.id == ActionId::TogglePerfOverlay {
        item = item.tone(if selected {
            StatusTone::Success
        } else {
            StatusTone::Muted
        });
    }
    item
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(px(self.ui_font_size));
        let theme = *UiTheme::get(cx);
        let menu_bar_height = theme.metrics.menu_bar_height;

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
        let search_status = self.search_panel.read(cx).status();
        let inference_ready = self.state.inference_queue.is_some();
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
        drop(snap);
        let action_context = self.action_context(cx);

        // Weak handle used by drag-move closures to update panel sizes.
        let handle = cx.weak_entity();

        div()
            .id("app-root")
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.colors.app_surface)
            // Handle actions dispatched from native menu or keybindings.
            .when(
                crate::actions::descriptor(ActionId::SaveActiveItem)
                    .is_enabled(&action_context),
                |root| {
                    root.on_action(cx.listener(|this, _: &SaveActiveItem, _window, cx| {
                        this.do_save_active(cx);
                        cx.notify();
                    }))
                },
            )
            .when(
                crate::actions::descriptor(ActionId::SaveAllItems).is_enabled(&action_context),
                |root| {
                    root.on_action(cx.listener(|this, _: &SaveAllItems, _window, cx| {
                        this.do_save_all(cx);
                        cx.notify();
                    }))
                },
            )
            .on_action(cx.listener(|this, _: &OpenLemonadeSetup, _window, cx| {
                this.setup_open = true;
                this.do_refresh_lemonade_setup(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                this.open_settings(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleFocusedPanelZoom, window, cx| {
                this.toggle_focused_panel_zoom(window, cx);
                this.refresh_native_menus(cx);
            }))
            .on_action(cx.listener(|this, _: &FocusNextRegion, window, cx| {
                this.cycle_workspace_focus(false, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusPreviousRegion, window, cx| {
                this.cycle_workspace_focus(true, window, cx);
            }))
            .when(
                crate::actions::descriptor(ActionId::DetailsNextTab)
                    .is_enabled(&action_context),
                |root| {
                    root.on_action(cx.listener(|this, _: &DetailsNextTab, _window, cx| {
                        this.node_editor
                            .update(cx, |editor, cx| editor.activate_relative_tab(false, cx));
                    }))
                },
            )
            .when(
                crate::actions::descriptor(ActionId::DetailsPreviousTab)
                    .is_enabled(&action_context),
                |root| {
                    root.on_action(cx.listener(|this, _: &DetailsPreviousTab, _window, cx| {
                        this.node_editor
                            .update(cx, |editor, cx| editor.activate_relative_tab(true, cx));
                    }))
                },
            )
            .when(
                crate::actions::descriptor(ActionId::DetailsCloseTab)
                    .is_enabled(&action_context),
                |root| {
                    root.on_action(cx.listener(|this, _: &DetailsCloseTab, window, cx| {
                        this.request_close_active_editor_tab(window, cx);
                    }))
                },
            )
            .when(
                crate::actions::descriptor(ActionId::FitGraph).is_enabled(&action_context),
                |root| {
                    root.on_action(cx.listener(|this, _: &FitGraph, _window, cx| {
                        this.graph_canvas
                            .update(cx, |canvas, cx| canvas.fit_graph(cx));
                    }))
                },
            )
            .on_action(cx.listener(|this, _: &ToggleSidebar, window, cx| {
                this.toggle_dock_panel(PanelId::World, window, cx);
                this.refresh_native_menus(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleSearchPanel, window, cx| {
                this.toggle_dock_panel(PanelId::Search, window, cx);
                this.refresh_native_menus(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleRightPanel, window, cx| {
                this.toggle_dock_panel(PanelId::Assistant, window, cx);
                this.refresh_native_menus(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleDetailsPanel, window, cx| {
                this.toggle_dock_panel(PanelId::Details, window, cx);
                this.refresh_native_menus(cx);
            }))
            .when(
                crate::actions::descriptor(ActionId::ClearData).is_enabled(&action_context),
                |root| {
                    root.on_action(cx.listener(|this, _: &ClearData, window, cx| {
                        this.request_clear_data(window, cx);
                    }))
                },
            )
            .when(
                crate::actions::descriptor(ActionId::ClearSchema).is_enabled(&action_context),
                |root| {
                    root.on_action(cx.listener(|this, _: &ClearSchema, window, cx| {
                        this.request_clear_schema(window, cx);
                    }))
                },
            )
            .when(
                crate::actions::descriptor(ActionId::ImportData).is_enabled(&action_context),
                |root| {
                    root.on_action(cx.listener(|this, _: &ImportData, window, cx| {
                        this.do_import_data_picker(window, cx);
                    }))
                },
            )
            .on_action(cx.listener(|this, _: &ImportSchema, window, cx| {
                this.do_import_schema_picker(window, cx);
            }))
            .when(
                crate::actions::descriptor(ActionId::ExportData).is_enabled(&action_context),
                |root| {
                    root.on_action(cx.listener(|this, _: &ExportData, window, cx| {
                        this.do_export_data_picker(window, cx);
                    }))
                },
            )
            .when(
                crate::actions::descriptor(ActionId::TogglePerfOverlay)
                    .is_enabled(&action_context),
                |root| {
                    root.on_action(cx.listener(|this, _: &TogglePerfOverlay, _window, cx| {
                        this.perf_enabled = !this.perf_enabled;
                        if !this.perf_enabled {
                            this.frame_times_us.clear();
                        }
                        this.refresh_native_menus(cx);
                        cx.notify();
                    }))
                },
            )
            // ── Menu bar ──────────────────────────────────────────────────────
            .child(
                div()
                    .id("menu-bar")
                    .flex()
                    .flex_none()
                    .h(theme.metrics.menu_bar_height)
                    .w_full()
                    .bg(theme.colors.panel_surface)
                    .border_b_1()
                    .border_color(theme.colors.border_subtle)
                    .items_center()
                    .child(
                        // "File" menu button
                        div()
                            .id("file-btn")
                            .key_context("MenuBar")
                            .track_focus(&self.file_menu_button_focus)
                            .tab_index(0)
                            .flex()
                            .items_center()
                            .h_full()
                            .px_3()
                            .text_color(theme.colors.text)
                            .text_size(theme.typography.chrome)
                            .cursor_pointer()
                            .focus_visible(move |style| {
                                style.border_1().border_color(theme.colors.focus)
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                    this.file_menu_button_focus.focus(window);
                                    this.file_menu_open = !this.file_menu_open;
                                    this.view_menu_open = false;
                                    if this.file_menu_open {
                                        let focus = this.file_menu_focus.clone();
                                        window.defer(cx, move |window, _cx| focus.focus(window));
                                    }
                                    cx.notify();
                                }),
                            )
                            .child("File"),
                    )
                    .child(
                        // "View" menu button
                        div()
                            .id("view-btn")
                            .key_context("MenuBar")
                            .track_focus(&self.view_menu_button_focus)
                            .tab_index(0)
                            .flex()
                            .items_center()
                            .h_full()
                            .px_3()
                            .text_color(theme.colors.text)
                            .text_size(theme.typography.chrome)
                            .cursor_pointer()
                            .focus_visible(move |style| {
                                style.border_1().border_color(theme.colors.focus)
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                    this.view_menu_button_focus.focus(window);
                                    this.view_menu_open = !this.view_menu_open;
                                    this.file_menu_open = false;
                                    if this.view_menu_open {
                                        let focus = this.view_menu_focus.clone();
                                        window.defer(cx, move |window, _cx| focus.focus(window));
                                    }
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
                        .hover(move |s: StyleRefinement| s.bg(theme.colors.selected))
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
                        .hover(move |s: StyleRefinement| s.bg(theme.colors.selected))
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
                            .h(theme.metrics.panel_header_height)
                            .px_3()
                            .gap(px(theme.metrics.space_4))
                            .bg(theme.colors.panel_surface)
                            .border_b_1()
                            .border_color(theme.colors.border_subtle)
                            .text_size(theme.typography.label)
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
                        .hover(move |s: StyleRefinement| s.bg(theme.colors.selected))
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
                    .h(theme.metrics.status_bar_height)
                    .w_full()
                    .bg(theme.colors.panel_surface)
                    .border_t_1()
                    .border_color(theme.colors.border_subtle)
                    .items_center()
                    .text_size(theme.typography.chrome)
                    .child(
                        div()
                            .id("status-left")
                            .flex()
                            .flex_row()
                            .items_center()
                            .flex_none()
                            .gap(px(2.0))
                            .px_1()
                            .children(
                                status_descriptors(StatusSide::Left, &action_context)
                                    .map(|descriptor| {
                                        status_action_item(descriptor, &action_context)
                                    }),
                            ),
                    )
                    .child({
                        let data_status = self.state.data_status.clone();
                        let mut center = div()
                            .id("status-center")
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_center()
                            .min_w_0()
                            .overflow_hidden()
                            .gap(px(2.0));
                        center.style().flex_grow = Some(1.0);
                        center = center.child(
                            StatusItem::new(
                                "status-counts",
                                format!("{node_count} items · {edge_count} relationships"),
                            )
                            .tone(StatusTone::Muted)
                            .tooltip("World Canvas item and relationship counts"),
                        );
                        if let Some(msg) = data_status {
                            center = center.child(
                                StatusItem::new("status-data", msg.clone())
                                    .tone(operation_status_tone(&msg))
                                    .tooltip(msg),
                            );
                        }
                        if let Some(status) = search_status {
                            center = center.child(match status {
                                SearchPanelStatus::Searching => {
                                    StatusItem::new("status-search-activity", "Searching…")
                                        .tone(StatusTone::Normal)
                                        .tooltip("Searching the current world")
                                }
                                SearchPanelStatus::Degraded(message) => {
                                    StatusItem::new("status-search-activity", message.clone())
                                        .tone(StatusTone::Warning)
                                        .tooltip(message)
                                }
                                SearchPanelStatus::Failed(message) => {
                                    StatusItem::new("status-search-activity", "Search unavailable")
                                        .tone(StatusTone::Danger)
                                        .tooltip(message)
                                }
                            });
                        }
                        if let Some(msg) = embedding_status {
                            center = center.child(
                                StatusItem::new("status-embedding", msg.clone())
                                    .tone(StatusTone::Warning)
                                    .tooltip(msg),
                            );
                        }
                        if let Some(perf) = perf_text {
                            center = center.child(
                                StatusItem::new("status-performance", perf.clone())
                                    .tone(StatusTone::Success)
                                    .tooltip(perf),
                            );
                        }
                        center = center.child(
                            StatusItem::new(
                                "status-inference",
                                if inference_ready {
                                    "AI ready"
                                } else {
                                    "AI unavailable"
                                },
                            )
                            .tone(if inference_ready {
                                StatusTone::Success
                            } else {
                                StatusTone::Muted
                            })
                            .tooltip(if inference_ready {
                                "Local AI capabilities are ready"
                            } else {
                                "Worldbuilding remains available without local AI"
                            }),
                        );
                        center
                    })
                    .child(
                        div()
                            .id("status-right")
                            .flex()
                            .flex_row()
                            .items_center()
                            .flex_none()
                            .gap(px(2.0))
                            .px_1()
                            .children(
                                status_descriptors(StatusSide::Right, &action_context)
                                    .map(|descriptor| {
                                        status_action_item(descriptor, &action_context)
                                    }),
                            ),
                    ),
            )
            // ── File dropdown overlay ─────────────────────────────────────────
            .when(file_menu_open, |root| {
                root.child(deferred(
                    anchored()
                        .position(point(px(0.0), menu_bar_height))
                        .anchor(Corner::TopLeft)
                        .child(
                            Menu::new(
                                "file-dropdown",
                                div()
                                    .w(px(220.0))
                                    .children(action_menu_entries(
                                        ActionMenu::File,
                                        &action_context,
                                        self.file_menu_button_focus.clone(),
                                        handle.clone(),
                                        theme,
                                    )),
                            )
                            .focus_handle(self.file_menu_focus.clone())
                            .return_focus(self.file_menu_button_focus.clone())
                            .on_dismiss({
                                let handle = handle.clone();
                                move |_window, cx| {
                                    handle
                                        .update(cx, |view, cx| {
                                            view.file_menu_open = false;
                                            cx.notify();
                                        })
                                        .ok();
                                }
                            }),
                        ),
                ))
            })
            // ── View dropdown overlay ─────────────────────────────────────────
            .when(view_menu_open, |root| {
                // Position horizontally after the "File" button (~30px wide + padding).
                root.child(deferred(
                    anchored()
                        .position(point(px(32.0), menu_bar_height))
                        .anchor(Corner::TopLeft)
                        .child(
                            Menu::new(
                                "view-dropdown",
                                div()
                                    .w(px(250.0))
                                    .children(action_menu_entries(
                                        ActionMenu::View,
                                        &action_context,
                                        self.view_menu_button_focus.clone(),
                                        handle.clone(),
                                        theme,
                                    )),
                            )
                            .focus_handle(self.view_menu_focus.clone())
                            .return_focus(self.view_menu_button_focus.clone())
                            .on_dismiss({
                                let handle = handle.clone();
                                move |_window, cx| {
                                    handle
                                        .update(cx, |view, cx| {
                                            view.view_menu_open = false;
                                            cx.notify();
                                        })
                                        .ok();
                                }
                            }),
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
                let decrease_interface = handle.clone();
                let increase_interface = handle.clone();
                let toggle_advanced = handle.clone();
                let cancel = handle.clone();
                let dismiss = handle.clone();
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
                                    .child(format!("{:.0} px", self.settings_draft_font_size))
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
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child("Interface size")
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(theme.metrics.space_2))
                                    .child(
                                        Button::new("settings-interface-smaller", "Smaller")
                                            .on_click(move |_, _, cx| {
                                                decrease_interface
                                                    .update(cx, |view, cx| {
                                                        view.settings_draft_interface_size = (view
                                                            .settings_draft_interface_size
                                                            - 1.0)
                                                            .max(14.0);
                                                        cx.notify();
                                                    })
                                                    .ok();
                                            }),
                                    )
                                    .child(format!("{:.0} px", self.settings_draft_interface_size))
                                    .child(
                                        Button::new("settings-interface-larger", "Larger")
                                            .on_click(move |_, _, cx| {
                                                increase_interface
                                                    .update(cx, |view, cx| {
                                                        view.settings_draft_interface_size = (view
                                                            .settings_draft_interface_size
                                                            + 1.0)
                                                            .min(32.0);
                                                        cx.notify();
                                                    })
                                                    .ok();
                                            }),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .text_size(theme.typography.caption)
                            .text_color(theme.colors.text_muted)
                            .child(
                                "Text size affects readable content; interface size affects panels, controls, spacing, and icons.",
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
                let mut dialog = Dialog::new("Settings", body)
                    .id("settings-dialog")
                    .focus_handle(self.settings_focus.clone())
                    .on_dismiss(move |window, cx| {
                        dismiss
                            .update(cx, |view, cx| view.close_settings(window, cx))
                            .ok();
                    })
                    .action(
                        Button::new("settings-cancel", "Cancel").on_click(move |_, window, cx| {
                            cancel
                                .update(cx, |view, cx| view.close_settings(window, cx))
                                .ok();
                        }),
                    )
                    .action(
                        Button::new("settings-save", "Save Settings")
                            .style(ButtonStyle::Filled)
                            .on_click(move |_, window, cx| {
                                save.update(cx, |view, cx| view.save_settings(window, cx))
                                    .ok();
                            }),
                    );
                if let Some(return_focus) = self.settings_return_focus.clone() {
                    dialog = dialog.return_focus(return_focus);
                }
                root.child(
                    div()
                        .id("settings-overlay")
                        .absolute()
                        .size_full()
                        .occlude()
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

#[cfg(test)]
mod tests {
    use super::operation_status_tone;
    use crate::ui::components::StatusTone;

    #[test]
    fn operation_status_tones_distinguish_progress_success_and_failure() {
        assert_eq!(operation_status_tone("Importing…"), StatusTone::Warning);
        assert_eq!(operation_status_tone("Data imported."), StatusTone::Success);
        assert_eq!(
            operation_status_tone("Cannot save unfinished relationships."),
            StatusTone::Danger
        );
    }
}
