//! Full-text and semantic search methods for KnowledgeGraphStorage.

use super::storage::{self, *};
use anyhow::{anyhow, Context, Result};
use rusqlite::params;
use uuid::Uuid;

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
        let conn = self.conn.lock().unwrap();
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
                Uuid::parse_str(&chunk_id_s)
                    .with_context(|| format!("Invalid chunk UUID in FTS result: '{chunk_id_s}'"))?,
                Uuid::parse_str(&obj_id_s)
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
    /// * `embedding.len() != EMBEDDING_DIMENSIONS`.
    pub fn upsert_chunk_embedding(&self, chunk_id: ChunkId, embedding: &[f32]) -> Result<()> {
        if embedding.len() != EMBEDDING_DIMENSIONS {
            return Err(anyhow!(
                "Embedding dimension mismatch: expected {EMBEDDING_DIMENSIONS}, got {}. \
                 Ensure the embedding model matches the vec0 table configuration \
                 (EMBEDDING_DIMENSIONS constant in storage.rs).",
                embedding.len()
            ));
        }

        let conn = self.conn.lock().unwrap();

        // Resolve the chunk's integer rowid — vec0 uses rowid as its PK.
        let rowid: i64 = conn
            .query_row(
                "SELECT rowid FROM chunks WHERE id = ?1",
                params![chunk_id.hyphenated().to_string()],
                |row| row.get(0),
            )
            .with_context(|| {
                format!("upsert_chunk_embedding: chunk '{chunk_id}' not found in chunks table")
            })?;

        // Serialise &[f32] → little-endian bytes (no extra dependency required).
        let bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();

        // vec0 virtual tables do not support INSERT OR REPLACE / ON CONFLICT,
        // so we emulate an upsert with an explicit DELETE (no-op if absent)
        // followed by a fresh INSERT.  Both statements share the same connection
        // lock so no other writer can interleave between them.
        conn.execute("DELETE FROM chunks_vec WHERE rowid = ?1", params![rowid])
            .context("Failed to delete old embedding from chunks_vec")?;

        conn.execute(
            "INSERT INTO chunks_vec(rowid, embedding) VALUES (?1, ?2)",
            params![rowid, bytes],
        )
        .context("Failed to insert embedding into chunks_vec")?;

        Ok(())
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
        let bytes: Vec<u8> = query_embedding
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();

        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT c.id, c.object_id, c.content, v.distance
             FROM chunks c
             INNER JOIN (
                 SELECT rowid, distance
                 FROM   chunks_vec
                 WHERE  embedding MATCH ?1
                 ORDER  BY distance
                 LIMIT  ?2
             ) v ON c.rowid = v.rowid
             ORDER BY v.distance",
        )?;

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
            let (chunk_id_s, obj_id_s, content, distance) = row?;
            results.push((
                Uuid::parse_str(&chunk_id_s).with_context(|| {
                    format!("Invalid chunk UUID in semantic result: '{chunk_id_s}'")
                })?,
                Uuid::parse_str(&obj_id_s).with_context(|| {
                    format!("Invalid object UUID in semantic result: '{obj_id_s}'")
                })?,
                content,
                distance,
            ));
        }
        Ok(results)
    }

    // ── High-quality (4096-dim) embedding methods ───────────────────────────

    /// Store or update the high-quality embedding vector for an existing chunk.
    ///
    /// Identical to [`upsert_chunk_embedding`] but writes to the `chunks_vec_hq`
    /// table (4096-dim) instead of `chunks_vec` (768-dim).
    pub fn upsert_chunk_embedding_hq(&self, chunk_id: ChunkId, embedding: &[f32]) -> Result<()> {
        if embedding.len() != storage::HIGH_QUALITY_EMBEDDING_DIMENSIONS {
            return Err(anyhow!(
                "HQ embedding dimension mismatch: expected {}, got {}.",
                storage::HIGH_QUALITY_EMBEDDING_DIMENSIONS,
                embedding.len()
            ));
        }

        let conn = self.conn.lock().unwrap();

        let rowid: i64 = conn
            .query_row(
                "SELECT rowid FROM chunks WHERE id = ?1",
                params![chunk_id.hyphenated().to_string()],
                |row| row.get(0),
            )
            .with_context(|| {
                format!("upsert_chunk_embedding_hq: chunk '{chunk_id}' not found in chunks table")
            })?;

        let bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();

        conn.execute("DELETE FROM chunks_vec_hq WHERE rowid = ?1", params![rowid])
            .context("Failed to delete old HQ embedding from chunks_vec_hq")?;

        conn.execute(
            "INSERT INTO chunks_vec_hq(rowid, embedding) VALUES (?1, ?2)",
            params![rowid, bytes],
        )
        .context("Failed to insert HQ embedding into chunks_vec_hq")?;

        Ok(())
    }

    /// Approximate nearest-neighbour search over the high-quality embedding index.
    ///
    /// Identical to [`search_chunks_semantic`] but queries `chunks_vec_hq`
    /// (4096-dim) instead of `chunks_vec` (768-dim).
    pub fn search_chunks_semantic_hq(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<(ChunkId, ObjectId, String, f32)>> {
        let bytes: Vec<u8> = query_embedding
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();

        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT c.id, c.object_id, c.content, v.distance
             FROM chunks c
             INNER JOIN (
                 SELECT rowid, distance
                 FROM   chunks_vec_hq
                 WHERE  embedding MATCH ?1
                 ORDER  BY distance
                 LIMIT  ?2
             ) v ON c.rowid = v.rowid
             ORDER BY v.distance",
        )?;

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
            let (chunk_id_s, obj_id_s, content, distance) = row?;
            results.push((
                Uuid::parse_str(&chunk_id_s).with_context(|| {
                    format!("Invalid chunk UUID in HQ semantic result: '{chunk_id_s}'")
                })?,
                Uuid::parse_str(&obj_id_s).with_context(|| {
                    format!("Invalid object UUID in HQ semantic result: '{obj_id_s}'")
                })?,
                content,
                distance,
            ));
        }
        Ok(results)
    }
}
