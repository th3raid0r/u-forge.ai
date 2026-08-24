//! Chunk storage methods for KnowledgeGraphStorage.

use super::storage::*;
use anyhow::{Context, Result};
use rusqlite::params;

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

    /// Return all chunks that do not yet have a standard embedding in `chunks_vec`.
    ///
    /// The LEFT JOIN on `chunks_vec` returns only rows where no matching vector
    /// rowid exists — i.e. chunks that have never been embedded via
    /// [`upsert_chunk_embedding`](super::fts::KnowledgeGraphStorage::upsert_chunk_embedding).
    pub fn get_unembedded_chunks(&self) -> Result<Vec<TextChunk>> {
        self.get_unembedded_chunks_for_lane(VectorLane::Standard)
    }

    /// Return all chunks that do not yet have a high-quality embedding in `chunks_vec_hq`.
    pub fn get_unembedded_chunks_hq(&self) -> Result<Vec<TextChunk>> {
        self.get_unembedded_chunks_for_lane(VectorLane::HighQuality)
    }

    fn get_unembedded_chunks_for_lane(&self, lane: VectorLane) -> Result<Vec<TextChunk>> {
        let descriptor = self.vector_lane(lane);
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT c.id, c.object_id, c.chunk_type, c.content, c.token_count, c.created_at
             FROM chunks c
             LEFT JOIN {} v ON c.rowid = v.rowid
             WHERE v.rowid IS NULL",
            descriptor.table
        ))?;
        let rows = stmt.query_map([], RawChunkRow::from_row)?;
        let mut chunks = Vec::new();
        for row in rows {
            chunks.push(row?.into_chunk()?);
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
        let rows = stmt.query_map(params![id_str], RawChunkRow::from_row)?;

        let mut chunks = Vec::new();
        for row in rows {
            chunks.push(row?.into_chunk()?);
        }
        Ok(chunks)
    }

    /// Delete all text chunks belonging to `node_id`.
    ///
    /// This removes the chunk rows from `chunks`; the `chunks_ad` and
    /// `chunks_vec_ad` / `chunks_vec_hq_ad` triggers automatically clean up
    /// the corresponding FTS5 and vector-index entries.
    ///
    /// Used by the rechunk-on-save path to replace stale chunks with freshly
    /// flattened content.
    pub fn delete_chunks_for_node(&self, node_id: ObjectId) -> Result<usize> {
        let conn = self.conn.lock();
        let id_str = node_id.hyphenated().to_string();
        let deleted = conn
            .execute("DELETE FROM chunks WHERE object_id = ?1", params![id_str])
            .context("Failed to delete chunks for node")?;
        Ok(deleted)
    }
}
