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

actions!([SaveLayout, ToggleSidebar, ToggleRightPanel]);

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

/// Menu bar / status bar height in pixels.
const MENU_BAR_H: f32 = 28.0;

/// Status bar height in pixels.
const STATUS_BAR_H: f32 = 24.0;

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

// ── Node detail panel ─────────────────────────────────────────────────────────

/// Height of the tab bar inside the detail panel.
const DETAIL_TAB_H: f32 = 28.0;

/// Read-only node detail panel rendered in the top 30% of the workspace.
///
/// Observes `SelectionModel` and updates whenever the selection changes.
/// Shows the selected node's data as formatted JSON in a scrollable view.
struct NodeDetailPanel {
    selection: Entity<SelectionModel>,
    snapshot: Arc<RwLock<GraphSnapshot>>,
    /// Keeps the selection subscription alive.
    _selection_sub: Subscription,
}

impl NodeDetailPanel {
    fn new(
        snapshot: Arc<RwLock<GraphSnapshot>>,
        selection: Entity<SelectionModel>,
        cx: &mut Context<Self>,
    ) -> Self {
        let sub = cx.observe(&selection, |_this, _sel, cx| {
            cx.notify();
        });
        Self { selection, snapshot, _selection_sub: sub }
    }
}

/// Target display column width for word-wrapping property values.
const DISPLAY_COLS: usize = 72;

/// Escape only the characters that are illegal inside a JSON string.
/// Whitespace (newlines, tabs) is left for `word_wrap_str` to normalize.
fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Split `s` into word-wrapped chunks.
/// `first_avail` is the character budget for the first chunk;
/// `cont_avail` is the budget for subsequent chunks.
/// All whitespace sequences (including embedded newlines) are treated as
/// word separators — the caller gets clean single-space-separated text.
fn word_wrap_str(s: &str, first_avail: usize, cont_avail: usize) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut avail = first_avail;

    for word in s.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.len() + 1 + word.len() <= avail {
            current.push(' ');
            current.push_str(word);
        } else {
            chunks.push(current);
            current = word.to_string();
            avail = cont_avail;
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    chunks
}

/// Format a string-valued property as one or more display lines with word
/// wrapping. Continuation lines are indented to align with the opening `"`.
fn string_prop_lines(key: &str, value: &str, comma: &str) -> Vec<String> {
    let key_part = format!("    \"{}\": \"", escape_json(key));
    let cont_indent = " ".repeat(key_part.len());
    let first_avail = DISPLAY_COLS.saturating_sub(key_part.len() + 1); // +1 for closing `"`
    let cont_avail = DISPLAY_COLS.saturating_sub(cont_indent.len());

    // Fast path: fits on one line.
    let single = format!("{}{}\"{}",  key_part, escape_json(value), comma);
    if single.len() <= DISPLAY_COLS {
        return vec![single];
    }

    let chunks = word_wrap_str(value, first_avail, cont_avail);
    let n = chunks.len();
    let mut lines: Vec<String> = Vec::new();
    for (i, chunk) in chunks.into_iter().enumerate() {
        let escaped = escape_json(&chunk);
        let is_last = i == n - 1;
        if i == 0 && is_last {
            lines.push(format!("{}{}\"{}", key_part, escaped, comma));
        } else if i == 0 {
            lines.push(format!("{}{}", key_part, escaped));
        } else if is_last {
            lines.push(format!("{}{}\"{}",  cont_indent, escaped, comma));
        } else {
            lines.push(format!("{}{}", cont_indent, escaped));
        }
    }
    lines
}

/// Format a non-string serde_json Value as a compact single-line string.
fn json_value_to_display(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "null".into(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => format!("\"{}\"", escape_json(s)),
        serde_json::Value::Array(arr) => {
            if arr.is_empty() {
                return "[]".into();
            }
            let all_scalar = arr.iter().all(|item| {
                matches!(
                    item,
                    serde_json::Value::String(_)
                        | serde_json::Value::Number(_)
                        | serde_json::Value::Bool(_)
                )
            });
            if all_scalar {
                let items: Vec<String> = arr.iter().map(json_value_to_display).collect();
                format!("[{}]", items.join(", "))
            } else {
                v.to_string()
            }
        }
        serde_json::Value::Object(_) => v.to_string(),
    }
}

