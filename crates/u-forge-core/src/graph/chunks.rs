//! Chunk storage methods for KnowledgeGraphStorage.

use super::storage::*;
use anyhow::{Context, Result};
use rusqlite::params;
use uuid::Uuid;

use crate::types::{ObjectId, TextChunk};

impl KnowledgeGraphStorage {
    /// Insert or update a text chunk.
    ///
    /// Uses `ON CONFLICT(id) DO UPDATE SET …` rather than `INSERT OR REPLACE`
    /// to **preserve the row's implicit SQLite `rowid`** across updates.  The
    /// FTS5 content table (`chunks_fts`) maps FTS rowids to chunk content via
    /// the `rowid` column; changing the rowid on every write would corrupt the
    /// FTS index.
    ///
    /// The three triggers (`chunks_ai`, `chunks_ad`, `chunks_au`) keep
    /// `chunks_fts` synchronised automatically.
    pub fn upsert_chunk(&self, chunk: TextChunk) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO chunks
                 (id, object_id, chunk_type, content, token_count, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                 chunk_type  = excluded.chunk_type,
                 content     = excluded.content,
                 token_count = excluded.token_count",
            params![
                chunk.id.hyphenated().to_string(),
                chunk.object_id.hyphenated().to_string(),
                chunk_type_to_str(&chunk.chunk_type),
                chunk.content,
                chunk.token_count as i64,
                chunk.created_at.to_rfc3339(),
            ],
        )
        .context("Failed to upsert chunk")?;
        Ok(())
    }

    /// Return all chunks that do not yet have a 768-dim embedding in `chunks_vec`.
    ///
    /// The LEFT JOIN on `chunks_vec` returns only rows where no matching vector
    /// rowid exists — i.e. chunks that have never been embedded via
    /// [`upsert_chunk_embedding`](super::fts::KnowledgeGraphStorage::upsert_chunk_embedding).
    pub fn get_unembedded_chunks(&self) -> Result<Vec<TextChunk>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT c.id, c.object_id, c.chunk_type, c.content, c.token_count, c.created_at
             FROM chunks c
             LEFT JOIN chunks_vec v ON c.rowid = v.rowid
             WHERE v.rowid IS NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        let mut chunks = Vec::new();
        for row in rows {
            let (id_s, obj_s, ct_s, content, token_count, ca_s) = row?;
            chunks.push(TextChunk {
                id: Uuid::parse_str(&id_s)
                    .with_context(|| format!("Invalid chunk UUID: '{id_s}'"))?,
                object_id: Uuid::parse_str(&obj_s)
                    .with_context(|| format!("Invalid object UUID in chunk: '{obj_s}'"))?,
                chunk_type: str_to_chunk_type(&ct_s),
                content,
                token_count: token_count as usize,
                created_at: chrono::DateTime::parse_from_rfc3339(&ca_s)
                    .with_context(|| format!("Invalid chunk created_at: '{ca_s}'"))?
                    .with_timezone(&chrono::Utc),
            });
        }
        Ok(chunks)
    }

    /// Return all chunks that do not yet have a 4096-dim embedding in `chunks_vec_hq`.
    pub fn get_unembedded_chunks_hq(&self) -> Result<Vec<TextChunk>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT c.id, c.object_id, c.chunk_type, c.content, c.token_count, c.created_at
             FROM chunks c
             LEFT JOIN chunks_vec_hq v ON c.rowid = v.rowid
             WHERE v.rowid IS NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        let mut chunks = Vec::new();
        for row in rows {
            let (id_s, obj_s, ct_s, content, token_count, ca_s) = row?;
            chunks.push(TextChunk {
                id: Uuid::parse_str(&id_s)
                    .with_context(|| format!("Invalid chunk UUID: '{id_s}'"))?,
                object_id: Uuid::parse_str(&obj_s)
                    .with_context(|| format!("Invalid object UUID in chunk: '{obj_s}'"))?,
                chunk_type: str_to_chunk_type(&ct_s),
                content,
                token_count: token_count as usize,
                created_at: chrono::DateTime::parse_from_rfc3339(&ca_s)
                    .with_context(|| format!("Invalid chunk created_at: '{ca_s}'"))?
                    .with_timezone(&chrono::Utc),
            });
        }
        Ok(chunks)
    }

    /// Return all text chunks associated with `node_id`.
    pub fn get_chunks_for_node(&self, node_id: ObjectId) -> Result<Vec<TextChunk>> {
        let conn = self.conn.lock();
        let id_str = node_id.hyphenated().to_string();
        let mut stmt = conn.prepare(
            "SELECT id, object_id, chunk_type, content, token_count, created_at
             FROM chunks
             WHERE object_id = ?1",
        )?;
        let rows = stmt.query_map(params![id_str], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;

        let mut chunks = Vec::new();
        for row in rows {
            let (id_s, obj_s, ct_s, content, token_count, ca_s) = row?;
            chunks.push(TextChunk {
                id: Uuid::parse_str(&id_s)
                    .with_context(|| format!("Invalid chunk UUID: '{id_s}'"))?,
                object_id: Uuid::parse_str(&obj_s)
                    .with_context(|| format!("Invalid object UUID in chunk: '{obj_s}'"))?,
                chunk_type: str_to_chunk_type(&ct_s),
                content,
                token_count: token_count as usize,
                created_at: chrono::DateTime::parse_from_rfc3339(&ca_s)
                    .with_context(|| format!("Invalid chunk created_at: '{ca_s}'"))?
                    .with_timezone(&chrono::Utc),
            });
        }
        Ok(chunks)
    }
}
