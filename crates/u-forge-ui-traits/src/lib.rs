// u-forge-ui-traits — framework-agnostic rendering contracts.
//
// Defines the DrawCommand primitive set and Viewport state that both GPUI
// and egui backends consume. No UI framework dependencies — only glam
// and u-forge-graph-view types.

use glam::Vec2;
use u_forge_graph_view::{GraphSnapshot, LodLevel};

/// A positioned, styled rendering primitive that UI frameworks draw.
#[derive(Debug, Clone)]
pub enum DrawCommand {
    Circle {
        center: Vec2,
        radius: f32,
        color: [u8; 4],
        /// Whether this node is the currently selected node.
        selected: bool,
    },
    Line {
        from: Vec2,
        to: Vec2,
        width: f32,
        color: [u8; 4],
    },
    Text {
        position: Vec2,
        content: String,
        size: f32,
        color: [u8; 4],
    },
}

/// Camera/viewport state for culling and LOD decisions.
#[derive(Debug, Clone)]
pub struct Viewport {
    pub center: Vec2,
    pub size: Vec2,
    pub zoom: f32,
}

/// Zoom thresholds for LOD transitions.
const LOD_DOT_THRESHOLD: f32 = 0.25;
const LOD_LABEL_THRESHOLD: f32 = 0.6;

impl Viewport {
    /// World-space bounding rectangle `(min, max)`.
    pub fn world_rect(&self) -> (Vec2, Vec2) {
        let half = self.size / (2.0 * self.zoom);
        (self.center - half, self.center + half)
    }

    /// Current LOD level based on zoom.
    pub fn lod_level(&self) -> LodLevel {
        if self.zoom < LOD_DOT_THRESHOLD {
            LodLevel::Dot
        } else if self.zoom < LOD_LABEL_THRESHOLD {
            LodLevel::Label
        } else {
            LodLevel::Full
        }
    }

    /// Convert a world-space position to screen-space position.
    pub fn world_to_screen(&self, world_pos: Vec2) -> Vec2 {
        (world_pos - self.center) * self.zoom + self.size * 0.5
    }

    /// Convert a screen-space position to world-space position.
    pub fn screen_to_world(&self, screen_pos: Vec2) -> Vec2 {
        (screen_pos - self.size * 0.5) / self.zoom + self.center
    }
}

/// Trait that each UI backend implements.
pub trait GraphRenderer {
    fn draw_commands(&mut self, commands: &[DrawCommand]);
    fn canvas_size(&self) -> Vec2;
}

// ── Node color palette ───────────────────────────────────────────────────────

/// Convert HSL (h in [0,360), s and l in [0,1]) to sRGB bytes.
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r1, g1, b1) = if h < 60.0 {
        (c, x, 0.0)
    } else if h < 120.0 {
        (x, c, 0.0)
    } else if h < 180.0 {
        (0.0, c, x)
    } else if h < 240.0 {
        (0.0, x, c)
    } else if h < 300.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    (
        ((r1 + m) * 255.0).round() as u8,
        ((g1 + m) * 255.0).round() as u8,
        ((b1 + m) * 255.0).round() as u8,
    )
}

/// Return a stable, visually distinct RGBA color for any object type name.
///
/// Hue is derived from the type name via FNV-1a hash scattered with the golden
/// angle (137.5°), giving maximally-separated hues for any set of names.
/// Saturation and lightness are fixed to match the Catppuccin Mocha accent
/// palette (~75 % / 76 %), so every color looks at home on the dark background.
pub fn node_color_for_type(object_type: &str) -> [u8; 4] {
    // FNV-1a hash of the type name — fast and well-distributed.
    let hash = object_type
        .bytes()
        .fold(2_166_136_261_u32, |acc, b| {
            acc.wrapping_mul(16_777_619).wrapping_add(b as u32)
        });
    // Golden-angle scatter: multiply by φ fractional part, wrap to [0, 1).
    let hue = ((hash as f64 * 0.618_033_988_749_895).fract() * 360.0) as f32;
    let (r, g, b) = hsl_to_rgb(hue, 0.75, 0.76);
    [r, g, b, 255]
}

/// Internal alias kept for brevity inside this module.
#[inline(always)]
fn type_color(object_type: &str) -> [u8; 4] {
    node_color_for_type(object_type)
}

const EDGE_COLOR: [u8; 4] = [88, 91, 112, 200]; // surface2 with alpha
/// Node radius in world units. Exported so the GPUI renderer can compute hit-test distances.
pub const NODE_RADIUS: f32 = 10.0;
const DOT_RADIUS: f32 = 3.0;
const EDGE_WIDTH: f32 = 1.0;