/// Render a `NodeView` as display lines resembling pretty-printed JSON.
///
/// `description` and `tags` live in dedicated DB columns (not inside the
/// `properties` JSON blob), so we merge them in at display time.
/// The panel shows: id, name, type, then a unified `properties` block with
/// description first, followed by all schema properties, with tags last if
/// not already present in the properties map.
fn node_json_lines(node: &u_forge_graph_view::NodeView) -> Vec<String> {
    // Collect the merged property entries in display order.
    // Each entry is (key, display_lines_for_value).
    let mut prop_lines: Vec<Vec<String>> = Vec::new();

    // 1. description (from the dedicated column) — shown first if present.
    if let Some(desc) = &node.description {
        prop_lines.push(vec!["__desc_placeholder__".into(), desc.clone()]);
    }

    // 2. All entries from the properties JSON blob.
    //    Skip any key named "description" or "tags" to avoid duplication
    //    (the ingestion pipeline sometimes stores them in both places).
    let props_has_tags = node
        .properties
        .as_object()
        .map(|o| o.contains_key("tags"))
        .unwrap_or(false);

    if let Some(obj) = node.properties.as_object() {
        for (key, val) in obj.iter() {
            if key.eq_ignore_ascii_case("description") {
                continue; // already shown from the dedicated column
            }
            if key.eq_ignore_ascii_case("tags") {
                continue; // handled below
            }
            prop_lines.push(vec!["__prop__".into(), key.clone(), val.to_string()]);
        }
    }

    // 3. tags — from properties if present, else fall back to the dedicated column.
    if props_has_tags {
        if let Some(v) = node.properties.as_object().and_then(|o| o.get("tags")) {
            prop_lines.push(vec!["__tags_json__".into(), v.to_string()]);
        }
    } else if !node.tags.is_empty() {
        let tag_list = node
            .tags
            .iter()
            .map(|t| format!("\"{}\"", escape_json(t)))
            .collect::<Vec<_>>()
            .join(", ");
        prop_lines.push(vec!["__tags_list__".into(), tag_list]);
    }

    // ── Render ────────────────────────────────────────────────────────────────
    let mut lines: Vec<String> = Vec::new();
    lines.push("{".into());
    lines.push(format!("  \"id\": \"{}\",", node.id));
    lines.push(format!("  \"name\": \"{}\",", escape_json(&node.name)));
    lines.push(format!("  \"type\": \"{}\",", escape_json(&node.object_type)));

    if prop_lines.is_empty() {
        lines.push("  \"properties\": {}".into());
    } else {
        lines.push("  \"properties\": {".into());
        let total = prop_lines.len();
        for (i, entry) in prop_lines.iter().enumerate() {
            let comma = if i < total - 1 { "," } else { "" };
            match entry[0].as_str() {
                "__desc_placeholder__" => {
                    lines.extend(string_prop_lines("description", &entry[1], comma));
                }
                "__prop__" => {
                    let key = &entry[1];
                    let val: serde_json::Value =
                        serde_json::from_str(&entry[2]).unwrap_or(serde_json::Value::Null);
                    match &val {
                        serde_json::Value::String(s) => {
                            lines.extend(string_prop_lines(key, s, comma));
                        }
                        _ => {
                            lines.push(format!(
                                "    \"{}\": {}{}",
                                escape_json(key),
                                json_value_to_display(&val),
                                comma
                            ));
                        }
                    }
                }
                "__tags_json__" => {
                    let val: serde_json::Value =
                        serde_json::from_str(&entry[1]).unwrap_or(serde_json::Value::Null);
                    lines.push(format!(
                        "    \"tags\": {}{}",
                        json_value_to_display(&val),
                        comma
                    ));
                }
                "__tags_list__" => {
                    lines.push(format!("    \"tags\": [{}]{}", entry[1], comma));
                }
                _ => {}
            }
        }
        lines.push("  }".into());
    }

    lines.push("}".into());
    lines
}

