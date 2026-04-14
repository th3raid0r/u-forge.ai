use std::sync::Arc;

use glam::Vec2;
use gpui::{
    anchored, canvas, deferred, div, fill, font, point, prelude::*, px, rgb, rgba, size, App,
    Application, Bounds, Context, Corner, Entity, Font, Hsla, KeyBinding, Menu, MenuItem,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels, Point,
    ScrollDelta, ScrollWheelEvent, SharedString, Subscription, TextRun, Window, WindowBounds,
    WindowOptions, actions, relative,
};
use parking_lot::RwLock;
use u_forge_core::{AppConfig, KnowledgeGraph, ObjectId};
use u_forge_graph_view::{build_snapshot, GraphSnapshot, LodLevel};
use u_forge_ui_traits::{generate_draw_commands, DrawCommand, Viewport, NODE_RADIUS};

actions!([SaveLayout, ToggleSidebar]);

/// Edges per batched PathBuilder.
const EDGE_BATCH_SIZE: usize = 500;

/// Legend entries matching the `type_color` palette in u-forge-ui-traits.
const LEGEND_ENTRIES: &[(&str, u32)] = &[
    ("Character / NPC", 0x89b4fa),
    ("Location", 0xa6e3a1),
    ("Faction", 0xf9e2af),
    ("Quest", 0xf38ba8),
    ("Item / Transport", 0xcba6f7),
    ("Currency", 0x94e2d5),
    ("Other", 0xcdd6f4),
];

/// Menu bar height in pixels.
const MENU_BAR_H: f32 = 28.0;

// ── Selection model ──────────────────────────────────────────────────────────

/// Shared selection state observed by both TreePanel and GraphCanvas.
/// When either side changes the selection, it calls `cx.notify()` so
/// observers re-render.
struct SelectionModel {
    /// Index into `GraphSnapshot::nodes` of the currently selected node.
    selected_node_idx: Option<usize>,
    /// ObjectId of the currently selected node (kept in sync with idx).
    selected_node_id: Option<ObjectId>,
    /// The shared snapshot — both panels read from this.
    snapshot: Arc<RwLock<GraphSnapshot>>,
}

impl SelectionModel {
    fn new(snapshot: Arc<RwLock<GraphSnapshot>>) -> Self {
        Self {
            selected_node_idx: None,
            selected_node_id: None,
            snapshot,
        }
    }

    /// Select a node by its snapshot index. Called from graph canvas clicks.
    fn select_by_idx(&mut self, idx: Option<usize>, cx: &mut Context<Self>) {
        self.selected_node_idx = idx;
        self.selected_node_id = idx.map(|i| self.snapshot.read().nodes[i].id);
        cx.notify();
    }

    /// Select a node by ObjectId. Called from tree panel clicks.
    /// Returns the node index if found.
    fn select_by_id(&mut self, id: Option<ObjectId>, cx: &mut Context<Self>) -> Option<usize> {
        if let Some(id) = id {
            let snap = self.snapshot.read();
            let idx = snap.nodes.iter().position(|n| n.id == id);
            drop(snap);
            self.selected_node_idx = idx;
            self.selected_node_id = Some(id);
            cx.notify();
            idx
        } else {
            self.selected_node_idx = None;
            self.selected_node_id = None;
            cx.notify();
            None
        }
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        self.selected_node_idx = None;
        self.selected_node_id = None;
        cx.notify();
    }
}

// ── Tree panel ──────────────────────────────────────────────────────────────

/// A group of nodes sharing the same object_type, for the tree panel.
struct TypeGroup {
    type_name: String,
    /// (index into snapshot.nodes, display name)
    entries: Vec<(usize, String, ObjectId)>,
}

/// Sidebar tree view listing all nodes grouped by type, alphabetically.
struct TreePanel {
    selection: Entity<SelectionModel>,
    /// Pre-sorted groups, rebuilt when the snapshot changes.
    groups: Vec<TypeGroup>,
    /// Which type groups are collapsed (by type_name).
    collapsed: std::collections::HashSet<String>,
}

