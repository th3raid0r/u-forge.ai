use std::sync::Arc;

use glam::Vec2;
use gpui::{
    canvas, div, fill, point, prelude::*, px, rgb, size, App, Application, Bounds, Context,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels, Point,
    ScrollDelta, ScrollWheelEvent, Window, WindowBounds, WindowOptions,
};
use u_forge_graph_view::{build_snapshot, GraphSnapshot, LodLevel};
use u_forge_ui_traits::{generate_draw_commands, DrawCommand, Viewport};

/// Edges per batched PathBuilder.
const EDGE_BATCH_SIZE: usize = 500;

struct GraphCanvas {
    snapshot: Arc<GraphSnapshot>,
    /// Camera center in world space
    camera: Vec2,
    zoom: f32,
    dragging: bool,
    last_mouse: Point<Pixels>,
}

impl GraphCanvas {
    fn new(snapshot: GraphSnapshot) -> Self {
        Self {
            snapshot: Arc::new(snapshot),
            camera: Vec2::ZERO,
            zoom: 1.0,
            dragging: false,
            last_mouse: point(px(0.0), px(0.0)),
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

/// Convert DrawCommand color [u8;4] → gpui rgb u32
fn color_to_rgb(c: [u8; 4]) -> u32 {
    ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | (c[2] as u32)
}

impl Render for GraphCanvas {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let zoom = self.zoom;
        let camera = self.camera;
        let snapshot = self.snapshot.clone();

        div()
            .id("graph-root")
            .size_full()
            .bg(rgb(0x1e1e2e))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _window, _cx| {
                    this.dragging = true;
                    this.last_mouse = event.position;
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, _window, _cx| {
                    this.dragging = false;
                }),
            )
            .on_mouse_move(cx.listener(
                |this, event: &MouseMoveEvent, _window, cx| {
                    if this.dragging {
                        let delta = event.position - this.last_mouse;
                        // Pan moves camera in the opposite direction of mouse drag
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
                    // Zoom toward mouse position
                    let mouse_screen = Vec2::new(
                        f32::from(event.position.x),
                        f32::from(event.position.y),
                    );
                    let canvas_size = Vec2::new(1200.0, 800.0); // approximate
                    let vp = this.viewport(canvas_size);
                    let world_under_mouse = vp.screen_to_world(mouse_screen);

                    this.zoom = (this.zoom * factor).clamp(0.05, 20.0);

                    // Adjust camera so world_under_mouse stays at the same screen position
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
                    move |bounds, (), window, _cx| {
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

                        let commands = generate_draw_commands(&snapshot, &viewport);
                        let lod = viewport.lod_level();

                        // Draw edges first (batched paths)
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

                        // Draw circles (nodes)
                        let use_dots = lod == LodLevel::Dot;
                        for cmd in &commands {
                            if let DrawCommand::Circle {
                                center,
                                radius,
                                color,
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
                                    window.paint_quad(
                                        fill(node_bounds, rgb(col)).corner_radii(r),
                                    );
                                }
                            }
                        }

                        // Draw text labels (only at Label/Full LOD)
                        // Note: text rendering via paint_glyph is complex in GPUI.
                        // For now we skip text in the canvas and will add it as
                        // overlay div elements in a future iteration.
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

            // Import the test data
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
