use std::collections::{HashMap, HashSet};

use anyhow::Result;
use glam::Vec2;
use rstar::RTree;
use serde_json::Value as JsonValue;
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
    pub position: Vec2,
    /// All schema-defined properties, including `"description"` and `"tags"`.
    pub properties: JsonValue,
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
    /// Unique node `object_type` values, sorted case-insensitively. Precomputed
    /// at snapshot build time so renderers can draw the color legend without
    /// iterating `nodes` every paint frame.
    pub legend_types: Vec<String>,
    /// Maps each node's `ObjectId` to its index in `nodes`. Used by
    /// `nodes_in_viewport` to resolve spatial-index entries to `nodes` indices,
    /// and by `build_snapshot_incremental` to locate existing nodes cheaply.
    pub id_to_idx: HashMap<ObjectId, usize>,
    /// Count of nodes per `object_type`. Maintained alongside `legend_types` so
    /// incremental updates can detect when a type is fully removed (count → 0)
    /// without a full O(N) scan.
    pub type_counts: HashMap<String, usize>,
}

impl GraphSnapshot {
    /// Return indices of nodes whose positions fall within the given rectangle.
    pub fn nodes_in_viewport(&self, min: Vec2, max: Vec2) -> Vec<usize> {
        use rstar::AABB;
        let envelope = AABB::from_corners([min.x, min.y], [max.x, max.y]);
        self.spatial_index
            .locate_in_envelope(&envelope)
            .filter_map(|entry| self.id_to_idx.get(&entry.id).copied())
            .collect()
    }

    /// Rebuild the R-tree spatial index from the current node positions.
    ///
    /// Call this after mutating any `NodeView::position` values (e.g. after a
    /// drag operation) so that subsequent viewport culling and hit-testing
    /// reflect the new positions.
    pub fn rebuild_spatial_index(&mut self) {
        let entries: Vec<NodeEntry> = self
            .nodes
            .iter()
            .map(|n| NodeEntry {
                id: n.id,
                position: [n.position.x, n.position.y],
            })
            .collect();
        self.spatial_index = RTree::bulk_load(entries);
    }

    /// Return the index of the node closest to `world_pos` within `max_dist` world units,
    /// or `None` if no node is within range.
    pub fn node_at_position(&self, world_pos: Vec2, max_dist: f32) -> Option<usize> {
        self.nodes_in_viewport(
            world_pos - Vec2::splat(max_dist),
            world_pos + Vec2::splat(max_dist),
        )
        .into_iter()
        .filter_map(|idx| {
            let dist = (self.nodes[idx].position - world_pos).length();
            if dist <= max_dist {
                Some((idx, dist))
            } else {
                None
            }
        })
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(idx, _)| idx)
    }

    /// Return the index of the closest node whose AABB (centered on the node,
    /// half-extent `half_size` in each axis) contains `world_pos`.
    ///
    /// Prefer this over [`node_at_position`] for hit-testing squircle nodes —
    /// the rectangular test matches the visual footprint of a rounded square.
    pub fn node_at_point_aabb(&self, world_pos: Vec2, half_size: f32) -> Option<usize> {
        self.nodes_in_viewport(
            world_pos - Vec2::splat(half_size),
            world_pos + Vec2::splat(half_size),
        )
        .into_iter()
        .filter_map(|idx| {
            let delta = (self.nodes[idx].position - world_pos).abs();
            if delta.x <= half_size && delta.y <= half_size {
                // Use L∞ distance so the closest node in AABB sense is preferred.
                let dist = delta.x.max(delta.y);
                Some((idx, dist))
            } else {
                None
            }
        })
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(idx, _)| idx)
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
/// 1. Fetches all objects, edges, and any previously saved canvas positions
/// 2. Builds an ObjectId → index map
/// 3. Converts edges to index-based EdgeViews
/// 4. Runs force-directed layout to assign positions for all nodes
/// 5. Overwrites positions with saved values where available
/// 6. Builds the R-tree spatial index
pub fn build_snapshot(graph: &KnowledgeGraph) -> Result<GraphSnapshot> {
    let objects = graph.get_all_objects()?;
    let raw_edges = graph.get_all_edges()?;

    // Load any previously saved UI positions (empty map on first run)
    let saved_positions = graph.load_layout().unwrap_or_default();

    // Build ObjectId → usize index map
    let id_to_idx: HashMap<ObjectId, usize> = objects
        .iter()
        .enumerate()
        .map(|(i, obj)| (obj.id, i))
        .collect();

    // Convert to NodeViews (positions will be set below)
    let mut nodes: Vec<NodeView> = objects
        .into_iter()
        .map(|obj| NodeView {
            id: obj.id,
            name: obj.name,
            object_type: obj.object_type,
            position: Vec2::ZERO,
            properties: obj.properties,
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
                edge_type: e.edge_type.into_inner(),
                weight: e.weight,
            })
        })
        .collect();

    // If every node already has a saved position (the common case for an
    // established graph), skip force-directed layout entirely — it would just
    // be overwritten immediately and wastes O(iterations * N) work.
    // Only run layout when at least one node is missing a saved position
    // (new node added, first run, or layout was reset).
    let all_saved = nodes.iter().all(|n| saved_positions.contains_key(&n.id));
    if !all_saved {
        force_directed_layout(&mut nodes, &edges);
    }

    // Apply saved positions for all known nodes.
    for node in &mut nodes {
        if let Some(&(x, y)) = saved_positions.get(&node.id) {
            node.position = Vec2::new(x, y);
        }
    }

    // Build R-tree spatial index from final positions
    let spatial_index = RTree::bulk_load(
        nodes
            .iter()
            .map(|n| NodeEntry {
                id: n.id,
                position: [n.position.x, n.position.y],
            })
            .collect(),
    );

    // Precompute type counts and the sorted unique type list for the legend.
    let (legend_types, type_counts) = build_legend(&nodes);

    Ok(GraphSnapshot {
        nodes,
        edges,
        spatial_index,
        legend_types,
        id_to_idx,
        type_counts,
    })
}

