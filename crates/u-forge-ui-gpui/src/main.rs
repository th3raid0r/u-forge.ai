use gpui::{
    canvas, div, fill, point, prelude::*, px, rgb, size, App, Application, Bounds, Context,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels, Point,
    ScrollDelta, ScrollWheelEvent, Window, WindowBounds, WindowOptions,
};

const NODE_COUNT: usize = 5_000;
const EDGE_COUNT: usize = 8_000;
const NODE_RADIUS: f32 = 8.0;
const WORLD_SIZE: f32 = 4000.0;

/// Edges per batched PathBuilder. Balances tessellation cost vs draw call count.
const EDGE_BATCH_SIZE: usize = 500;

/// Zoom threshold below which edges are hidden (too zoomed out to be useful).
const EDGE_ZOOM_THRESHOLD: f32 = 0.15;

/// Zoom threshold below which nodes shrink to 2px dots (no rounded corners).
const DOT_ZOOM_THRESHOLD: f32 = 0.25;

struct GraphCanvas {
    /// (x, y, color) tuples in world space
    nodes: Vec<(f32, f32, u32)>,
    /// (source_idx, target_idx) pairs
    edges: Vec<(usize, usize)>,
    pan: Point<Pixels>,
    zoom: f32,
    dragging: bool,
    last_mouse: Point<Pixels>,
}

impl GraphCanvas {
    fn new() -> Self {
        let mut seed: u64 = 42;
        let mut rng = || -> f32 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((seed >> 33) as f32) / (u32::MAX as f32 / 2.0)
        };

        let palette = [
            0x89b4fa, 0xa6e3a1, 0xf9e2af, 0xf38ba8,
            0xcba6f7, 0x94e2d5, 0xfab387, 0xf5c2e7,
        ];

        let nodes: Vec<(f32, f32, u32)> = (0..NODE_COUNT)
            .map(|i| {
                let x = rng() * WORLD_SIZE - WORLD_SIZE / 2.0;
                let y = rng() * WORLD_SIZE - WORLD_SIZE / 2.0;
                (x, y, palette[i % palette.len()])
            })
            .collect();

        // Generate edges connecting nearby-ish nodes (deterministic)
        let mut edges = Vec::with_capacity(EDGE_COUNT);
        let mut edge_seed: u64 = 123;
        for _ in 0..EDGE_COUNT {
            edge_seed = edge_seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let a = (edge_seed >> 33) as usize % NODE_COUNT;
            edge_seed = edge_seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let b = (edge_seed >> 33) as usize % NODE_COUNT;
            if a != b {
                edges.push((a, b));
            }
        }

        Self {
            nodes,
            edges,
            pan: point(px(0.0), px(0.0)),
            zoom: 1.0,
            dragging: false,
            last_mouse: point(px(0.0), px(0.0)),
        }
    }
}

impl Render for GraphCanvas {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let zoom = self.zoom;
        let pan = self.pan;
        let nodes = self.nodes.clone();
        let edges = self.edges.clone();

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
                        this.pan.x += delta.x;
                        this.pan.y += delta.y;
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
                    let mouse = event.position;
                    let old_zoom = this.zoom;
                    this.zoom = (this.zoom * factor).clamp(0.05, 20.0);
                    let scale = this.zoom / old_zoom;
                    this.pan.x = mouse.x - (mouse.x - this.pan.x) * scale;
                    this.pan.y = mouse.y - (mouse.y - this.pan.y) * scale;
                    cx.notify();
                },
            ))
            .child(
                canvas(
                    |_bounds, _window, _cx| {},
                    move |bounds, (), window, _cx| {
                        window.paint_quad(fill(bounds, rgb(0x1e1e2e)));

                        let center = bounds.center();
                        let r = px(NODE_RADIUS * zoom);
                        let diameter = r * 2.0;

                        let view_min_x = bounds.origin.x - r;
                        let view_max_x = bounds.origin.x + bounds.size.width + r;
                        let view_min_y = bounds.origin.y - r;
                        let view_max_y = bounds.origin.y + bounds.size.height + r;

                        // Pre-compute screen positions for all nodes
                        let screen_pos: Vec<(Pixels, Pixels)> = nodes
                            .iter()
                            .map(|&(wx, wy, _)| {
                                (
                                    px(wx * zoom) + pan.x + center.x,
                                    px(wy * zoom) + pan.y + center.y,
                                )
                            })
                            .collect();

                        // Draw edges — batched into chunked paths for performance.
                        // Skip entirely when zoomed out past threshold (LOD: Dot level).
                        if zoom >= EDGE_ZOOM_THRESHOLD {
                            let edge_color = rgb(0x585b70);
                            let mut builder = PathBuilder::stroke(px(1.0));
                            let mut count_in_batch = 0;

                            for &(src, tgt) in &edges {
                                let (sx, sy) = screen_pos[src];
                                let (tx, ty) = screen_pos[tgt];

                                // Skip if both endpoints are outside viewport
                                let src_visible = sx >= view_min_x && sx <= view_max_x
                                    && sy >= view_min_y && sy <= view_max_y;
                                let tgt_visible = tx >= view_min_x && tx <= view_max_x
                                    && ty >= view_min_y && ty <= view_max_y;
                                if !src_visible && !tgt_visible {
                                    continue;
                                }

                                builder.move_to(point(sx, sy));
                                builder.line_to(point(tx, ty));
                                count_in_batch += 1;

                                if count_in_batch >= EDGE_BATCH_SIZE {
                                    if let Ok(path) = builder.build() {
                                        window.paint_path(path, edge_color);
                                    }
                                    builder = PathBuilder::stroke(px(1.0));
                                    count_in_batch = 0;
                                }
                            }
                            // Flush remaining edges
                            if count_in_batch > 0 {
                                if let Ok(path) = builder.build() {
                                    window.paint_path(path, edge_color);
                                }
                            }
                        }

                        // Draw nodes — at very low zoom, use tiny squares instead of
                        // rounded quads (skips corner radius GPU cost).
                        let use_dots = zoom < DOT_ZOOM_THRESHOLD;
                        let dot_size = px(3.0);
                        let dot_half = px(1.5);

                        for (i, &(_, _, color)) in nodes.iter().enumerate() {
                            let (sx, sy) = screen_pos[i];
                            if sx < view_min_x || sx > view_max_x || sy < view_min_y || sy > view_max_y {
                                continue;
                            }
                            if use_dots {
                                let dot_bounds = Bounds::new(
                                    point(sx - dot_half, sy - dot_half),
                                    size(dot_size, dot_size),
                                );
                                window.paint_quad(fill(dot_bounds, rgb(color)));
                            } else {
                                let node_bounds = Bounds::new(
                                    point(sx - r, sy - r),
                                    size(diameter, diameter),
                                );
                                window.paint_quad(fill(node_bounds, rgb(color)).corner_radii(r));
                            }
                        }
                    },
                )
                .size_full(),
            )
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1200.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| GraphCanvas::new()),
        )
        .unwrap();
    });
}
