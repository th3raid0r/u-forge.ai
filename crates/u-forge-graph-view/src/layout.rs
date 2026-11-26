use std::collections::HashMap;

use glam::{DVec2, Vec2};

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

/// Displacement observed during one force-layout iteration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutIterationMetrics {
    pub iteration: usize,
    pub max_displacement: f32,
    pub mean_displacement: f32,
}

/// Measurements from a completed force-layout run.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayoutMetrics {
    pub iterations: Vec<LayoutIterationMetrics>,
}

/// Candidate visits are counted before distance checks; evaluations only
/// when a spring or repulsion is actually computed.
#[derive(Default)]
struct LayoutWork {
    repulsion_candidates: usize,
    repulsion_evaluations: usize,
    spring_candidates: usize,
    spring_evaluations: usize,
}

/// Assign positions to nodes using a force-directed layout with grid-cell
/// bucketed repulsion, which limits checks to neighboring cells.
///
/// Modifies `nodes[..].position` in-place.
pub fn force_directed_layout(nodes: &mut [NodeView], edges: &[EdgeView]) -> LayoutMetrics {
    let n = nodes.len();
    if n == 0 {
        return LayoutMetrics::default();
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

    force_directed_layout_with_fixed(nodes, edges, &vec![false; n])
}

/// Relax pre-seeded node positions while preserving every node marked fixed.
///
/// `fixed` must have one entry per node. Fixed nodes exert forces on movable
/// nodes only. When any node is fixed, final centering is skipped.
pub fn force_directed_layout_with_fixed(
    nodes: &mut [NodeView],
    edges: &[EdgeView],
    fixed: &[bool],
) -> LayoutMetrics {
    let mut work = LayoutWork::default();
    let metrics = relax(nodes, edges, fixed, &mut work);
    tracing::debug!(
        repulsion_candidates = work.repulsion_candidates,
        repulsion_evaluations = work.repulsion_evaluations,
        spring_candidates = work.spring_candidates,
        spring_evaluations = work.spring_evaluations,
        "Force-directed layout work"
    );
    metrics
}

fn relax(
    nodes: &mut [NodeView],
    edges: &[EdgeView],
    fixed: &[bool],
    work: &mut LayoutWork,
) -> LayoutMetrics {
    let n = nodes.len();
    assert_eq!(fixed.len(), n, "fixed mask must match node count");
    assert!(
        nodes.iter().all(|node| node.position.is_finite()),
        "force-directed layout received a non-finite position"
    );
    if n == 0 || fixed.iter().all(|is_fixed| *is_fixed) {
        return LayoutMetrics::default();
    }

    // Partition once: anchors never change cells, and springs with two
    // fixed endpoints cannot affect any displacement.
    let mut movable = Vec::new();
    let mut anchors = Grid::new();
    for (i, node) in nodes.iter().enumerate() {
        if fixed[i] {
            anchors.entry(cell_for(node.position)).or_default().push(i);
        } else {
            movable.push(i);
        }
    }
    let active_springs: Vec<_> = edges
        .iter()
        .map(|edge| (edge.source_idx, edge.target_idx))
        .filter(|&(source, target)| !fixed[source] || !fixed[target])
        .collect();
    let mut displacements = vec![DVec2::ZERO; n];
    let mut temperature = 1.0f32;
    let mut metrics = LayoutMetrics {
        iterations: Vec::with_capacity(ITERATIONS),
    };

    for iteration in 0..ITERATIONS {
        // ── Repulsion via grid-cell bucketing ────────────────────────────
        let mut grid = Grid::new();
        for &i in &movable {
            grid.entry(cell_for(nodes[i].position)).or_default().push(i);
            displacements[i] = DVec2::ZERO;
        }

        // Start only from movable cells, so no anchor-to-anchor pairs are visited.
        for (&cell, cell_nodes) in &grid {
            for dx in -1..=1 {
                for dy in -1..=1 {
                    let neighbor = (cell.0.saturating_add(dx), cell.1.saturating_add(dy));
                    if let Some(anchor_nodes) = anchors.get(&neighbor) {
                        for &i in cell_nodes {
                            for &j in anchor_nodes {
                                if let Some(force) =
                                    repulsion(nodes[i].position, nodes[j].position, work)
                                {
                                    displacements[i] += force;
                                }
                            }
                        }
                    }

                    // Visit only one direction across cells and one triangle
                    // within a cell. A movable pair updates both endpoints.
                    if neighbor < cell {
                        continue;
                    }
                    if let Some(neighbor_nodes) = grid.get(&neighbor) {
                        for (offset, &i) in cell_nodes.iter().enumerate() {
                            let start = if neighbor == cell { offset + 1 } else { 0 };
                            for &j in &neighbor_nodes[start..] {
                                if let Some(force) =
                                    repulsion(nodes[i].position, nodes[j].position, work)
                                {
                                    displacements[i] += force;
                                    displacements[j] -= force;
                                }
                            }
                        }
                    }
                }
            }
        }

        // ── Attraction along edges ───────────────────────────────────────
        for &(source, target) in &active_springs {
            work.spring_candidates += 1;
            let delta = nodes[target].position.as_dvec2() - nodes[source].position.as_dvec2();
            let dist = delta.length();
            if dist < 0.01 {
                continue;
            }
            work.spring_evaluations += 1;
            let force = delta / dist * (f64::from(ATTRACTION) * (dist - f64::from(IDEAL_LENGTH)));
            if !fixed[source] {
                displacements[source] += force;
            }
            if !fixed[target] {
                displacements[target] -= force;
            }
        }

        // ── Apply displacements with temperature clamping ────────────────
        let max_disp = f64::from(MAX_DISPLACEMENT * temperature);
        let mut applied_max = 0.0_f32;
        let mut applied_total = 0.0_f32;
        for &i in &movable {
            let node = &mut nodes[i];
            let d = displacements[i];
            let mag = d.length();
            if mag > 0.01 {
                let clamped = if mag > max_disp {
                    d * (max_disp / mag)
                } else {
                    d
                };
                let next =
                    DVec2::new(f64::from(node.position.x), f64::from(node.position.y)) + clamped;
                node.position = finite_vec2(next);
                let applied = clamped.length() as f32;
                applied_max = applied_max.max(applied);
                applied_total += applied;
            }
        }
        metrics.iterations.push(LayoutIterationMetrics {
            iteration,
            max_displacement: applied_max,
            mean_displacement: applied_total / movable.len() as f32,
        });

        temperature *= COOLING;
    }

    // Center the layout around the origin
    if anchors.is_empty() {
        let center = nodes
            .iter()
            .map(|node| DVec2::new(f64::from(node.position.x), f64::from(node.position.y)))
            .sum::<DVec2>()
            / n as f64;
        for node in nodes.iter_mut() {
            node.position = finite_vec2(
                DVec2::new(f64::from(node.position.x), f64::from(node.position.y)) - center,
            );
        }
    }

    assert!(
        nodes.iter().all(|node| node.position.is_finite()),
        "force-directed layout produced a non-finite position"
    );
    metrics
}

type Grid = HashMap<(i64, i64), Vec<usize>>;

fn cell_for(position: Vec2) -> (i64, i64) {
    let position = position.as_dvec2() / f64::from(CELL_SIZE);
    (position.x.floor() as i64, position.y.floor() as i64)
}

fn repulsion(a: Vec2, b: Vec2, work: &mut LayoutWork) -> Option<DVec2> {
    work.repulsion_candidates += 1;
    let delta = a.as_dvec2() - b.as_dvec2();
    let dist_sq = delta.length_squared();
    let max_sq = f64::from(MAX_REPULSION_DIST).powi(2);
    if dist_sq > max_sq || dist_sq < 0.01 {
        return None;
    }
    work.repulsion_evaluations += 1;
    Some(delta / dist_sq.sqrt() * (f64::from(REPULSION) / dist_sq))
}

fn finite_vec2(value: DVec2) -> Vec2 {
    debug_assert!(value.is_finite());
    Vec2::new(finite_f32(value.x), finite_f32(value.y))
}

fn finite_f32(value: f64) -> f32 {
    value.clamp(f64::from(f32::MIN), f64::from(f32::MAX)) as f32
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
            position: Vec2::ZERO,
            properties: serde_json::Value::Object(Default::default()),
        }
    }

    #[test]
    fn layout_separates_disconnected_nodes() {
        let mut nodes = vec![make_node("A"), make_node("B"), make_node("C")];
        let edges = vec![];
        force_directed_layout(&mut nodes, &edges);

        let center = nodes.iter().map(|node| node.position).sum::<Vec2>() / nodes.len() as f32;
        assert!(center.abs_diff_eq(Vec2::ZERO, 1e-3));

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

    fn edge(source_idx: usize, target_idx: usize) -> EdgeView {
        EdgeView {
            source_idx,
            target_idx,
            edge_type: "next".to_string(),
            weight: 1.0,
        }
    }

    fn nodes_at(positions: &[(f32, f32)]) -> Vec<NodeView> {
        positions
            .iter()
            .map(|&(x, y)| {
                let mut node = make_node("node");
                node.position = Vec2::new(x, y);
                node
            })
            .collect()
    }

    #[test]
    fn dense_anchors_only_visit_candidates_with_a_movable_endpoint() {
        let positions: Vec<_> = (0..100)
            .map(|i| (10.0 + 5.0 * (i % 10) as f32, 10.0 + 5.0 * (i / 10) as f32))
            .chain([(65.0, 35.0)])
            .collect();
        let mut nodes = nodes_at(&positions);
        let mut fixed = [true; 101];
        fixed[100] = false;
        let edges: Vec<_> = (0..99)
            .map(|i| edge(i, i + 1))
            .chain([edge(59, 100)])
            .collect();

        let mut work = LayoutWork::default();
        let metrics = relax(&mut nodes, &edges, &fixed, &mut work);
        assert_eq!(work.repulsion_candidates, 100 * ITERATIONS);
        assert_eq!(work.repulsion_evaluations, 480); // Other candidates are outside the cutoff.
        assert_eq!(work.spring_candidates, ITERATIONS);
        assert_eq!(work.spring_evaluations, ITERATIONS);
        assert!(
            nodes[..100]
                .iter()
                .zip(&positions)
                .all(|(node, &(x, y))| node.position == Vec2::new(x, y))
        );
        assert_eq!(metrics.iterations.len(), ITERATIONS);
        // Position recorded with the original solver, before skipping fixed pairs.
        assert!(
            nodes[100]
                .position
                .abs_diff_eq(Vec2::new(205.00421, 39.86535), 1e-3)
        );
    }

    #[test]
    fn mixed_layout_preserves_movable_positions_and_displacement_metrics() {
        // Movable indices bracket the anchor indices. The movers start in
        // neighboring cells and interact with both each other and the anchors.
        let mut nodes = nodes_at(&[
            (-20.0, 20.0),
            (-30.0, 50.0),
            (45.0, 55.0),
            (70.0, 30.0),
            (900.0, 900.0),
        ]);
        let metrics = force_directed_layout_with_fixed(
            &mut nodes,
            &[edge(0, 1), edge(2, 3), edge(0, 3), edge(1, 2)],
            &[false, true, true, false, false],
        );
        assert_eq!(nodes[1].position, Vec2::new(-30.0, 50.0));
        assert_eq!(nodes[2].position, Vec2::new(45.0, 55.0));
        assert_eq!(nodes[4].position, Vec2::splat(900.0));
        // Baseline from the original solver, including the stationary mover
        // at (900, 900) in the mean displacement denominator.
        assert!(
            nodes[0]
                .position
                .abs_diff_eq(Vec2::new(-5.9385123, -98.00244), 1e-3)
        );
        assert!(
            nodes[3]
                .position
                .abs_diff_eq(Vec2::new(139.03433, -59.31621), 1e-3)
        );
        assert_eq!(metrics.iterations.len(), ITERATIONS);
        for (iteration, max, mean) in [(0, 9.267566, 6.020865), (199, 0.11655947, 0.077706315)] {
            let sample = metrics.iterations[iteration];
            assert_eq!(sample.iteration, iteration);
            assert!((sample.max_displacement - max).abs() < 1e-5);
            assert!((sample.mean_displacement - mean).abs() < 1e-5);
        }
    }

    #[test]
    fn layout_reports_finite_displacement_for_each_iteration() {
        let mut nodes = vec![make_node("A"), make_node("B"), make_node("C")];
        let edges = vec![EdgeView {
            source_idx: 0,
            target_idx: 1,
            edge_type: "knows".to_string(),
            weight: 1.0,
        }];

        let metrics = force_directed_layout(&mut nodes, &edges);

        assert_eq!(metrics.iterations.len(), ITERATIONS);
        assert!(metrics.iterations.iter().all(|sample| {
            sample.max_displacement.is_finite()
                && sample.mean_displacement.is_finite()
                && sample.mean_displacement <= sample.max_displacement
        }));
        assert!(
            metrics.iterations.last().unwrap().max_displacement
                < metrics.iterations.first().unwrap().max_displacement
        );
    }
}