impl TreePanel {
    fn new(
        snapshot: Arc<RwLock<GraphSnapshot>>,
        selection: Entity<SelectionModel>,
    ) -> Self {
        let groups = Self::build_groups(&snapshot.read());
        // Start with all groups collapsed so the tree fits on screen.
        let collapsed: std::collections::HashSet<String> =
            groups.iter().map(|g| g.type_name.clone()).collect();
        Self {
            selection,
            groups,
            collapsed,
        }
    }

    fn build_groups(snap: &GraphSnapshot) -> Vec<TypeGroup> {
        use std::collections::BTreeMap;
        let mut by_type: BTreeMap<String, Vec<(usize, String, ObjectId)>> = BTreeMap::new();
        for (idx, node) in snap.nodes.iter().enumerate() {
            by_type
                .entry(node.object_type.clone())
                .or_default()
                .push((idx, node.name.clone(), node.id));
        }
        let mut groups: Vec<TypeGroup> = by_type
            .into_iter()
            .map(|(type_name, mut entries)| {
                entries.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
                TypeGroup { type_name, entries }
            })
            .collect();
        // BTreeMap already sorts keys, but let's be explicit about case-insensitive sort.
        groups.sort_by(|a, b| a.type_name.to_lowercase().cmp(&b.type_name.to_lowercase()));
        groups
    }
}

/// Color for a type header in the tree panel, matching the node palette.
fn tree_type_color(object_type: &str) -> u32 {
    match object_type {
        "npc" | "character" => 0x89b4fa,
        "location" => 0xa6e3a1,
        "faction" => 0xf9e2af,
        "quest" => 0xf38ba8,
        "item" | "transportation" => 0xcba6f7,
        "currency" => 0x94e2d5,
        _ => 0xcdd6f4,
    }
}

/// Width of the sidebar in pixels.
const SIDEBAR_W: f32 = 220.0;

impl Render for TreePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected_id = self.selection.read(cx).selected_node_id;

        // Outer shell: fixed width, fills parent height, does not grow vertically.
        let mut panel = div()
            .id("tree-panel")
            .flex()
            .flex_col()
            .flex_none()
            .w(px(SIDEBAR_W))
            .h_full()
            .min_h_0()
            .bg(rgb(0x181825))
            .border_r_1()
            .border_color(rgb(0x313244));

        // Fixed header
        panel = panel.child(
            div()
                .id("tree-header")
                .flex()
                .items_center()
                .h(px(28.0))
                .px_3()
                .flex_none()
                .border_b_1()
                .border_color(rgb(0x313244))
                .text_color(rgba(0xcdd6f4ff))
                .text_xs()
                .child("NODES"),
        );

        // Scrollable content area that fills remaining height.
        let mut scroll_area = div()
            .id("tree-scroll")
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .min_h_0();
        scroll_area.style().flex_grow = Some(1.0);
        scroll_area.style().flex_shrink = Some(1.0);
        scroll_area.style().flex_basis = Some(relative(0.).into());

        // Groups
        for (group_idx, group) in self.groups.iter().enumerate() {
            let type_name = group.type_name.clone();
            let is_collapsed = self.collapsed.contains(&type_name);
            let type_color = tree_type_color(&type_name);
            let count = group.entries.len();
            let collapse_label = format!(
                "{} {} ({})",
                if is_collapsed { "▸" } else { "▾" },
                type_name,
                count
            );
            let type_name_for_click = type_name.clone();

            // Type group header
            scroll_area = scroll_area.child(
                div()
                    .id(("type-group", group_idx))
                    .flex()
                    .items_center()
                    .h(px(24.0))
                    .px_2()
                    .flex_none()
                    .text_color(rgb(type_color))
                    .text_xs()
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                            if this.collapsed.contains(&type_name_for_click) {
                                this.collapsed.remove(&type_name_for_click);
                            } else {
                                this.collapsed.insert(type_name_for_click.clone());
                            }
                            cx.notify();
                        }),
                    )
                    .child(collapse_label),
            );

            // Node entries (if not collapsed)
            if !is_collapsed {
                for (entry_idx, (_node_idx, name, node_id)) in group.entries.iter().enumerate() {
                    let is_selected = selected_id == Some(*node_id);
                    let node_id = *node_id;
                    let display_name = if name.len() > 26 {
                        let mut s: String = name.chars().take(25).collect();
                        s.push('…');
                        s
                    } else {
                        name.clone()
                    };

                    scroll_area = scroll_area.child(
                        div()
                            .id(("node-entry", group_idx * 10000 + entry_idx))
                            .flex()
                            .items_center()
                            .h(px(22.0))
                            .pl(px(20.0))
                            .pr(px(4.0))
                            .flex_none()
                            .text_xs()
                            .cursor_pointer()
                            .text_color(if is_selected {
                                rgba(0xffffffff)
                            } else {
                                rgba(0xa6adc8ff)
                            })
                            .when(is_selected, |el| el.bg(rgba(0x45475aaa)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                                    this.selection.update(cx, |sel, cx| {
                                        sel.select_by_id(Some(node_id), cx);
                                    });
                                }),
                            )
                            .child(display_name),
                    );
                }
            }
        }

        panel.child(scroll_area)
    }
}

