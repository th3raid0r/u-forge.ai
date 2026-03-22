//! Node CRUD methods for KnowledgeGraphStorage.

use super::storage::*;
use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension};

use crate::types::{ObjectId, ObjectMetadata};

impl KnowledgeGraphStorage {
    /// Insert or update a node.
    ///
    /// Uses `ON CONFLICT(id) DO UPDATE SET …` (the SQLite upsert syntax) rather
    /// than `INSERT OR REPLACE` because `INSERT OR REPLACE` performs a DELETE
    /// followed by an INSERT, which would fire the `ON DELETE CASCADE` on the
    /// `edges` and `chunks` tables and wipe out every relationship and text
    /// chunk every time a node property changes.
    pub fn upsert_node(&self, metadata: ObjectMetadata) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO nodes
                 (id, object_type, schema_name, name, description,
                  tags, properties, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET
                 object_type  = excluded.object_type,
                 schema_name  = excluded.schema_name,
                 name         = excluded.name,
                 description  = excluded.description,
                 tags         = excluded.tags,
                 properties   = excluded.properties,
                 updated_at   = excluded.updated_at",
            params![
                metadata.id.hyphenated().to_string(),
                metadata.object_type,
                metadata.schema_name,
                metadata.name,
                metadata.description,
                serde_json::to_string(&metadata.tags).context("Failed to serialise node tags")?,
                metadata.properties.to_string(),
                metadata.created_at.to_rfc3339(),
                metadata.updated_at.to_rfc3339(),
            ],
        )
        .context("Failed to upsert node")?;
        Ok(())
    }

    /// Retrieve a node by its UUID.  Returns `Ok(None)` when the ID is unknown.
    pub fn get_node(&self, id: ObjectId) -> Result<Option<ObjectMetadata>> {
        let conn = self.conn.lock().unwrap();
        let result = conn
            .query_row(
                "SELECT id, object_type, schema_name, name, description,
                        tags, properties, created_at, updated_at
                 FROM nodes
                 WHERE id = ?1",
                params![id.hyphenated().to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                    ))
                },
            )
            .optional()
            .context("Failed to query node by id")?;

        match result {
            None => Ok(None),
            Some((id_s, ot, sn, nm, desc, tags, props, ca, ua)) => Ok(Some(row_to_metadata(
                id_s, ot, sn, nm, desc, tags, props, ca, ua,
            )?)),
        }
    }

    /// Return every node stored in the graph.
    pub fn get_all_objects(&self) -> Result<Vec<ObjectMetadata>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, object_type, schema_name, name, description,
                    tags, properties, created_at, updated_at
             FROM nodes",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
            ))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (id_s, ot, sn, nm, desc, tags, props, ca, ua) = row?;
            out.push(row_to_metadata(
                id_s, ot, sn, nm, desc, tags, props, ca, ua,
            )?);
        }
        Ok(out)
    }

    /// Find nodes whose `object_type` **and** `name` both match exactly.
    ///
    /// Uses the composite index `idx_nodes_name (object_type, name)`.
    pub fn find_nodes_by_name(&self, object_type: &str, name: &str) -> Result<Vec<ObjectMetadata>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, object_type, schema_name, name, description,
                    tags, properties, created_at, updated_at
             FROM nodes
             WHERE object_type = ?1 AND name = ?2",
        )?;
        let rows = stmt.query_map(params![object_type, name], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
            ))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (id_s, ot, sn, nm, desc, tags, props, ca, ua) = row?;
            out.push(row_to_metadata(
                id_s, ot, sn, nm, desc, tags, props, ca, ua,
            )?);
        }
        Ok(out)
    }

    /// Find nodes whose `name` matches exactly, regardless of `object_type`.
    ///
    /// Backed by `idx_nodes_name_only`.  Intended as a cross-type lookup
    /// fallback (e.g. BUG-7 cross-session edge resolution).
    pub fn find_nodes_by_name_only(&self, name: &str) -> Result<Vec<ObjectMetadata>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, object_type, schema_name, name, description,
                    tags, properties, created_at, updated_at
             FROM nodes
             WHERE name = ?1",
        )?;
        let rows = stmt.query_map(params![name], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
            ))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (id_s, ot, sn, nm, desc, tags, props, ca, ua) = row?;
            out.push(row_to_metadata(
                id_s, ot, sn, nm, desc, tags, props, ca, ua,
            )?);
        }
        Ok(out)
    }

    /// Delete a node by ID.
    ///
    /// `ON DELETE CASCADE` on `edges` and `chunks` handles all dependent rows
    /// automatically — no manual cleanup is required.
    pub fn delete_node(&self, id: ObjectId) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM nodes WHERE id = ?1",
            params![id.hyphenated().to_string()],
        )
        .context("Failed to delete node")?;
        Ok(())
    }
}
