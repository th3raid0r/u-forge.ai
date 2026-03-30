//! Graph traversal methods for KnowledgeGraphStorage.

use super::storage::*;
use anyhow::Result;

use crate::types::{ObjectId, QueryResult};
use std::collections::HashSet;
use tracing::warn;

impl KnowledgeGraphStorage {
    /// BFS subgraph expansion starting from `start`, up to `max_hops` hops.
    ///
    /// Traversal details:
    /// * A node is expanded at most once (tracked by a `visited` `HashSet`).
    /// * Both outgoing **and** incoming edges are followed at each hop.
    /// * Edges are deduplicated: each `(source, target, edge_type)` triple
    ///   appears at most once in `QueryResult::edges` regardless of which
    ///   endpoint triggered the visit.
    /// * Text chunks for every visited node are collected into the result.
    /// * If a neighbour UUID has no matching node row (should not happen with FK
    ///   enforcement but guarded anyway), it is skipped with a `warn!`.
    ///
    /// The loop runs for `max_hops + 1` iterations: iteration 0 processes the
    /// start node, iteration 1 its direct neighbours, and so on.
    pub fn query_subgraph(&self, start: ObjectId, max_hops: usize) -> Result<QueryResult> {
        let mut result = QueryResult::new();
        let mut visited: HashSet<ObjectId> = HashSet::new();
        let mut seen_edges: HashSet<(ObjectId, ObjectId, String)> = HashSet::new();
        let mut frontier = vec![start];

        for _hop in 0..=max_hops {
            if frontier.is_empty() {
                break;
            }
            let mut next_frontier: Vec<ObjectId> = Vec::new();

            for node_id in frontier {
                if visited.contains(&node_id) {
                    continue;
                }
                visited.insert(node_id);

                // ── node metadata ─────────────────────────────────────────────
                match self.get_node(node_id)? {
                    Some(meta) => result.add_object(meta),
                    None => {
                        warn!(
                            id = %node_id,
                            "BFS reached a node_id with no metadata row; skipping"
                        );
                        continue;
                    }
                }

                // ── edges (deduplicated) ──────────────────────────────────────
                for edge in self.get_edges(node_id)? {
                    let key = (edge.from, edge.to, edge.edge_type.as_str().to_string());
                    if seen_edges.insert(key) {
                        result.add_edge(edge.clone());
                    }
                    // Enqueue the other endpoint for the next hop.
                    let neighbour = if edge.from == node_id {
                        edge.to
                    } else {
                        edge.from
                    };
                    if !visited.contains(&neighbour) {
                        next_frontier.push(neighbour);
                    }
                }

                // ── text chunks ───────────────────────────────────────────────
                for chunk in self.get_chunks_for_node(node_id)? {
                    result.add_chunk(chunk);
                }
            }

            frontier = next_frontier;
        }

        Ok(result)
    }
}