// ── Graph canvas ─────────────────────────────────────────────────────────────

struct GraphCanvas {
    snapshot: Arc<RwLock<GraphSnapshot>>,
    graph: Arc<KnowledgeGraph>,
    selection: Entity<SelectionModel>,
    /// Camera center in world space.
    camera: Vec2,
    zoom: f32,
    /// True when the user is panning the canvas (drag on empty space).
    panning: bool,
    /// Index into `snapshot.nodes` of the node being dragged, if any.
    dragging_node: Option<usize>,
    last_mouse: Point<Pixels>,
    /// Canvas bounds in window coordinates, updated each paint frame.
    /// Used to convert window-space mouse positions to canvas-local coordinates.
    canvas_bounds: Arc<RwLock<Bounds<Pixels>>>,
    /// When true, the next selection-model observe callback skips panning
    /// (because the selection originated from a canvas click, not the tree).
    suppress_pan: bool,
    /// Subscription to selection model changes (kept alive).
    _selection_sub: Subscription,
}

impl GraphCanvas {
    fn new(
        snapshot: Arc<RwLock<GraphSnapshot>>,
        graph: Arc<KnowledgeGraph>,
        selection: Entity<SelectionModel>,
        cx: &mut Context<Self>,
    ) -> Self {
        // When the selection model changes, repaint and pan to the new selection
        // (unless the change originated from a canvas click — suppress_pan flag).
        let sel_sub = cx.observe(&selection, |this: &mut GraphCanvas, sel, cx| {
            if this.suppress_pan {
                this.suppress_pan = false;
            } else if let Some(idx) = sel.read(cx).selected_node_idx {
                let pos = this.snapshot.read().nodes[idx].position;
                this.camera = pos;
            }
            cx.notify();
        });
        Self {
            snapshot,
            graph,
            selection,
            camera: Vec2::ZERO,
            zoom: 1.0,
            panning: false,
            dragging_node: None,
            last_mouse: point(px(0.0), px(0.0)),
            canvas_bounds: Arc::new(RwLock::new(Bounds::default())),
            suppress_pan: false,
            _selection_sub: sel_sub,
        }
    }

    fn viewport(&self, canvas_size: Vec2) -> Viewport {
        Viewport {
            center: self.camera,
            size: canvas_size,
            zoom: self.zoom,
        }
    }

    /// Returns (canvas_size, canvas_origin) from the last paint frame.
    fn canvas_metrics(&self) -> (Vec2, Vec2) {
        let b = *self.canvas_bounds.read();
        (
            Vec2::new(f32::from(b.size.width), f32::from(b.size.height)),
            Vec2::new(f32::from(b.origin.x), f32::from(b.origin.y)),
        )
    }

    /// Persist all current node positions to the database.
    fn save_layout(&self) {
        let snap = self.snapshot.read();
        let positions: Vec<(ObjectId, f32, f32)> = snap
            .nodes
            .iter()
            .map(|n| (n.id, n.position.x, n.position.y))
            .collect();
        drop(snap);
        if let Err(e) = self.graph.save_layout(&positions) {
            eprintln!("Warning: failed to save layout: {e}");
        } else {
            eprintln!("Layout saved.");
        }
    }
}