impl Render for NodeDetailPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected_idx = self.selection.read(cx).selected_node_idx;

        let outer = div()
            .id("node-detail-panel")
            .flex()
            .flex_col()
            .w_full()
            .h_full()
            .min_h_0()
            .overflow_hidden()
            .bg(rgb(0x1e1e2e))
            .border_b_1()
            .border_color(rgb(0x313244));

        if let Some(idx) = selected_idx {
            let snap = self.snapshot.read();
            if idx < snap.nodes.len() {
                let lines = node_json_lines(&snap.nodes[idx]);
                drop(snap);

                // Tab bar — single "Overview" tab for now.
                let tab_bar = div()
                    .id("detail-tab-bar")
                    .flex()
                    .flex_row()
                    .flex_none()
                    .h(px(DETAIL_TAB_H))
                    .bg(rgb(0x181825))
                    .border_b_1()
                    .border_color(rgb(0x313244))
                    .child(
                        div()
                            .id("tab-overview")
                            .flex()
                            .items_center()
                            .px_3()
                            .h_full()
                            .flex_none()
                            .text_xs()
                            .text_color(rgba(0xcdd6f4ff))
                            .border_b_2()
                            .border_color(rgb(0x89b4fa))
                            .child("Overview"),
                    );

                // Scrollable JSON content.
                let mut scroll = div()
                    .id("detail-scroll")
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .min_h_0()
                    .p_3();
                scroll.style().flex_grow = Some(1.0);
                scroll.style().flex_shrink = Some(1.0);
                scroll.style().flex_basis = Some(relative(0.).into());

                for (i, line) in lines.into_iter().enumerate() {
                    scroll = scroll.child(
                        div()
                            .id(("detail-line", i))
                            .flex_none()
                            .text_xs()
                            .font_family(".SystemUIMonospacedFont")
                            .text_color(rgba(0xcdd6f4ff))
                            .child(line),
                    );
                }

                outer.child(tab_bar).child(scroll)
            } else {
                drop(snap);
                outer
                    .items_center()
                    .justify_center()
                    .child(div().text_sm().text_color(rgba(0x6c7086ff)).child("—"))
            }
        } else {
            outer
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(rgba(0x6c7086ff))
                        .child("Select a node to view details"),
                )
        }
    }
}

// ── Root app view ─────────────────────────────────────────────────────────────

struct AppView {
    graph_canvas: Entity<GraphCanvas>,
    tree_panel: Entity<TreePanel>,
    node_detail: Entity<NodeDetailPanel>,
    #[allow(dead_code)]
    selection: Entity<SelectionModel>,
    snapshot: Arc<RwLock<GraphSnapshot>>,
    file_menu_open: bool,
    view_menu_open: bool,
    sidebar_open: bool,
    right_panel_open: bool,
}

impl AppView {
    fn new(snapshot: GraphSnapshot, graph: Arc<KnowledgeGraph>, cx: &mut Context<Self>) -> Self {
        let snapshot_arc = Arc::new(RwLock::new(snapshot));
        let selection = cx.new(|_cx| SelectionModel::new(snapshot_arc.clone()));
        let graph_canvas = cx.new(|cx| {
            GraphCanvas::new(snapshot_arc.clone(), graph, selection.clone(), cx)
        });
        let tree_panel = cx.new(|_cx| TreePanel::new(snapshot_arc.clone(), selection.clone()));
        let node_detail = cx.new(|cx| {
            NodeDetailPanel::new(snapshot_arc.clone(), selection.clone(), cx)
        });
        Self {
            graph_canvas,
            tree_panel,
            node_detail,
            selection,
            snapshot: snapshot_arc,
            file_menu_open: false,
            view_menu_open: false,
            sidebar_open: false,
            right_panel_open: false,
        }
    }

    fn do_save(&self, cx: &Context<Self>) {
        self.graph_canvas.read(cx).save_layout();
    }
}

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
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _window, cx| {
                this.sidebar_open = !this.sidebar_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleRightPanel, _window, cx| {
                this.right_panel_open = !this.right_panel_open;
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
                    .flex_col();
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
                    .child(self.node_detail.clone());
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
                    // ── Center: graph stats ───────────────────────────────────
                    .child({
                        let mut center = div()
                            .id("status-center")
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_center()
                            .text_color(rgba(0xa6adc8ff));
                        center.style().flex_grow = Some(1.0);
                        center.child(format!("{} nodes  ·  {} edges", node_count, edge_count))
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
                                .w(px(180.0))
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
                                        .child("Save                Ctrl+S"),
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
                                            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                                this.sidebar_open = !this.sidebar_open;
                                                this.view_menu_open = false;
                                                cx.notify();
                                            }),
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
                                            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                                this.right_panel_open = !this.right_panel_open;
                                                this.view_menu_open = false;
                                                cx.notify();
                                            }),
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
            KeyBinding::new("ctrl-j", ToggleRightPanel, None),
        ]);

        // Register native application menu (macOS menu bar; no-op on Linux).
        cx.set_menus(vec![
            Menu {
                name: "File".into(),
                items: vec![MenuItem::action("Save", SaveLayout)],
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("Toggle Left Panel", ToggleSidebar),
                    MenuItem::action("Toggle Right Panel", ToggleRightPanel),
                ],
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