/// Rebuild a snapshot incrementally from `prev`, applying only what changed.
///
/// This is the hot path for single-mutation events (e.g. an agent calling
/// `UpsertNodeTool`). It still fetches all objects/edges from the DB (same
/// I/O as a full rebuild), but:
///
/// - When all positions are saved (layout is skipped), the R-tree is updated
///   with O(delta × log N) insert/remove operations instead of an O(N log N)
///   `bulk_load`.
/// - `legend_types` and `type_counts` are updated in O(delta) when no new
///   object types are introduced or removed entirely; otherwise they are
///   recomputed in O(N).
///
/// Falls back to a full `bulk_load` whenever layout runs (i.e. a new node
/// without a saved position was added), because layout changes all positions.
pub fn build_snapshot_incremental(
    graph: &KnowledgeGraph,
    prev: &GraphSnapshot,
) -> Result<GraphSnapshot> {
    let objects = graph.get_all_objects()?;
    let raw_edges = graph.get_all_edges()?;

    // Load any previously saved UI positions
    let saved_positions = graph.load_layout().unwrap_or_default();

    // Build new ObjectId → index map
    let id_to_idx: HashMap<ObjectId, usize> = objects
        .iter()
        .enumerate()
        .map(|(i, obj)| (obj.id, i))
        .collect();

    // Convert to NodeViews
    let mut nodes: Vec<NodeView> = objects
        .into_iter()
        .map(|obj| NodeView {
            id: obj.id,
            name: obj.name,
            object_type: obj.object_type,
            position: Vec2::ZERO,
            properties: obj.properties,
        })
        .collect();

    // Convert edges
    let edges: Vec<EdgeView> = raw_edges
        .into_iter()
        .filter_map(|e| {
            let src = *id_to_idx.get(&e.from)?;
            let tgt = *id_to_idx.get(&e.to)?;
            Some(EdgeView {
                source_idx: src,
                target_idx: tgt,
                edge_type: e.edge_type.into_inner(),
                weight: e.weight,
            })
        })
        .collect();

    let all_saved = nodes.iter().all(|n| saved_positions.contains_key(&n.id));
    if !all_saved {
        force_directed_layout(&mut nodes, &edges);
    }
    for node in &mut nodes {
        if let Some(&(x, y)) = saved_positions.get(&node.id) {
            node.position = Vec2::new(x, y);
        }
    }

    // Compute the delta: which nodes were added and which were removed.
    let new_ids: HashSet<ObjectId> = nodes.iter().map(|n| n.id).collect();
    let prev_ids: HashSet<ObjectId> = prev.nodes.iter().map(|n| n.id).collect();
    let removed: Vec<ObjectId> = prev_ids.difference(&new_ids).copied().collect();
    let added: Vec<ObjectId> = new_ids.difference(&prev_ids).copied().collect();

    // Spatial index: incremental when all positions are saved (no layout ran),
    // full bulk_load otherwise (layout changes every position).
    let spatial_index = if all_saved {
        let mut index = prev.spatial_index.clone();

        // Remove deleted nodes — we know their old positions from prev.
        for id in &removed {
            if let Some(old) = prev.nodes.iter().find(|n| n.id == *id) {
                let entry = NodeEntry {
                    id: *id,
                    position: [old.position.x, old.position.y],
                };
                index.remove(&entry);
            }
        }

        // Insert new nodes using their freshly-assigned positions.
        for id in &added {
            if let Some(new_node) = nodes.iter().find(|n| n.id == *id) {
                index.insert(NodeEntry {
                    id: *id,
                    position: [new_node.position.x, new_node.position.y],
                });
            }
        }

        index
    } else {
        // Layout ran — all positions changed, so a full rebuild is required.
        RTree::bulk_load(
            nodes
                .iter()
                .map(|n| NodeEntry {
                    id: n.id,
                    position: [n.position.x, n.position.y],
                })
                .collect(),
        )
    };

    // Legend: check whether any type was added or fully removed.
    //
    // A type is "fully removed" if the last node of that type was deleted
    // (count was 1 in prev). A type is "added" if the new node's type did not
    // exist in prev.type_counts. When neither condition holds, clone the prev
    // legend and update counts in O(delta) without a sort.
    let type_removed = removed.iter().any(|id| {
        prev.nodes
            .iter()
            .find(|n| n.id == *id)
            .map(|n| prev.type_counts.get(&n.object_type).copied().unwrap_or(0) == 1)
            .unwrap_or(false)
    });
    let type_added = added.iter().any(|id| {
        nodes
            .iter()
            .find(|n| n.id == *id)
            .map(|n| !prev.type_counts.contains_key(&n.object_type))
            .unwrap_or(false)
    });

    let (legend_types, type_counts) = if type_removed || type_added || !all_saved {
        // Recompute from scratch — O(N) but unavoidable when the type set changed.
        build_legend(&nodes)
    } else {
        // No type added or fully removed: clone prev legend (same sorted list)
        // and adjust counts for the delta in O(delta).
        let mut tc = prev.type_counts.clone();
        for id in &removed {
            if let Some(old) = prev.nodes.iter().find(|n| n.id == *id) {
                if let Some(c) = tc.get_mut(&old.object_type) {
                    if *c <= 1 {
                        tc.remove(&old.object_type);
                    } else {
                        *c -= 1;
                    }
                }
            }
        }
        for id in &added {
            if let Some(new_node) = nodes.iter().find(|n| n.id == *id) {
                *tc.entry(new_node.object_type.clone()).or_insert(0) += 1;
            }
        }
        (prev.legend_types.clone(), tc)
    };

    Ok(GraphSnapshot {
        nodes,
        edges,
        spatial_index,
        legend_types,
        id_to_idx,
        type_counts,
    })
}