/// Produce draw commands from a graph snapshot and viewport.
///
/// - `selected_idx`: index into `snapshot.nodes` of the currently selected node, if any.
///
/// This is the main rendering pipeline — it handles viewport culling,
/// LOD selection, and command generation. The result is consumed by
/// whichever UI framework implements `GraphRenderer`.
pub fn generate_draw_commands(
    snapshot: &GraphSnapshot,
    viewport: &Viewport,
    selected_idx: Option<usize>,
) -> Vec<DrawCommand> {
    let (world_min, world_max) = viewport.world_rect();
    let lod = viewport.lod_level();

    // Determine which nodes are visible using the R-tree spatial index
    let visible_indices = snapshot.nodes_in_viewport(
        world_min - Vec2::splat(NODE_RADIUS),
        world_max + Vec2::splat(NODE_RADIUS),
    );

    // Build a visibility bitset for edge culling
    let mut visible_set = vec![false; snapshot.nodes.len()];
    for &idx in &visible_indices {
        visible_set[idx] = true;
    }

    let mut commands = Vec::new();

    // Edges first (drawn behind nodes)
    if lod != LodLevel::Dot {
        let visible_edge_indices = snapshot.edges_in_viewport(&visible_set);
        for idx in visible_edge_indices {
            let edge = &snapshot.edges[idx];
            let from = viewport.world_to_screen(snapshot.nodes[edge.source_idx].position);
            let to = viewport.world_to_screen(snapshot.nodes[edge.target_idx].position);
            commands.push(DrawCommand::Line {
                from,
                to,
                width: EDGE_WIDTH,
                color: EDGE_COLOR,
            });
        }
    }

    // Nodes — squircle shape is handled by the renderer; here we emit circle primitives.
    let radius = if lod == LodLevel::Dot { DOT_RADIUS } else { NODE_RADIUS };
    // Screen radius used to gate text visibility.
    let screen_radius = NODE_RADIUS * viewport.zoom;

    for &idx in &visible_indices {
        let node = &snapshot.nodes[idx];
        let screen_pos = viewport.world_to_screen(node.position);
        let is_selected = selected_idx == Some(idx);
        let base_color = type_color(&node.object_type);
        let color = if is_selected {
            brighten(base_color, 1.45)
        } else {
            base_color
        };

        commands.push(DrawCommand::Circle {
            center: screen_pos,
            radius,
            color,
            selected: is_selected,
        });

        // Node label — rendered inside the squircle when the node is large enough on screen.
        if lod != LodLevel::Dot && screen_radius > 10.0 {
            // Font size proportional to node screen size, clamped to a readable range.
            let font_size = (screen_radius * 0.75).clamp(7.0, 12.0);
            // Approximate max chars that fit horizontally inside the squircle.
            let max_chars = ((screen_radius * 2.0 * 0.8) / (font_size * 0.55)) as usize;
            let max_chars = max_chars.max(2);
            let display_name = if node.name.chars().count() > max_chars {
                let mut s: String = node.name.chars().take(max_chars.saturating_sub(1)).collect();
                s.push('…');
                s
            } else {
                node.name.clone()
            };
            commands.push(DrawCommand::Text {
                // Center position — the renderer centers the text here.
                position: screen_pos,
                content: display_name,
                size: font_size,
                // Dark text for contrast against the pastel node fills.
                color: [10, 8, 20, 220],
            });
        }
    }

    commands
}

fn brighten(color: [u8; 4], factor: f32) -> [u8; 4] {
    [
        (color[0] as f32 * factor).min(255.0) as u8,
        (color[1] as f32 * factor).min(255.0) as u8,
        (color[2] as f32 * factor).min(255.0) as u8,
        color[3],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_transforms_roundtrip() {
        let vp = Viewport {
            center: Vec2::new(100.0, 200.0),
            size: Vec2::new(800.0, 600.0),
            zoom: 1.5,
        };
        let world = Vec2::new(150.0, 250.0);
        let screen = vp.world_to_screen(world);
        let back = vp.screen_to_world(screen);
        assert!((world - back).length() < 0.01);
    }

    #[test]
    fn lod_levels_match_zoom() {
        let make = |zoom| Viewport {
            center: Vec2::ZERO,
            size: Vec2::new(800.0, 600.0),
            zoom,
        };
        assert_eq!(make(0.1).lod_level(), LodLevel::Dot);
        assert_eq!(make(0.4).lod_level(), LodLevel::Label);
        assert_eq!(make(1.0).lod_level(), LodLevel::Full);
    }
}
