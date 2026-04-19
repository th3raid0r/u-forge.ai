use std::sync::Arc;

use glam::Vec2;
use gpui::{
    canvas, div, fill, font, point, prelude::*, px, rgb, rgba, size, Bounds, Context, Entity,
    Font, Hsla, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels,
    Point, ScrollDelta, ScrollWheelEvent, SharedString, Subscription, TextRun, Window,
};
use parking_lot::RwLock;
use u_forge_core::{KnowledgeGraph, ObjectId};
use u_forge_graph_view::{GraphSnapshot, LodLevel};
use u_forge_ui_traits::{generate_draw_commands, node_color_for_type, Viewport, NODE_RADIUS};

use crate::selection_model::SelectionModel;

// ── Graph canvas ─────────────────────────────────────────────────────────────

/// Edges per batched PathBuilder.
const EDGE_BATCH_SIZE: usize = 500;

pub(crate) struct GraphCanvas {
    pub(crate) snapshot: Arc<RwLock<GraphSnapshot>>,
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
    pub(crate) fn new(
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
    pub(crate) fn save_layout(&self) {
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

/// Convert draw-command color `[u8;4]` → gpui `rgb` u32 (ignores alpha).
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

                    if let Some(idx) =
                        this.snapshot.read().node_at_point_aabb(world_pos, half_size)
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

                        let canvas_size = Vec2::new(
                            f32::from(bounds.size.width),
                            f32::from(bounds.size.height),
                        );
                        // Offset added to every canvas-local position to get window coordinates.
                        let ox = f32::from(bounds.origin.x);
                        let oy = f32::from(bounds.origin.y);

                        let viewport = Viewport {
                            center: camera,
                            size: canvas_size,
                            zoom,
                        };

                        let snap = snapshot.read();
                        let commands =
                            generate_draw_commands(&snap, &viewport, selected_node_idx);
                        let lod = viewport.lod_level();
                        // Clone the precomputed legend so we can drop the read lock.
                        // `legend_types` is built once in `build_snapshot()` and only
                        // changes when the snapshot is rebuilt — no per-frame scan.
                        let legend_types = snap.legend_types.clone();
                        drop(snap);

                        // ── Edges (batched paths) ────────────────────────────────────
                        if !commands.edges.is_empty() {
                            let mut builder = PathBuilder::stroke(px(1.0));
                            let mut count = 0;
                            let edge_color = rgb(0x585b70);

                            for edge in &commands.edges {
                                builder.move_to(point(
                                    px(edge.from.x + ox),
                                    px(edge.from.y + oy),
                                ));
                                builder.line_to(point(
                                    px(edge.to.x + ox),
                                    px(edge.to.y + oy),
                                ));
                                count += 1;

                                if count >= EDGE_BATCH_SIZE {
                                    if let Ok(path) = builder.build() {
                                        window.paint_path(path, edge_color);
                                    }
                                    builder = PathBuilder::stroke(px(1.0));
                                    count = 0;
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
                        for node in &commands.nodes {
                            let r = if use_dots {
                                px(3.0)
                            } else {
                                px(node.radius * zoom)
                            };
                            let c = point(px(node.center.x + ox), px(node.center.y + oy));
                            let node_bounds =
                                Bounds::new(point(c.x - r, c.y - r), size(r * 2.0, r * 2.0));
                            let col = color_to_rgb(node.color);

                            if use_dots {
                                window.paint_quad(fill(node_bounds, rgb(col)));
                            } else {
                                let sq_radii = r * 0.6;

                                if node.selected {
                                    let hr = r * 1.45;
                                    let ring_bounds = Bounds::new(
                                        point(c.x - hr, c.y - hr),
                                        size(hr * 2.0, hr * 2.0),
                                    );
                                    window.paint_quad(
                                        fill(ring_bounds, rgb(0xffffff))
                                            .corner_radii(hr * 0.6),
                                    );
                                }

                                window.paint_quad(
                                    fill(node_bounds, rgb(col)).corner_radii(sq_radii),
                                );
                            }
                        }

                        // ── Node labels (inside squircles) ───────────────────────────
                        let text_system = window.text_system().clone();
                        let sys_font: Font = font(".SystemUIFont");
                        for label in &commands.labels {
                            if label.content.is_empty() {
                                continue;
                            }
                            let font_size = px(label.size);
                            let text_color = color_to_hsla(label.color);
                            let run = TextRun {
                                len: label.content.len(),
                                font: sys_font.clone(),
                                color: text_color,
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            };
                            let shaped = text_system.shape_line(
                                SharedString::from(label.content.clone()),
                                font_size,
                                &[run],
                                None,
                            );
                            let line_height = font_size * 1.2;
                            let tx = label.position.x + ox - f32::from(shaped.width) / 2.0;
                            let ty = label.position.y + oy - f32::from(line_height) / 2.0;
                            let _ = shaped.paint(
                                point(px(tx), px(ty)),
                                line_height,
                                window,
                                cx,
                            );
                        }

                        // ── Color legend (bottom-right of canvas pane) ───────────────
                        if !legend_types.is_empty() {
                            let entry_h = 20.0_f32;
                            let swatch = 12.0_f32;
                            let pad = 8.0_f32;
                            let legend_w = 160.0_f32;
                            let legend_h =
                                legend_types.len() as f32 * entry_h + pad * 2.0;
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

                            for (i, type_name) in legend_types.iter().enumerate() {
                                let row_y = ly + pad + i as f32 * entry_h;
                                let center_y = row_y + entry_h / 2.0;

                                let [r, g, b, _] = node_color_for_type(type_name);
                                let color_hex = ((r as u32) << 16)
                                    | ((g as u32) << 8)
                                    | (b as u32);

                                window.paint_quad(
                                    fill(
                                        Bounds::new(
                                            point(
                                                px(lx + pad),
                                                px(center_y - swatch / 2.0),
                                            ),
                                            size(px(swatch), px(swatch)),
                                        ),
                                        rgb(color_hex),
                                    )
                                    .corner_radii(px(3.0)),
                                );

                                let run = TextRun {
                                    len: type_name.len(),
                                    font: sys_font.clone(),
                                    color: Hsla::from(rgba(0xcdd6f4ff)),
                                    background_color: None,
                                    underline: None,
                                    strikethrough: None,
                                };
                                let shaped = text_system.shape_line(
                                    SharedString::from(type_name.clone()),
                                    label_size,
                                    &[run],
                                    None,
                                );
                                let text_x = lx + pad + swatch + 6.0;
                                let text_y = center_y - f32::from(line_h) / 2.0;
                                let _ = shaped.paint(
                                    point(px(text_x), px(text_y)),
                                    line_h,
                                    window,
                                    cx,
                                );
                            }
                        }
                    },
                )
                .size_full(),
            )
    }
}