/// Convert DrawCommand color `[u8;4]` → gpui `rgb` u32 (ignores alpha).
fn color_to_rgb(c: [u8; 4]) -> u32 {
    ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | (c[2] as u32)
}

/// Convert `[u8;4]` RGBA → `Hsla` for use with the text system.
fn color_to_hsla(c: [u8; 4]) -> Hsla {
    Hsla::from(rgba(
        ((c[0] as u32) << 24) | ((c[1] as u32) << 16) | ((c[2] as u32) << 8) | (c[3] as u32),
    ))
}

impl Render for GraphCanvas {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let zoom = self.zoom;
        let camera = self.camera;
        let snapshot = self.snapshot.clone();
        let selected_node_idx = self.selection.read(cx).selected_node_idx;
        let canvas_bounds_arc = self.canvas_bounds.clone();

        div()
            .id("graph-root")
            .size_full()
            .overflow_hidden()
            .bg(rgb(0x1e1e2e))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                    this.last_mouse = event.position;

                    let (canvas_size, origin) = this.canvas_metrics();
                    // Convert window-space position to canvas-local.
                    let screen_pos = Vec2::new(
                        f32::from(event.position.x) - origin.x,
                        f32::from(event.position.y) - origin.y,
                    );
                    let world_pos = this.viewport(canvas_size).screen_to_world(screen_pos);
                    let half_size = NODE_RADIUS * 1.5;

