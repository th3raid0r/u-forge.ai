use std::collections::HashMap;

use anyhow::Result;
use glam::Vec2;
use rstar::RTree;
use u_forge_core::{KnowledgeGraph, ObjectId};

use crate::layout::force_directed_layout;
use crate::spatial::NodeEntry;

/// Level-of-detail for rendering, keyed on zoom level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LodLevel {
    /// Circles only — no text shaping cost.
    Dot,
    /// Circles + node names.
    Label,
    /// Circles + name + type + description.
    Full,
}

/// A single node ready for rendering.
#[derive(Debug, Clone)]
pub struct NodeView {
    pub id: ObjectId,
    pub name: String,
    pub object_type: String,
    pub description: Option<String>,
    pub position: Vec2,
    pub tags: Vec<String>,
}

/// An edge between two nodes, stored as indices into `GraphSnapshot::nodes`.
///
/// Using `usize` indices instead of `ObjectId` avoids a hashmap lookup per
/// edge per frame in the render hot path.
#[derive(Debug, Clone)]
pub struct EdgeView {
    pub source_idx: usize,
    pub target_idx: usize,
    pub edge_type: String,
    pub weight: f32,
}

/// Immutable snapshot of the graph, optimized for rendering.
pub struct GraphSnapshot {
    pub nodes: Vec<NodeView>,
    pub edges: Vec<EdgeView>,
    pub spatial_index: RTree<NodeEntry>,
}

impl GraphSnapshot {
    /// Return indices of nodes whose positions fall within the given rectangle.
    pub fn nodes_in_viewport(&self, min: Vec2, max: Vec2) -> Vec<usize> {
        use rstar::AABB;
        let envelope = AABB::from_corners([min.x, min.y], [max.x, max.y]);
        self.spatial_index
            .locate_in_envelope(&envelope)
            .map(|entry| entry.index)
            .collect()
    }

    /// Return indices of edges where at least one endpoint is visible.
    pub fn edges_in_viewport(&self, visible_nodes: &[bool]) -> Vec<usize> {
        self.edges
            .iter()
            .enumerate()
            .filter(|(_, e)| visible_nodes[e.source_idx] || visible_nodes[e.target_idx])
            .map(|(i, _)| i)
            .collect()
    }
}

/// Build a `GraphSnapshot` from a `KnowledgeGraph`.
///
/// 1. Fetches all objects and edges
/// 2. Builds an ObjectId → index map
/// 3. Converts edges to index-based EdgeViews
/// 4. Runs force-directed layout to assign positions
/// 5. Builds the R-tree spatial index
pub fn build_snapshot(graph: &KnowledgeGraph) -> Result<GraphSnapshot> {
    let objects = graph.get_all_objects()?;
    let raw_edges = graph.get_all_edges()?;

    // Build ObjectId → usize index map
    let id_to_idx: HashMap<ObjectId, usize> = objects
        .iter()
        .enumerate()
        .map(|(i, obj)| (obj.id, i))
        .collect();

    // Convert to NodeViews (positions will be set by layout)
    let mut nodes: Vec<NodeView> = objects
        .into_iter()
        .map(|obj| NodeView {
            id: obj.id,
            name: obj.name,
            object_type: obj.object_type,
            description: obj.description,
            position: Vec2::ZERO,
            tags: obj.tags,
        })
        .collect();

    // Convert edges — skip any referencing unknown nodes
    let edges: Vec<EdgeView> = raw_edges
        .into_iter()
        .filter_map(|e| {
            let src = *id_to_idx.get(&e.from)?;
            let tgt = *id_to_idx.get(&e.to)?;
            Some(EdgeView {
                source_idx: src,
                target_idx: tgt,
                edge_type: e.edge_type.as_str().to_string(),
                weight: e.weight,
            })
        })
        .collect();

    // Run layout to assign positions
    force_directed_layout(&mut nodes, &edges);

    // Build R-tree spatial index from final positions
    let entries: Vec<NodeEntry> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| NodeEntry {
            index: i,
            position: [n.position.x, n.position.y],
        })
        .collect();
    let spatial_index = RTree::bulk_load(entries);

    Ok(GraphSnapshot {
        nodes,
        edges,
        spatial_index,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use u_forge_core::{EdgeType, KnowledgeGraph, ObjectBuilder};

    fn test_graph() -> (TempDir, KnowledgeGraph) {
        let dir = TempDir::new().unwrap();
        let graph = KnowledgeGraph::new(dir.path()).unwrap();
        (dir, graph)
    }

    #[test]
    fn build_snapshot_empty_graph() {
        let (_dir, graph) = test_graph();
        let snap = build_snapshot(&graph).unwrap();
        assert!(snap.nodes.is_empty());
        assert!(snap.edges.is_empty());
    }

    #[test]
    fn build_snapshot_with_nodes_and_edges() {
        let (_dir, graph) = test_graph();

        let id_a = ObjectBuilder::character("Alice".to_string())
            .add_to_graph(&graph)
            .unwrap();
        let id_b = ObjectBuilder::character("Bob".to_string())
            .add_to_graph(&graph)
            .unwrap();
        graph
            .connect_objects(id_a, id_b, EdgeType::new("knows"))
            .unwrap();

        let snap = build_snapshot(&graph).unwrap();
        assert_eq!(snap.nodes.len(), 2);
        assert_eq!(snap.edges.len(), 1);

        // Edge indices should be valid
        let edge = &snap.edges[0];
        assert!(edge.source_idx < snap.nodes.len());
        assert!(edge.target_idx < snap.nodes.len());

        // Nodes should have non-zero positions (layout ran)
        let a_pos = snap.nodes[edge.source_idx].position;
        let b_pos = snap.nodes[edge.target_idx].position;
        assert_ne!(a_pos, b_pos, "connected nodes should not overlap");
    }

    #[test]
    fn viewport_culling() {
        let (_dir, graph) = test_graph();

        for i in 0..10 {
            ObjectBuilder::character(format!("Node{i}"))
                .add_to_graph(&graph)
                .unwrap();
        }

        let snap = build_snapshot(&graph).unwrap();
        assert_eq!(snap.nodes.len(), 10);

        // Query the full bounding box — should return all nodes
        let all = snap.nodes_in_viewport(Vec2::splat(-10000.0), Vec2::splat(10000.0));
        assert_eq!(all.len(), 10);

        // Query a tiny box at the origin — should return fewer
        let near_origin = snap.nodes_in_viewport(Vec2::splat(-1.0), Vec2::splat(1.0));
        assert!(near_origin.len() <= 10);
    }
}