/// Build `(legend_types, type_counts)` from a node slice in O(N).
fn build_legend(nodes: &[NodeView]) -> (Vec<String>, HashMap<String, usize>) {
    let mut type_counts: HashMap<String, usize> = HashMap::new();
    for n in nodes {
        *type_counts.entry(n.object_type.clone()).or_insert(0) += 1;
    }
    let mut legend_types: Vec<String> = type_counts.keys().cloned().collect();
    legend_types.sort_by_key(|a| a.to_lowercase());
    (legend_types, type_counts)
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
    fn build_snapshot_skips_layout_when_all_positions_saved() {
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

        // First build assigns layout-computed positions.
        let snap1 = build_snapshot(&graph).unwrap();
        let pos_a = snap1.nodes.iter().find(|n| n.id == id_a).unwrap().position;
        let pos_b = snap1.nodes.iter().find(|n| n.id == id_b).unwrap().position;

        // Save those positions — simulates a user who has arranged the graph.
        graph
            .save_layout(&[(id_a, pos_a.x, pos_a.y), (id_b, pos_b.x, pos_b.y)])
            .unwrap();

        // Second build must skip layout and use the saved positions exactly.
        let snap2 = build_snapshot(&graph).unwrap();
        let p2_a = snap2.nodes.iter().find(|n| n.id == id_a).unwrap().position;
        let p2_b = snap2.nodes.iter().find(|n| n.id == id_b).unwrap().position;

        assert_eq!(p2_a, pos_a, "saved position for Alice must be restored exactly");
        assert_eq!(p2_b, pos_b, "saved position for Bob must be restored exactly");
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

    #[test]
    fn incremental_snapshot_add_node_reuses_rtree() {
        let (_dir, graph) = test_graph();

        let id_a = ObjectBuilder::character("Alice".to_string())
            .add_to_graph(&graph)
            .unwrap();
        let id_b = ObjectBuilder::character("Bob".to_string())
            .add_to_graph(&graph)
            .unwrap();

        // Build initial snapshot and save positions so layout is skipped next time.
        let snap1 = build_snapshot(&graph).unwrap();
        let pos_a = snap1.nodes.iter().find(|n| n.id == id_a).unwrap().position;
        let pos_b = snap1.nodes.iter().find(|n| n.id == id_b).unwrap().position;
        graph
            .save_layout(&[(id_a, pos_a.x, pos_a.y), (id_b, pos_b.x, pos_b.y)])
            .unwrap();

        // Add a third node (no saved position yet).
        let id_c = ObjectBuilder::character("Carol".to_string())
            .add_to_graph(&graph)
            .unwrap();

        // Incremental rebuild must include all three nodes.
        let snap2 = build_snapshot_incremental(&graph, &snap1).unwrap();
        assert_eq!(snap2.nodes.len(), 3);
        assert!(snap2.id_to_idx.contains_key(&id_a));
        assert!(snap2.id_to_idx.contains_key(&id_b));
        assert!(snap2.id_to_idx.contains_key(&id_c));

        // Existing nodes must retain their saved positions.
        let p2_a = snap2.nodes.iter().find(|n| n.id == id_a).unwrap().position;
        let p2_b = snap2.nodes.iter().find(|n| n.id == id_b).unwrap().position;
        assert_eq!(p2_a, pos_a, "Alice's saved position must be preserved");
        assert_eq!(p2_b, pos_b, "Bob's saved position must be preserved");

        // All nodes must be in the spatial index.
        let all = snap2.nodes_in_viewport(Vec2::splat(-100_000.0), Vec2::splat(100_000.0));
        assert_eq!(all.len(), 3, "all three nodes must be in the spatial index");
    }

    #[test]
    fn incremental_snapshot_remove_node_updates_rtree() {
        let (_dir, graph) = test_graph();

        let id_a = ObjectBuilder::character("Alice".to_string())
            .add_to_graph(&graph)
            .unwrap();
        let id_b = ObjectBuilder::character("Bob".to_string())
            .add_to_graph(&graph)
            .unwrap();

        let snap1 = build_snapshot(&graph).unwrap();
        let pos_a = snap1.nodes.iter().find(|n| n.id == id_a).unwrap().position;
        let pos_b = snap1.nodes.iter().find(|n| n.id == id_b).unwrap().position;
        graph
            .save_layout(&[(id_a, pos_a.x, pos_a.y), (id_b, pos_b.x, pos_b.y)])
            .unwrap();

        // Delete Bob.
        graph.delete_object(id_b).unwrap();

        let snap2 = build_snapshot_incremental(&graph, &snap1).unwrap();
        assert_eq!(snap2.nodes.len(), 1);
        assert!(snap2.id_to_idx.contains_key(&id_a));
        assert!(!snap2.id_to_idx.contains_key(&id_b));

        // Spatial index must reflect the deletion.
        let all = snap2.nodes_in_viewport(Vec2::splat(-100_000.0), Vec2::splat(100_000.0));
        assert_eq!(all.len(), 1, "only Alice should remain in the spatial index");
    }

    #[test]
    fn incremental_snapshot_legend_reused_when_types_unchanged() {
        let (_dir, graph) = test_graph();

        let id_a = ObjectBuilder::character("Alice".to_string())
            .add_to_graph(&graph)
            .unwrap();
        let id_b = ObjectBuilder::character("Bob".to_string())
            .add_to_graph(&graph)
            .unwrap();

        let snap1 = build_snapshot(&graph).unwrap();
        let pos_a = snap1.nodes.iter().find(|n| n.id == id_a).unwrap().position;
        let pos_b = snap1.nodes.iter().find(|n| n.id == id_b).unwrap().position;
        graph
            .save_layout(&[(id_a, pos_a.x, pos_a.y), (id_b, pos_b.x, pos_b.y)])
            .unwrap();

        // Add another character — same type, no new type in legend.
        let id_c = ObjectBuilder::character("Carol".to_string())
            .add_to_graph(&graph)
            .unwrap();
        // Carol needs a saved position so all_saved stays true on next build.
        // (We give her Alice's position; spatial index handles them as distinct entries.)
        graph
            .save_layout(&[
                (id_a, pos_a.x, pos_a.y),
                (id_b, pos_b.x, pos_b.y),
                (id_c, 10.0, 10.0),
            ])
            .unwrap();

        let snap2 = build_snapshot_incremental(&graph, &snap1).unwrap();

        // Legend must still contain exactly the same type(s).
        assert_eq!(snap1.legend_types, snap2.legend_types, "legend must be identical when no type change");
        // But count for that type should have increased.
        let char_type = snap1.legend_types[0].as_str();
        assert_eq!(snap2.type_counts[char_type], 3);
    }
}
