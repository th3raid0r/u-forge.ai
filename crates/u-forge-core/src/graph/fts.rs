//! Full-text and semantic search methods for KnowledgeGraphStorage.

use super::storage::*;
use anyhow::{Context, Result, anyhow};
use rusqlite::params;

use crate::types::{ChunkId, ObjectId};

impl KnowledgeGraphStorage {
    /// Full-text search over chunk content using the FTS5 index.
    ///
    /// `query` is an FTS5 query string — simple terms (`"wizard"`), phrases
    /// (`"grey hat"`), and prefix queries (`"wiz*"`) are all supported.
    ///
    /// Returns at most `limit` results as `(ChunkId, ObjectId, content)` triples,
    /// ordered by FTS5 relevance rank.
    pub fn search_chunks_fts(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(ChunkId, ObjectId, String)>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT c.id, c.object_id, c.content
             FROM chunks c
             INNER JOIN (
                 SELECT rowid
                 FROM   chunks_fts
                 WHERE  chunks_fts MATCH ?1
                 LIMIT  ?2
             ) fts ON c.rowid = fts.rowid",
        )?;
        let rows = stmt.query_map(params![query, limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;

        let mut results = Vec::new();
        for row in rows {
            let (chunk_id_s, obj_id_s, content) = row?;
            results.push((
                ChunkId::parse_str(&chunk_id_s)
                    .with_context(|| format!("Invalid chunk UUID in FTS result: '{chunk_id_s}'"))?,
                ObjectId::parse_str(&obj_id_s)
                    .with_context(|| format!("Invalid object UUID in FTS result: '{obj_id_s}'"))?,
                content,
            ));
        }
        Ok(results)
    }

    /// Store or update the embedding vector for an existing chunk.
    ///
    /// Looks up the chunk's integer `rowid` from the `chunks` table then
    /// inserts/replaces the corresponding row in `chunks_vec`.  The rowid
    /// mapping mirrors the FTS5 content-table approach so both indexes stay
    /// aligned with the same chunk identity.
    ///
    /// Embeddings are stored as raw little-endian `f32` bytes — the wire format
    /// sqlite-vec expects for `float[N]` columns.
    ///
    /// # Errors
    /// * `chunk_id` does not exist in the `chunks` table.
    /// * `embedding.len()` does not match the configured standard index width.
    pub fn upsert_chunk_embedding(&self, chunk_id: ChunkId, embedding: &[f32]) -> Result<()> {
        self.upsert_chunk_embedding_for_lane(VectorLane::Standard, chunk_id, embedding)
    }

    /// Approximate nearest-neighbour search over stored chunk embeddings.
    ///
    /// Uses the `vec0` cosine-distance index to find at most `limit` chunks
    /// whose stored embeddings are closest to `query_embedding`.  Only chunks
    /// that have been indexed via [`upsert_chunk_embedding`] are candidates —
    /// chunks without a stored embedding are invisible to this method.
    ///
    /// Returns `(chunk_id, object_id, content, distance)` tuples ordered by
    /// ascending cosine distance (`0.0` = identical, `2.0` = maximally
    /// dissimilar).
    ///
    /// Returns an empty `Vec` (not an error) when `chunks_vec` has no rows.
    pub fn search_chunks_semantic(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<(ChunkId, ObjectId, String, f32)>> {
        self.search_chunks_semantic_for_lane(VectorLane::Standard, query_embedding, limit)
    }

    // ── High-quality embedding methods ──────────────────────────────────────

    /// Store or update the high-quality embedding vector for an existing chunk.
    ///
    /// Identical to [`upsert_chunk_embedding`] but writes to the `chunks_vec_hq`
    /// table instead of `chunks_vec`.
    pub fn upsert_chunk_embedding_hq(&self, chunk_id: ChunkId, embedding: &[f32]) -> Result<()> {
        self.upsert_chunk_embedding_for_lane(VectorLane::HighQuality, chunk_id, embedding)
    }

    /// Approximate nearest-neighbour search over the high-quality embedding index.
    ///
    /// Identical to [`search_chunks_semantic`] but queries `chunks_vec_hq`
    /// instead of `chunks_vec`.
    pub fn search_chunks_semantic_hq(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<(ChunkId, ObjectId, String, f32)>> {
        self.search_chunks_semantic_for_lane(VectorLane::HighQuality, query_embedding, limit)
    }

    fn upsert_chunk_embedding_for_lane(
        &self,
        lane: VectorLane,
        chunk_id: ChunkId,
        embedding: &[f32],
    ) -> Result<()> {
        let descriptor = self.vector_lane(lane);
        if embedding.len() != descriptor.expected_dimensions {
            return Err(anyhow!(
                "{} dimension mismatch: expected {}, got {}.",
                lane.dimension_subject(),
                descriptor.expected_dimensions,
                embedding.len()
            ));
        }

        let conn = self.conn.lock();
        let rowid: i64 = conn
            .query_row(
                "SELECT rowid FROM chunks WHERE id = ?1",
                params![chunk_id.hyphenated().to_string()],
                |row| row.get(0),
            )
            .with_context(|| {
                format!(
                    "{}: chunk '{chunk_id}' not found in chunks table",
                    lane.upsert_operation()
                )
            })?;
        let bytes = embedding
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<_>>();

        // vec0 virtual tables do not support INSERT OR REPLACE / ON CONFLICT.
        // Keep the explicit delete-plus-insert under one connection lock so no
        // writer can interleave between the two statements.
        conn.execute(
            &format!("DELETE FROM {} WHERE rowid = ?1", descriptor.table),
            params![rowid],
        )
        .with_context(|| format!("Failed to delete old vector from {}", descriptor.table))?;
        conn.execute(
            &format!(
                "INSERT INTO {}(rowid, embedding) VALUES (?1, ?2)",
                descriptor.table
            ),
            params![rowid, bytes],
        )
        .with_context(|| format!("Failed to insert vector into {}", descriptor.table))?;
        Ok(())
    }

    fn search_chunks_semantic_for_lane(
        &self,
        lane: VectorLane,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<(ChunkId, ObjectId, String, f32)>> {
        let descriptor = self.vector_lane(lane);
        if query_embedding.len() != descriptor.expected_dimensions {
            return Err(anyhow!(
                "{} dimension mismatch: expected {}, got {}.",
                lane.dimension_subject(),
                descriptor.expected_dimensions,
                query_embedding.len()
            ));
        }
        let bytes = query_embedding
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<_>>();
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT c.id, c.object_id, c.content, v.distance
             FROM chunks c
             INNER JOIN (
                 SELECT rowid, distance
                 FROM   {}
                 WHERE  embedding MATCH ?1
                 ORDER  BY distance
                 LIMIT  ?2
             ) v ON c.rowid = v.rowid
             ORDER BY v.distance",
            descriptor.table
        ))?;
        let rows = stmt.query_map(params![bytes, limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, f64>(3)? as f32,
            ))
        })?;

        let mut results = Vec::new();
        for row in rows {
            let (chunk_id, object_id, content, distance) = row?;
            results.push((
                ChunkId::parse_str(&chunk_id).with_context(|| {
                    format!(
                        "Invalid chunk UUID in {} result: '{chunk_id}'",
                        lane.result_context()
                    )
                })?,
                ObjectId::parse_str(&object_id).with_context(|| {
                    format!(
                        "Invalid object UUID in {} result: '{object_id}'",
                        lane.result_context()
                    )
                })?,
                content,
                distance,
            ));
        }
        Ok(results)
    }
}