                    if let Some(idx) = this.snapshot.read().node_at_point_aabb(world_pos, half_size)
                    {
                        // Selection originated from the canvas — don't pan.
                        this.suppress_pan = true;
                        this.selection.update(cx, |sel, cx| {
                            sel.select_by_idx(Some(idx), cx);
                        });
                        this.dragging_node = Some(idx);
                    } else {
                        this.suppress_pan = true;
                        this.selection.update(cx, |sel, cx| sel.clear(cx));
                        this.panning = true;
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                    if this.dragging_node.is_some() {
                        this.snapshot.write().rebuild_spatial_index();
                        this.save_layout();
                        this.dragging_node = None;
                        cx.notify();
                    } else {
                        this.panning = false;
                    }
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                let delta = event.position - this.last_mouse;
                this.last_mouse = event.position;

                if let Some(node_idx) = this.dragging_node {
                    let world_delta = Vec2::new(
                        f32::from(delta.x) / this.zoom,
                        f32::from(delta.y) / this.zoom,
                    );
                    this.snapshot.write().nodes[node_idx].position += world_delta;
                    cx.notify();
                } else if this.panning {
                    this.camera.x -= f32::from(delta.x) / this.zoom;
                    this.camera.y -= f32::from(delta.y) / this.zoom;
                    cx.notify();
                }
            }))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _window, cx| {
                let factor = match event.delta {
                    ScrollDelta::Pixels(delta) => 1.0 + f32::from(delta.y) * 0.002,
                    ScrollDelta::Lines(delta) => 1.0 + delta.y * 0.1,
                };
                let (canvas_size, origin) = this.canvas_metrics();
                // Convert window-space position to canvas-local.
                let mouse_screen = Vec2::new(
                    f32::from(event.position.x) - origin.x,
                    f32::from(event.position.y) - origin.y,
                );
                let vp = this.viewport(canvas_size);
                let world_under_mouse = vp.screen_to_world(mouse_screen);

                this.zoom = (this.zoom * factor).clamp(0.05, 20.0);

                let new_vp = this.viewport(canvas_size);
                let new_screen = new_vp.world_to_screen(world_under_mouse);
                let screen_delta = mouse_screen - new_screen;
                this.camera.x -= screen_delta.x / this.zoom;
                this.camera.y -= screen_delta.y / this.zoom;

                cx.notify();
            }))
            .child(
                canvas(
                    |_bounds, _window, _cx| {},
                    move |bounds, (), window, cx| {
                        // Record bounds so event handlers can convert window → canvas coords.
                        *canvas_bounds_arc.write() = bounds;

                        window.paint_quad(fill(bounds, rgb(0x1e1e2e)));

                        let canvas_size =
                            Vec2::new(f32::from(bounds.size.width), f32::from(bounds.size.height));
                        // Offset added to every canvas-local position to get window coordinates.
                        let ox = f32::from(bounds.origin.x);
                        let oy = f32::from(bounds.origin.y);

                        let viewport = Viewport {
                            center: camera,
                            size: canvas_size,
                            zoom,
                        };

                        let snap = snapshot.read();
                        let commands = generate_draw_commands(&snap, &viewport, selected_node_idx);
                        let lod = viewport.lod_level();
                        drop(snap);

                        // ── Edges (batched paths) ────────────────────────────────────
                        let edge_commands: Vec<&DrawCommand> = commands
                            .iter()
                            .filter(|c| matches!(c, DrawCommand::Line { .. }))
                            .collect();

                        if !edge_commands.is_empty() {
                            let mut builder = PathBuilder::stroke(px(1.0));
                            let mut count = 0;
                            let edge_color = rgb(0x585b70);

                            for cmd in &edge_commands {
                                if let DrawCommand::Line { from, to, .. } = cmd {
                                    builder.move_to(point(px(from.x + ox), px(from.y + oy)));
                                    builder.line_to(point(px(to.x + ox), px(to.y + oy)));
                                    count += 1;

                                    if count >= EDGE_BATCH_SIZE {
                                        if let Ok(path) = builder.build() {
                                            window.paint_path(path, edge_color);
                                        }
                                        builder = PathBuilder::stroke(px(1.0));
                                        count = 0;
                                    }
                                }
                            }
                            if count > 0 {
                                if let Ok(path) = builder.build() {
                                    window.paint_path(path, edge_color);
                                }
                            }
                        }

                        // ── Nodes (squircles) ────────────────────────────────────────
                        let use_dots = lod == LodLevel::Dot;
                        for cmd in &commands {
                            if let DrawCommand::Circle {
                                center,
                                radius,
                                color,
                                selected,
                            } = cmd
                            {
                                let r = if use_dots {
                                    px(3.0)
                                } else {
                                    px(*radius * zoom)
                                };
                                let c = point(px(center.x + ox), px(center.y + oy));
                                let node_bounds =
                                    Bounds::new(point(c.x - r, c.y - r), size(r * 2.0, r * 2.0));
                                let col = color_to_rgb(*color);

                                if use_dots {
                                    window.paint_quad(fill(node_bounds, rgb(col)));
                                } else {
                                    let sq_radii = r * 0.6;

                                    if *selected {
                                        let hr = r * 1.45;
                                        let ring_bounds = Bounds::new(
                                            point(c.x - hr, c.y - hr),
                                            size(hr * 2.0, hr * 2.0),
                                        );
                                        window.paint_quad(
                                            fill(ring_bounds, rgb(0xffffff)).corner_radii(hr * 0.6),
                                        );
                                    }

                                    window.paint_quad(
                                        fill(node_bounds, rgb(col)).corner_radii(sq_radii),
                                    );
                                }
                            }
                        }

                        // ── Node labels (inside squircles) ───────────────────────────
                        let text_system = window.text_system().clone();
                        let sys_font: Font = font(".SystemUIFont");
                        for cmd in &commands {
                            if let DrawCommand::Text {
                                position,
                                content,
                                size,
                                color,
                            } = cmd
                            {
                                if content.is_empty() {
                                    continue;
                                }
                                let font_size = px(*size);
                                let text_color = color_to_hsla(*color);
                                let run = TextRun {
                                    len: content.len(),
                                    font: sys_font.clone(),
                                    color: text_color,
                                    background_color: None,
                                    underline: None,
                                    strikethrough: None,
                                };
                                let shaped = text_system.shape_line(
                                    SharedString::from(content.clone()),
                                    font_size,
                                    &[run],
                                    None,
                                );
                                let line_height = font_size * 1.2;
                                let tx = position.x + ox - f32::from(shaped.width) / 2.0;
                                let ty = position.y + oy - f32::from(line_height) / 2.0;
                                let _ =
                                    shaped.paint(point(px(tx), px(ty)), line_height, window, cx);
                            }
                        }

                        // ── Color legend (bottom-right of canvas pane) ───────────────
                        {
                            let entry_h = 20.0_f32;
                            let swatch = 12.0_f32;
                            let pad = 8.0_f32;
                            let legend_w = 155.0_f32;
                            let legend_h = LEGEND_ENTRIES.len() as f32 * entry_h + pad * 2.0;
                            // Canvas-local bottom-right, then shifted to window coords.
                            let lx = canvas_size.x - legend_w - pad + ox;
                            let ly = canvas_size.y - legend_h - pad + oy;

                            window.paint_quad(fill(
                                Bounds::new(
                                    point(px(lx), px(ly)),
                                    size(px(legend_w), px(legend_h)),
                                ),
                                rgba(0x1e1e2ed8),
                            ));

                            let label_size = px(10.0);
                            let line_h = label_size * 1.3;

                            for (i, (label, color_hex)) in LEGEND_ENTRIES.iter().enumerate() {
                                let row_y = ly + pad + i as f32 * entry_h;
                                let center_y = row_y + entry_h / 2.0;

                                window.paint_quad(
                                    fill(
                                        Bounds::new(
                                            point(px(lx + pad), px(center_y - swatch / 2.0)),
                                            size(px(swatch), px(swatch)),
                                        ),
                                        rgb(*color_hex),
                                    )
                                    .corner_radii(px(3.0)),
                                );

                                let run = TextRun {
                                    len: label.len(),
                                    font: sys_font.clone(),
                                    color: Hsla::from(rgba(0xcdd6f4ff)),
                                    background_color: None,
                                    underline: None,
                                    strikethrough: None,
                                };
                                let shaped = text_system.shape_line(
                                    SharedString::from(*label),
                                    label_size,
                                    &[run],
                                    None,
                                );
                                let text_x = lx + pad + swatch + 6.0;
                                let text_y = center_y - f32::from(line_h) / 2.0;
                                let _ =
                                    shaped.paint(point(px(text_x), px(text_y)), line_h, window, cx);
                            }
                        }
                    },
                )
                .size_full(),
            )
    }
}

