//! Node canvas-position persistence for the graph-view UI.
//!
//! Positions are keyed by `node_id` in the `node_positions` table.
//! `ON DELETE CASCADE` keeps the table clean when nodes are removed.

use anyhow::{Context, Result, ensure};
use rusqlite::params;
use std::collections::HashMap;

use crate::types::ObjectId;

use super::storage::KnowledgeGraphStorage;

impl KnowledgeGraphStorage {
    /// Persist the canvas position for every `(node_id, x, y)` triple.
    ///
    /// Uses an upsert so that calling this repeatedly only touches rows that
    /// changed.  Positions for nodes that no longer exist are silently ignored
    /// by the foreign-key constraint (they would already be gone via cascade).
    pub fn save_layout(&self, positions: &[(ObjectId, f32, f32)]) -> Result<()> {
        ensure!(
            positions
                .iter()
                .all(|(_, x, y)| x.is_finite() && y.is_finite()),
            "Cannot persist non-finite node positions"
        );
        let conn = self.conn.lock();
        for (id, x, y) in positions {
            conn.execute(
                "INSERT INTO node_positions (node_id, x, y, layout_version)
                 VALUES (?1, ?2, ?3, 1)
                 ON CONFLICT(node_id) DO UPDATE SET
                     x = excluded.x,
                     y = excluded.y",
                params![id.hyphenated().to_string(), x, y],
            )
            .context("Failed to save node position")?;
        }
        Ok(())
    }

    /// Load all saved canvas positions as an `ObjectId → (x, y)` map.
    ///
    /// Returns an empty map (not an error) when no positions have been saved yet.
    pub fn load_layout(&self) -> Result<HashMap<ObjectId, (f32, f32)>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT node_id, x, y FROM node_positions")
            .context("Failed to prepare load_layout query")?;

        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, f64>(1)?,
                row.get::<_, f64>(2)?,
            ))
        })?;

        let mut map = HashMap::new();
        for row in rows {
            let (id_str, x, y) = row?;
            match ObjectId::parse_str(&id_str) {
                Ok(id) if (x as f32).is_finite() && (y as f32).is_finite() => {
                    map.insert(id, (x as f32, y as f32));
                }
                Ok(id) => {
                    tracing::warn!(%id, x, y, "Skipping non-finite persisted node position");
                }
                Err(_) => {
                    tracing::warn!(node_id = %id_str, "Skipping malformed UUID in node_positions");
                }
            }
        }
        Ok(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ObjectBuilder;
    use tempfile::TempDir;

    #[test]
    fn save_layout_rejects_non_finite_positions_before_writing() {
        let dir = TempDir::new().unwrap();
        let graph = KnowledgeGraphStorage::new(dir.path()).unwrap();
        let object = ObjectBuilder::character("Alice".to_string()).build();
        let id = object.id;
        graph.upsert_node(object).unwrap();

        let error = graph.save_layout(&[(id, f32::NAN, 1.0)]).unwrap_err();
        assert!(error.to_string().contains("non-finite"));
        assert!(graph.load_layout().unwrap().is_empty());
    }

    #[test]
    fn load_layout_skips_non_finite_stored_positions() {
        let dir = TempDir::new().unwrap();
        let graph = KnowledgeGraphStorage::new(dir.path()).unwrap();
        let object = ObjectBuilder::character("Alice".to_string()).build();
        let id = object.id;
        graph.upsert_node(object).unwrap();
        graph
            .conn
            .lock()
            .execute(
                "INSERT INTO node_positions (node_id, x, y, layout_version) VALUES (?1, ?2, ?3, 1)",
                params![id.hyphenated().to_string(), f64::INFINITY, 1.0],
            )
            .unwrap();

        assert!(graph.load_layout().unwrap().is_empty());
    }
}
