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

/// Catppuccin Mocha palette for node types.
fn type_color(object_type: &str) -> [u8; 4] {
    match object_type {
        "npc" | "character" => [137, 180, 250, 255], // blue
        "location" => [166, 227, 161, 255],          // green
        "faction" => [249, 226, 175, 255],           // yellow
        "quest" => [243, 139, 168, 255],             // red
        "item" | "transportation" => [203, 166, 247, 255], // mauve
        "currency" => [148, 226, 213, 255],          // teal
        _ => [205, 214, 244, 255],                   // text (default)
    }
}

const EDGE_COLOR: [u8; 4] = [88, 91, 112, 200]; // surface2 with alpha
const NODE_RADIUS: f32 = 10.0;
const DOT_RADIUS: f32 = 3.0;
const EDGE_WIDTH: f32 = 1.0;
const LABEL_SIZE: f32 = 12.0;

/// Produce draw commands from a graph snapshot and viewport.
///
/// This is the main rendering pipeline — it handles viewport culling,
/// LOD selection, and command generation. The result is consumed by
/// whichever UI framework implements `GraphRenderer`.
pub fn generate_draw_commands(snapshot: &GraphSnapshot, viewport: &Viewport) -> Vec<DrawCommand> {
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

    // Nodes
    let radius = if lod == LodLevel::Dot { DOT_RADIUS } else { NODE_RADIUS };
    for &idx in &visible_indices {
        let node = &snapshot.nodes[idx];
        let screen_pos = viewport.world_to_screen(node.position);
        let color = type_color(&node.object_type);

        commands.push(DrawCommand::Circle {
            center: screen_pos,
            radius,
            color,
        });

        // Labels at Label and Full LOD
        if lod == LodLevel::Label || lod == LodLevel::Full {
            commands.push(DrawCommand::Text {
                position: screen_pos + Vec2::new(radius + 4.0, -LABEL_SIZE * 0.5),
                content: node.name.clone(),
                size: LABEL_SIZE,
                color: [205, 214, 244, 255],
            });
        }

        // Type subtitle at Full LOD
        if lod == LodLevel::Full {
            if let Some(desc) = &node.description {
                let truncated: String = desc.chars().take(60).collect();
                commands.push(DrawCommand::Text {
                    position: screen_pos + Vec2::new(radius + 4.0, LABEL_SIZE * 0.5 + 2.0),
                    content: truncated,
                    size: LABEL_SIZE * 0.8,
                    color: [166, 173, 200, 200],
                });
            }
        }
    }

    commands
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