// ── Root app view ─────────────────────────────────────────────────────────────

struct AppView {
    graph_canvas: Entity<GraphCanvas>,
    tree_panel: Entity<TreePanel>,
    #[allow(dead_code)]
    selection: Entity<SelectionModel>,
    file_menu_open: bool,
    sidebar_open: bool,
}

impl AppView {
    fn new(snapshot: GraphSnapshot, graph: Arc<KnowledgeGraph>, cx: &mut Context<Self>) -> Self {
        let snapshot_arc = Arc::new(RwLock::new(snapshot));
        let selection = cx.new(|_cx| SelectionModel::new(snapshot_arc.clone()));
        let graph_canvas = cx.new(|cx| {
            GraphCanvas::new(snapshot_arc.clone(), graph, selection.clone(), cx)
        });
        let tree_panel = cx.new(|_cx| TreePanel::new(snapshot_arc, selection.clone()));
        Self {
            graph_canvas,
            tree_panel,
            selection,
            file_menu_open: false,
            sidebar_open: false,
        }
    }

    fn do_save(&self, cx: &Context<Self>) {
        self.graph_canvas.read(cx).save_layout();
    }
}

impl Render for AppView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let file_menu_open = self.file_menu_open;
        let sidebar_open = self.sidebar_open;

        div()
            .id("app-root")
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x1e1e2e))
            // Handle SaveLayout action dispatched from the native menu or Ctrl+S.
            .on_action(cx.listener(|this, _: &SaveLayout, _window, cx| {
                this.do_save(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _window, cx| {
                this.sidebar_open = !this.sidebar_open;
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
                                    this.sidebar_open = !this.sidebar_open;
                                    this.file_menu_open = false;
                                    cx.notify();
                                }),
                            )
                            .child(if sidebar_open { "◀ Nodes" } else { "▶ Nodes" }),
                    ),
            )
            // ── Body: optional sidebar + main content ─────────────────────────
            .child({
                // Horizontal flex container for sidebar + main workspace.
                let mut body = div()
                    .id("body")
                    .flex()
                    .flex_row()
                    .overflow_hidden();
                body.style().flex_grow = Some(1.0);
                body.style().flex_shrink = Some(1.0);
                body.style().flex_basis = Some(relative(0.).into());

                // Sidebar (when open)
                if sidebar_open {
                    body = body.child(self.tree_panel.clone());
                }

                // Main workspace: editor 30% + graph 70% (vertical split)
                let mut workspace = div()
                    .id("workspace")
                    .flex()
                    .flex_col();
                workspace.style().flex_grow = Some(1.0);
                workspace.style().flex_shrink = Some(1.0);
                workspace.style().flex_basis = Some(relative(0.).into());

                // Editor placeholder pane — 3 parts out of 10 (30%).
                let mut editor = div()
                    .id("editor-pane")
                    .flex()
                    .w_full()
                    .bg(rgb(0x1e1e2e))
                    .border_b_1()
                    .border_color(rgb(0x313244))
                    .items_center()
                    .justify_center()
                    .text_color(rgba(0x6c7086ff))
                    .text_sm()
                    .child("Editor pane — coming soon");
                editor.style().flex_grow = Some(3.0);
                editor.style().flex_shrink = Some(1.0);
                editor.style().flex_basis = Some(relative(0.).into());

                // Graph canvas pane — 7 parts out of 10 (70%).
                let mut graph_pane = div()
                    .w_full()
                    .child(self.graph_canvas.clone());
                graph_pane.style().flex_grow = Some(7.0);
                graph_pane.style().flex_shrink = Some(1.0);
                graph_pane.style().flex_basis = Some(relative(0.).into());

                body.child(workspace.child(editor).child(graph_pane))
            })
            // ── File dropdown overlay (rendered on top via deferred) ───────────
            .when(file_menu_open, |root| {
                root.child(deferred(
                    anchored()
                        .position(point(px(0.0), px(MENU_BAR_H)))
                        .anchor(Corner::TopLeft)
                        .child(
                            div()
                                .id("file-dropdown")
                                .w(px(140.0))
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
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                                this.do_save(cx);
                                                this.file_menu_open = false;
                                                cx.notify();
                                            }),
                                        )
                                        .child("Save"),
                                ),
                        ),
                ))
            })
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    let cfg = AppConfig::load_default();
    let data_dir = cfg.storage.db_path;

    let (snapshot, graph) = {
        let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
        rt.block_on(async {
            let graph =
                Arc::new(KnowledgeGraph::new(&data_dir).expect("failed to open knowledge graph"));

            let stats = graph.get_stats().expect("failed to get stats");
            if stats.node_count == 0 {
                let data_file = std::path::Path::new("defaults/data/memory.json");
                if data_file.exists() {
                    let mut ingestion = u_forge_core::DataIngestion::new(&graph);
                    ingestion
                        .import_json_data(data_file)
                        .await
                        .expect("failed to import data");
                    let stats = graph.get_stats().expect("failed to get stats");
                    eprintln!(
                        "Imported {} nodes, {} edges from memory.json",
                        stats.node_count, stats.edge_count
                    );
                } else {
                    eprintln!("Warning: defaults/data/memory.json not found, using empty graph");
                }
            } else {
                eprintln!(
                    "Loaded existing graph: {} nodes, {} edges",
                    stats.node_count, stats.edge_count
                );
            }

            let snapshot = build_snapshot(&graph).expect("failed to build snapshot");
            (snapshot, graph)
        })
    };

    Application::new().run(|cx: &mut App| {
        // Register keybindings.
        cx.bind_keys([
            KeyBinding::new("ctrl-s", SaveLayout, None),
            KeyBinding::new("ctrl-b", ToggleSidebar, None),
        ]);

        // Register native application menu (macOS menu bar; no-op on Linux).
        cx.set_menus(vec![
            Menu {
                name: "File".into(),
                items: vec![MenuItem::action("Save", SaveLayout)],
            },
            Menu {
                name: "View".into(),
                items: vec![MenuItem::action("Toggle Sidebar", ToggleSidebar)],
            },
        ]);

        let bounds = Bounds::centered(None, size(px(1200.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|cx| AppView::new(snapshot, graph, cx)),
        )
        .unwrap();
    });
}
