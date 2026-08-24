//! Node CRUD methods for KnowledgeGraphStorage.

use super::storage::*;
use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};

use crate::types::{ObjectId, ObjectMetadata};

impl KnowledgeGraphStorage {
    /// Atomically insert or update a batch of nodes.
    pub fn upsert_nodes(&self, metadata: &[ObjectMetadata]) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction().context("Failed to begin node batch")?;
        {
            let mut statement = tx.prepare(
                "INSERT INTO nodes
                     (id, object_type, schema_name, name, properties, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET
                     object_type = excluded.object_type,
                     schema_name = excluded.schema_name,
                     name = excluded.name,
                     properties = excluded.properties,
                     updated_at = excluded.updated_at",
            )?;
            for node in metadata {
                statement.execute(params![
                    node.id.hyphenated().to_string(),
                    &node.object_type,
                    &node.schema_name,
                    &node.name,
                    node.properties.to_string(),
                    node.created_at.to_rfc3339(),
                    node.updated_at.to_rfc3339(),
                ])?;
            }
        }
        tx.commit().context("Failed to commit node batch")?;
        Ok(())
    }

    /// Insert or update a node.
    ///
    /// Uses `ON CONFLICT(id) DO UPDATE SET …` (the SQLite upsert syntax) rather
    /// than `INSERT OR REPLACE` because `INSERT OR REPLACE` performs a DELETE
    /// followed by an INSERT, which would fire the `ON DELETE CASCADE` on the
    /// `edges` and `chunks` tables and wipe out every relationship and text
    /// chunk every time a node property changes.
    pub fn upsert_node(&self, metadata: ObjectMetadata) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO nodes
                 (id, object_type, schema_name, name, properties, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
                 object_type  = excluded.object_type,
                 schema_name  = excluded.schema_name,
                 name         = excluded.name,
                 properties   = excluded.properties,
                 updated_at   = excluded.updated_at",
            params![
                metadata.id.hyphenated().to_string(),
                metadata.object_type,
                metadata.schema_name,
                metadata.name,
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
        let conn = self.conn.lock();
        let result = conn
            .query_row(
                "SELECT id, object_type, schema_name, name, properties, created_at, updated_at
                 FROM nodes
                 WHERE id = ?1",
                params![id.hyphenated().to_string()],
                RawNodeRow::from_row,
            )
            .optional()
            .context("Failed to query node by id")?;

        result.map(RawNodeRow::into_metadata).transpose()
    }

    /// Return every node stored in the graph.
    pub fn get_all_objects(&self) -> Result<Vec<ObjectMetadata>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, object_type, schema_name, name, properties, created_at, updated_at
             FROM nodes",
        )?;
        let rows = stmt.query_map([], RawNodeRow::from_row)?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?.into_metadata()?);
        }
        Ok(out)
    }

    /// Find nodes whose `object_type` **and** `name` both match exactly.
    ///
    /// Uses the composite index `idx_nodes_name (object_type, name)`.
    pub fn find_nodes_by_name(&self, object_type: &str, name: &str) -> Result<Vec<ObjectMetadata>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, object_type, schema_name, name, properties, created_at, updated_at
             FROM nodes
             WHERE object_type = ?1 AND name = ?2",
        )?;
        let rows = stmt.query_map(params![object_type, name], RawNodeRow::from_row)?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?.into_metadata()?);
        }
        Ok(out)
    }

    /// Find nodes whose `name` matches exactly, regardless of `object_type`.
    ///
    /// Backed by `idx_nodes_name_only`.  Intended as a cross-type lookup
    /// fallback (e.g. BUG-7 cross-session edge resolution).
    pub fn find_nodes_by_name_only(&self, name: &str) -> Result<Vec<ObjectMetadata>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, object_type, schema_name, name, properties, created_at, updated_at
             FROM nodes
             WHERE name = ?1",
        )?;
        let rows = stmt.query_map(params![name], RawNodeRow::from_row)?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?.into_metadata()?);
        }
        Ok(out)
    }

    /// Return a page of nodes ordered by name.
    ///
    /// Suitable for building full-graph snapshots incrementally without loading
    /// every node into memory at once.
    pub fn get_nodes_paginated(&self, offset: usize, limit: usize) -> Result<Vec<ObjectMetadata>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, object_type, schema_name, name, properties, created_at, updated_at
             FROM nodes
             ORDER BY name
             LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt.query_map(params![limit as i64, offset as i64], RawNodeRow::from_row)?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?.into_metadata()?);
        }
        Ok(out)
    }

    /// Atomically set a single property on a node using SQLite's `json_set`.
    ///
    /// `value` must be a valid JSON-encoded value (e.g. `"\"foo\""` for a
    /// string, `"42"` for a number, `"[\"a\",\"b\"]"` for an array).
    /// The node's `updated_at` timestamp is bumped on every call.
    pub fn set_node_property(
        &self,
        id: ObjectId,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<()> {
        let conn = self.conn.lock();
        let json_path = format!("$.{key}");
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE nodes
             SET properties = json_set(properties, ?1, json(?2)),
                 updated_at = ?3
             WHERE id = ?4",
            params![
                json_path,
                value.to_string(),
                now,
                id.hyphenated().to_string(),
            ],
        )
        .context("Failed to set node property")?;
        Ok(())
    }

    /// Delete a node by ID.
    ///
    /// `ON DELETE CASCADE` on `edges` and `chunks` handles all dependent rows
    /// automatically — no manual cleanup is required.
    pub fn delete_node(&self, id: ObjectId) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "DELETE FROM nodes WHERE id = ?1",
            params![id.hyphenated().to_string()],
        )
        .context("Failed to delete node")?;
        Ok(())
    }
}
