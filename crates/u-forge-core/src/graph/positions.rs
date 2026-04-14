//! Node canvas-position persistence for the graph-view UI.
//!
//! Positions are keyed by `node_id` in the `node_positions` table.
//! `ON DELETE CASCADE` keeps the table clean when nodes are removed.

use std::collections::HashMap;
use anyhow::{Context, Result};
use rusqlite::params;

use crate::types::ObjectId;

use super::storage::KnowledgeGraphStorage;

impl KnowledgeGraphStorage {
    /// Persist the canvas position for every `(node_id, x, y)` triple.
    ///
    /// Uses an upsert so that calling this repeatedly only touches rows that
    /// changed.  Positions for nodes that no longer exist are silently ignored
    /// by the foreign-key constraint (they would already be gone via cascade).
    pub fn save_layout(&self, positions: &[(ObjectId, f32, f32)]) -> Result<()> {
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
                Ok(id) => {
                    map.insert(id, (x as f32, y as f32));
                }
                Err(_) => {
                    tracing::warn!(node_id = %id_str, "Skipping malformed UUID in node_positions");
                }
            }
        }
        Ok(map)
    }
}
