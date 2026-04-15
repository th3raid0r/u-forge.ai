use std::collections::HashMap;

use glam::Vec2;

use crate::snapshot::{EdgeView, NodeView};

// ── Layout parameters ────────────────────────────────────────────────────────

/// How many iterations of the force-directed algorithm to run.
const ITERATIONS: usize = 200;

/// Ideal distance between connected nodes.
const IDEAL_LENGTH: f32 = 120.0;

/// Repulsion strength (Coulomb-like constant).
const REPULSION: f32 = 8000.0;

/// Attraction strength (spring constant for edges).
const ATTRACTION: f32 = 0.01;

/// Maximum displacement per iteration (prevents explosions).
const MAX_DISPLACEMENT: f32 = 50.0;

/// Damping factor that increases each iteration (simulated annealing).
const COOLING: f32 = 0.97;

/// Grid cell side length for bucketed repulsion (≈ 2× max repulsion radius).
const CELL_SIZE: f32 = 300.0;

/// Maximum repulsion distance — nodes further apart than this don't repel.
const MAX_REPULSION_DIST: f32 = CELL_SIZE * 0.5;

// ── Public API ───────────────────────────────────────────────────────────────

/// Assign positions to nodes using a force-directed layout with grid-cell
/// bucketed repulsion (O(N) per step, not O(N²)).
///
/// Modifies `nodes[..].position` in-place.
pub fn force_directed_layout(nodes: &mut [NodeView], edges: &[EdgeView]) {
    let n = nodes.len();
    if n == 0 {
        return;
    }

    // Seed initial positions in a rough grid to avoid overlapping starts
    let cols = (n as f32).sqrt().ceil() as usize;
    for (i, node) in nodes.iter_mut().enumerate() {
        let row = i / cols;
        let col = i % cols;
        node.position = Vec2::new(
            col as f32 * IDEAL_LENGTH * 0.8,
            row as f32 * IDEAL_LENGTH * 0.8,
        );
    }

    let mut temperature = 1.0f32;

    for _ in 0..ITERATIONS {
        let mut displacements = vec![Vec2::ZERO; n];

        // ── Repulsion via grid-cell bucketing ────────────────────────────
        // Assign each node to a grid cell, then only check neighboring cells.
        let mut grid: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
        for (i, node) in nodes.iter().enumerate() {
            let cx = (node.position.x / CELL_SIZE).floor() as i32;
            let cy = (node.position.y / CELL_SIZE).floor() as i32;
            grid.entry((cx, cy)).or_default().push(i);
        }

        for (&(cx, cy), cell_nodes) in &grid {
            // Check this cell and all 8 neighbors
            for dx in -1..=1 {
                for dy in -1..=1 {
                    let neighbor_key = (cx + dx, cy + dy);
                    let Some(neighbor_nodes) = grid.get(&neighbor_key) else {
                        continue;
                    };

                    for &i in cell_nodes {
                        for &j in neighbor_nodes {
                            if i >= j {
                                continue; // process each pair once
                            }
                            let delta = nodes[i].position - nodes[j].position;
                            let dist_sq = delta.length_squared();
                            let max_sq = MAX_REPULSION_DIST * MAX_REPULSION_DIST;
                            if dist_sq > max_sq || dist_sq < 0.01 {
                                continue;
                            }
                            let dist = dist_sq.sqrt();
                            let force = REPULSION / dist_sq;
                            let dir = delta / dist;
                            displacements[i] += dir * force;
                            displacements[j] -= dir * force;
                        }
                    }
                }
            }
        }

        // ── Attraction along edges ───────────────────────────────────────
        for edge in edges {
            let delta = nodes[edge.target_idx].position - nodes[edge.source_idx].position;
            let dist = delta.length();
            if dist < 0.01 {
                continue;
            }
            let force = ATTRACTION * (dist - IDEAL_LENGTH);
            let dir = delta / dist;
            displacements[edge.source_idx] += dir * force;
            displacements[edge.target_idx] -= dir * force;
        }

        // ── Apply displacements with temperature clamping ────────────────
        let max_disp = MAX_DISPLACEMENT * temperature;
        for (i, node) in nodes.iter_mut().enumerate() {
            let d = displacements[i];
            let mag = d.length();
            if mag > 0.01 {
                let clamped = if mag > max_disp { d * (max_disp / mag) } else { d };
                node.position += clamped;
            }
        }

        temperature *= COOLING;
    }

    // Center the layout around the origin
    if n > 0 {
        let center: Vec2 = nodes.iter().map(|n| n.position).sum::<Vec2>() / n as f32;
        for node in nodes.iter_mut() {
            node.position -= center;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::NodeView;
    use u_forge_core::ObjectId;

    fn make_node(name: &str) -> NodeView {
        NodeView {
            id: ObjectId::new_v4(),
            name: name.to_string(),
            object_type: "test".to_string(),
            description: None,
            position: Vec2::ZERO,
            tags: vec![],
            properties: serde_json::Value::Object(Default::default()),
        }
    }

    #[test]
    fn layout_separates_disconnected_nodes() {
        let mut nodes = vec![make_node("A"), make_node("B"), make_node("C")];
        let edges = vec![];
        force_directed_layout(&mut nodes, &edges);

        // All nodes should have distinct positions
        for i in 0..nodes.len() {
            for j in (i + 1)..nodes.len() {
                let dist = (nodes[i].position - nodes[j].position).length();
                assert!(dist > 1.0, "nodes {i} and {j} should be separated");
            }
        }
    }

    #[test]
    fn layout_pulls_connected_nodes_closer() {
        let mut connected = vec![make_node("A"), make_node("B"), make_node("C")];
        let edges = vec![
            EdgeView {
                source_idx: 0,
                target_idx: 1,
                edge_type: "knows".to_string(),
                weight: 1.0,
            },
            EdgeView {
                source_idx: 1,
                target_idx: 2,
                edge_type: "knows".to_string(),
                weight: 1.0,
            },
        ];
        force_directed_layout(&mut connected, &edges);

        // A-B should be roughly IDEAL_LENGTH apart
        let ab_dist = (connected[0].position - connected[1].position).length();
        assert!(
            ab_dist < IDEAL_LENGTH * 2.5,
            "connected nodes should be within ~2.5× ideal length, got {ab_dist}"
        );
    }
}
