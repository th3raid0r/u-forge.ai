use std::sync::Arc;

use glam::Vec2;
use gpui::{
    canvas, div, fill, font, point, prelude::*, px, rgb, rgba, size, App, Application, Bounds,
    Context, Font, Hsla, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder,
    Pixels, Point, ScrollDelta, ScrollWheelEvent, SharedString, TextRun, Window, WindowBounds,
    WindowOptions,
};
use u_forge_graph_view::{build_snapshot, GraphSnapshot, LodLevel};
use u_forge_ui_traits::{generate_draw_commands, DrawCommand, Viewport, NODE_RADIUS};

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

struct GraphCanvas {
    snapshot: Arc<GraphSnapshot>,
    /// Camera center in world space.
    camera: Vec2,
    zoom: f32,
    dragging: bool,
    last_mouse: Point<Pixels>,
    /// Screen position where the last mouse-down occurred (for click vs. drag detection).
    mouse_down_pos: Point<Pixels>,
    /// Index into `snapshot.nodes` of the currently selected node.
    selected_node_idx: Option<usize>,
}

impl GraphCanvas {
    fn new(snapshot: GraphSnapshot) -> Self {
        Self {
            snapshot: Arc::new(snapshot),
            camera: Vec2::ZERO,
            zoom: 1.0,
            dragging: false,
            last_mouse: point(px(0.0), px(0.0)),
            mouse_down_pos: point(px(0.0), px(0.0)),
            selected_node_idx: None,
        }
    }

