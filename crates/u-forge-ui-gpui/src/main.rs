use gpui::{
    canvas, div, fill, point, prelude::*, px, rgb, size, App, Application, Bounds, Context,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollDelta,
    ScrollWheelEvent, Window, WindowBounds, WindowOptions,
};

const NODE_COUNT: usize = 5_000;
const NODE_RADIUS: f32 = 8.0;
const WORLD_SIZE: f32 = 4000.0;

/// Pre-generated node positions in world space.
struct Node {
    x: f32,
    y: f32,
    color: u32,
}

struct GraphCanvas {
    nodes: Vec<Node>,
    pan: Point<Pixels>,
    zoom: f32,
    dragging: bool,
    last_mouse: Point<Pixels>,
}

impl GraphCanvas {
    fn new() -> Self {
        // Simple deterministic pseudo-random using a linear congruential generator
        let mut seed: u64 = 42;
        let mut rng = || -> f32 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((seed >> 33) as f32) / (u32::MAX as f32 / 2.0)
        };

        let palette = [
            0x89b4fa, // blue
            0xa6e3a1, // green
            0xf9e2af, // yellow
            0xf38ba8, // red
            0xcba6f7, // mauve
            0x94e2d5, // teal
            0xfab387, // peach
            0xf5c2e7, // pink
        ];

        let nodes = (0..NODE_COUNT)
            .map(|i| {
                let x = rng() * WORLD_SIZE - WORLD_SIZE / 2.0;
                let y = rng() * WORLD_SIZE - WORLD_SIZE / 2.0;
                let color = palette[i % palette.len()];
                Node { x, y, color }
            })
            .collect();

        Self {
            nodes,
            pan: point(px(0.0), px(0.0)),
            zoom: 1.0,
            dragging: false,
            last_mouse: point(px(0.0), px(0.0)),
        }
    }

    /// Convert world coordinates to screen coordinates.
    fn world_to_screen(&self, wx: f32, wy: f32, canvas_center: Point<Pixels>) -> Point<Pixels> {
        point(
            px(wx * self.zoom) + self.pan.x + canvas_center.x,
            px(wy * self.zoom) + self.pan.y + canvas_center.y,
        )
    }
}

impl Render for GraphCanvas {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Capture state for the paint closure (which takes ownership)
        let zoom = self.zoom;
        let pan = self.pan;
        let node_data: Vec<(f32, f32, u32)> = self
            .nodes
            .iter()
            .map(|n| (n.x, n.y, n.color))
            .collect();

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
                    // Zoom toward mouse position
                    let mouse = event.position;
                    let old_zoom = this.zoom;
                    this.zoom = (this.zoom * factor).clamp(0.05, 20.0);
                    let scale = this.zoom / old_zoom;
                    // Adjust pan so the world point under the mouse stays fixed
                    this.pan.x = mouse.x - (mouse.x - this.pan.x) * scale;
                    this.pan.y = mouse.y - (mouse.y - this.pan.y) * scale;
                    cx.notify();
                },
            ))
            .child(
                canvas(
                    |_bounds, _window, _cx| {},
                    move |bounds, (), window, _cx| {
                        // Background
                        window.paint_quad(fill(bounds, rgb(0x1e1e2e)));

                        let center = bounds.center();
                        let r = px(NODE_RADIUS * zoom);
                        let diameter = r * 2.0;

                        // Viewport culling bounds (in screen space)
                        let view_min_x = bounds.origin.x - r;
                        let view_max_x = bounds.origin.x + bounds.size.width + r;
                        let view_min_y = bounds.origin.y - r;
                        let view_max_y = bounds.origin.y + bounds.size.height + r;

                        for &(wx, wy, color) in &node_data {
                            let sx = px(wx * zoom) + pan.x + center.x;
                            let sy = px(wy * zoom) + pan.y + center.y;

                            // Skip nodes outside viewport
                            if sx < view_min_x || sx > view_max_x || sy < view_min_y || sy > view_max_y {
                                continue;
                            }

                            let node_bounds = Bounds::new(
                                point(sx - r, sy - r),
                                size(diameter, diameter),
                            );
                            window.paint_quad(fill(node_bounds, rgb(color)).corner_radii(r));
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
