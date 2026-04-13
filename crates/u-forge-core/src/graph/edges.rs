//! Edge CRUD methods for KnowledgeGraphStorage.

use super::storage::*;
use anyhow::{Context, Result};
use rusqlite::params;
use uuid::Uuid;

use crate::types::{Edge, EdgeType, ObjectId};
use std::collections::HashMap;

impl KnowledgeGraphStorage {
    /// Insert or replace an edge.
    ///
    /// `INSERT OR REPLACE` is safe here because the `edges` table has no
    /// cascading children.  The `UNIQUE(source_id, target_id, edge_type)`
    /// constraint ensures a logical edge is stored at most once; re-inserting
    /// the same (source, target, type) triplet updates `weight` and `metadata`.
    ///
    /// `EdgeType` is stored via `as_str()` and read back as
    /// `EdgeType::Custom(s)`, which round-trips correctly for all variants.
    pub fn upsert_edge(&self, edge: Edge) -> Result<()> {
        let conn = self.conn.lock();
        let meta_json =
            serde_json::to_string(&edge.metadata).context("Failed to serialise edge metadata")?;
        conn.execute(
            "INSERT OR REPLACE INTO edges
                 (source_id, target_id, edge_type, weight, metadata, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                edge.from.hyphenated().to_string(),
                edge.to.hyphenated().to_string(),
                edge.edge_type.as_str(),
                edge.weight as f64,
                meta_json,
                edge.created_at.to_rfc3339(),
            ],
        )
        .context("Failed to upsert edge")?;
        Ok(())
    }

    /// Return all edges incident on `node_id` (both outgoing **and** incoming).
    ///
    /// Each `Edge` is returned exactly once with its canonical `from`/`to`
    /// direction as stored; the caller should check both fields when the
    /// direction matters.
    pub fn get_edges(&self, node_id: ObjectId) -> Result<Vec<Edge>> {
        let conn = self.conn.lock();
        let id_str = node_id.hyphenated().to_string();
        let mut stmt = conn.prepare(
            "SELECT source_id, target_id, edge_type, weight, metadata, created_at
             FROM edges
             WHERE source_id = ?1 OR target_id = ?1",
        )?;
        let rows = stmt.query_map(params![id_str], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, f64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;

        let mut edges = Vec::new();
        for row in rows {
            let (src_s, tgt_s, et_s, weight, meta_s, ca_s) = row?;
            let metadata: HashMap<String, String> =
                serde_json::from_str(&meta_s).unwrap_or_default();
            edges.push(Edge {
                from: Uuid::parse_str(&src_s)
                    .with_context(|| format!("Invalid source UUID in edges table: '{src_s}'"))?,
                to: Uuid::parse_str(&tgt_s)
                    .with_context(|| format!("Invalid target UUID in edges table: '{tgt_s}'"))?,
                edge_type: EdgeType::Custom(et_s),
                weight: weight as f32,
                metadata,
                created_at: chrono::DateTime::parse_from_rfc3339(&ca_s)
                    .with_context(|| format!("Invalid edge created_at: '{ca_s}'"))?
                    .with_timezone(&chrono::Utc),
            });
        }
        Ok(edges)
    }

    /// Return every edge stored in the graph in a single query.
    ///
    /// Prefer this over repeated `get_edges()` calls when building a full graph
    /// snapshot — one `SELECT * FROM edges` is far cheaper than N per-node
    /// round-trips.
    pub fn get_all_edges(&self) -> Result<Vec<Edge>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT source_id, target_id, edge_type, weight, metadata, created_at
             FROM edges",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, f64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;

        let mut edges = Vec::new();
        for row in rows {
            let (src_s, tgt_s, et_s, weight, meta_s, ca_s) = row?;
            let metadata: HashMap<String, String> =
                serde_json::from_str(&meta_s).unwrap_or_default();
            edges.push(Edge {
                from: Uuid::parse_str(&src_s)
                    .with_context(|| format!("Invalid source UUID in edges table: '{src_s}'"))?,
                to: Uuid::parse_str(&tgt_s)
                    .with_context(|| format!("Invalid target UUID in edges table: '{tgt_s}'"))?,
                edge_type: EdgeType::Custom(et_s),
                weight: weight as f32,
                metadata,
                created_at: chrono::DateTime::parse_from_rfc3339(&ca_s)
                    .with_context(|| format!("Invalid edge created_at: '{ca_s}'"))?
                    .with_timezone(&chrono::Utc),
            });
        }
        Ok(edges)
    }

    /// Return the IDs of all nodes reachable in exactly one hop from
    /// `node_id`, following both outgoing and incoming edges.
    ///
    /// Results are deduplicated via `SELECT DISTINCT`.
    pub fn get_neighbors(&self, node_id: ObjectId) -> Result<Vec<ObjectId>> {
        let conn = self.conn.lock();
        let id_str = node_id.hyphenated().to_string();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT
                 CASE WHEN source_id = ?1 THEN target_id ELSE source_id END
             FROM edges
             WHERE source_id = ?1 OR target_id = ?1",
        )?;
        let rows = stmt.query_map(params![id_str], |row| row.get::<_, String>(0))?;

        let mut neighbors = Vec::new();
        for row in rows {
            let uuid_str = row?;
            neighbors.push(
                Uuid::parse_str(&uuid_str)
                    .with_context(|| format!("Invalid neighbor UUID: '{uuid_str}'"))?,
            );
        }
        Ok(neighbors)
    }
}