    fn viewport(&self, canvas_size: Vec2) -> Viewport {
        Viewport {
            center: self.camera,
            size: canvas_size,
            zoom: self.zoom,
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
        let selected_node_idx = self.selected_node_idx;

        div()
            .id("graph-root")
            .size_full()
            .bg(rgb(0x1e1e2e))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _window, _cx| {
                    this.dragging = true;
                    this.last_mouse = event.position;
                    this.mouse_down_pos = event.position;
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, window, cx| {
                    this.dragging = false;

                    // Distinguish a click (≤5 px movement) from a drag.
                    let dx =
                        (f32::from(event.position.x) - f32::from(this.mouse_down_pos.x)).abs();
                    let dy =
                        (f32::from(event.position.y) - f32::from(this.mouse_down_pos.y)).abs();
                    if dx < 5.0 && dy < 5.0 {
                        let vp = window.viewport_size();
                        let canvas_size =
                            Vec2::new(f32::from(vp.width), f32::from(vp.height));
                        let screen_pos = Vec2::new(
                            f32::from(event.position.x),
                            f32::from(event.position.y),
                        );
                        let world_pos = this.viewport(canvas_size).screen_to_world(screen_pos);
                        // Hit radius scales inversely with zoom so that the click target
                        // always matches the visible node size.
                        let max_dist = NODE_RADIUS * 2.0 / this.zoom;
                        if let Some(idx) = this.snapshot.node_at_position(world_pos, max_dist) {
                            this.selected_node_idx =
                                if this.selected_node_idx == Some(idx) {
                                    None // clicking same node deselects
                                } else {
                                    Some(idx)
                                };
                        } else {
                            this.selected_node_idx = None;
                        }
                        cx.notify();
                    }
                }),
            )
            .on_mouse_move(cx.listener(
                |this, event: &MouseMoveEvent, _window, cx| {
                    if this.dragging {
                        let delta = event.position - this.last_mouse;
                        this.camera.x -= f32::from(delta.x) / this.zoom;
                        this.camera.y -= f32::from(delta.y) / this.zoom;
                        this.last_mouse = event.position;
                        cx.notify();
                    }
                },
            ))
            .on_scroll_wheel(cx.listener(
                |this, event: &ScrollWheelEvent, _window, cx| {
                    let factor = match event.delta {
                        ScrollDelta::Pixels(delta) => 1.0 + f32::from(delta.y) * 0.002,
                        ScrollDelta::Lines(delta) => 1.0 + delta.y * 0.1,
                    };
                    let mouse_screen = Vec2::new(
                        f32::from(event.position.x),
                        f32::from(event.position.y),
                    );
                    let canvas_size = Vec2::new(1200.0, 800.0);
                    let vp = this.viewport(canvas_size);
                    let world_under_mouse = vp.screen_to_world(mouse_screen);

                    this.zoom = (this.zoom * factor).clamp(0.05, 20.0);

                    let new_vp = this.viewport(canvas_size);
                    let new_screen = new_vp.world_to_screen(world_under_mouse);
                    let screen_delta = mouse_screen - new_screen;
                    this.camera.x -= screen_delta.x / this.zoom;
                    this.camera.y -= screen_delta.y / this.zoom;

                    cx.notify();
                },
            ))
            .child(
                canvas(
                    |_bounds, _window, _cx| {},
                    move |bounds, (), window, cx| {
                        window.paint_quad(fill(bounds, rgb(0x1e1e2e)));

                        let canvas_size = Vec2::new(
                            f32::from(bounds.size.width),
                            f32::from(bounds.size.height),
                        );
                        let viewport = Viewport {
                            center: camera,
                            size: canvas_size,
                            zoom,
                        };

                        let commands =
                            generate_draw_commands(&snapshot, &viewport, selected_node_idx);
                        let lod = viewport.lod_level();

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
                                    builder.move_to(point(px(from.x), px(from.y)));
                                    builder.line_to(point(px(to.x), px(to.y)));
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
                                let r = if use_dots { px(3.0) } else { px(*radius * zoom) };
                                let c = point(px(center.x), px(center.y));
                                let node_bounds = Bounds::new(
                                    point(c.x - r, c.y - r),
                                    size(r * 2.0, r * 2.0),
                                );
                                let col = color_to_rgb(*color);

                                if use_dots {
                                    window.paint_quad(fill(node_bounds, rgb(col)));
                                } else {
                                    // Squircle: corner_radius ≈ 60 % of the radius gives
                                    // the characteristic rounded-square look.
                                    let sq_radii = r * 0.6;

                                    // Selection ring — a white squircle drawn behind the node.
                                    if *selected {
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
                        }

                        // ── Node labels (inside squircles) ───────────────────────────
                        // Text is centered on the node's screen position.
                        // Clone the Arc so we can call shape_line without borrowing `window`.
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
                                // Center the text on `position`.
                                let tx = position.x - f32::from(shaped.width) / 2.0;
                                let ty = position.y - f32::from(line_height) / 2.0;
                                let _ = shaped.paint(
                                    point(px(tx), px(ty)),
                                    line_height,
                                    window,
                                    cx,
                                );
                            }
                        }

                        // ── Color legend (bottom-right corner) ──────────────────────
                        {
                            let entry_h = 20.0_f32;
                            let swatch = 12.0_f32;
                            let pad = 8.0_f32;
                            let legend_w = 155.0_f32;
                            let legend_h =
                                LEGEND_ENTRIES.len() as f32 * entry_h + pad * 2.0;
                            let lx = canvas_size.x - legend_w - pad;
                            let ly = canvas_size.y - legend_h - pad;

                            // Semi-transparent background
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

                                // Color swatch (small squircle)
                                window.paint_quad(
                                    fill(
                                        Bounds::new(
                                            point(
                                                px(lx + pad),
                                                px(center_y - swatch / 2.0),
                                            ),
                                            size(px(swatch), px(swatch)),
                                        ),
                                        rgb(*color_hex),
                                    )
                                    .corner_radii(px(3.0)),
                                );

                                // Label
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

fn main() {
    // Load graph data before starting GPUI (blocking)
    let snapshot = {
        let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
        rt.block_on(async {
            let tmp = tempfile::TempDir::new().expect("failed to create temp dir");
            let graph =
                u_forge_core::KnowledgeGraph::new(tmp.path()).expect("failed to create graph");

            let data_file = std::path::Path::new("defaults/data/memory.json");
            if data_file.exists() {
                let mut ingestion = u_forge_core::DataIngestion::new(&graph);
                ingestion
                    .import_json_data(data_file)
                    .await
                    .expect("failed to import data");
                let stats = graph.get_stats().expect("failed to get stats");
                eprintln!(
                    "Loaded {} nodes, {} edges from memory.json",
                    stats.node_count, stats.edge_count
                );
            } else {
                eprintln!("Warning: defaults/data/memory.json not found, using empty graph");
            }

            build_snapshot(&graph).expect("failed to build snapshot")
        })
    };

    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1200.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| GraphCanvas::new(snapshot)),
        )
        .unwrap();
    });
}
