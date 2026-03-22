//! SQLite-backed knowledge graph storage for u-forge.ai.
//!
//! Each database lives at `<db_path>/knowledge.db`.  The schema uses:
//! * WAL journal mode for write concurrency.
//! * `PRAGMA foreign_keys=ON` with `ON DELETE CASCADE` on `edges` and `chunks`.
//! * FTS5 content-table on `chunks` for full-text search.
//! * Three DML triggers to keep the FTS5 index in sync with the `chunks` table.
//! * `vec0` virtual table (`chunks_vec`) via sqlite-vec for ANN similarity search.
//!
//! # Thread safety
//! `Connection` is wrapped in `Arc<Mutex<Connection>>` so `KnowledgeGraphStorage`
//! is `Send + Sync` and can be placed behind an `Arc` in the facade layer.

use crate::schema::SchemaDefinition;
use crate::types::{
    ChunkId, ChunkType, Edge, EdgeType, ObjectId, ObjectMetadata, QueryResult, TextChunk,
};
use anyhow::{anyhow, Context, Result};
use rusqlite::{ffi::sqlite3_auto_extension, params, Connection, OptionalExtension};
use sqlite_vec::sqlite3_vec_init;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, Once};
use tracing::warn;
use uuid::Uuid;

// ─── SQL schema ───────────────────────────────────────────────────────────────

const SQL_SCHEMA: &str = r#"
PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;

CREATE TABLE IF NOT EXISTS nodes (
    id          TEXT PRIMARY KEY,
    object_type TEXT NOT NULL,
    schema_name TEXT,
    name        TEXT NOT NULL,
    description TEXT,
    tags        TEXT NOT NULL DEFAULT '[]',
    properties  TEXT NOT NULL DEFAULT '{}',
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS edges (
    source_id  TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    target_id  TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    edge_type  TEXT NOT NULL,
    weight     REAL NOT NULL DEFAULT 1.0,
    metadata   TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    UNIQUE(source_id, target_id, edge_type)
);

CREATE TABLE IF NOT EXISTS chunks (
    id          TEXT PRIMARY KEY,
    object_id   TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    chunk_type  TEXT NOT NULL,
    content     TEXT NOT NULL,
    token_count INTEGER NOT NULL,
    created_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS schemas (
    name       TEXT PRIMARY KEY,
    definition TEXT NOT NULL
);

CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
    content,
    content='chunks',
    content_rowid='rowid'
);

CREATE INDEX IF NOT EXISTS idx_nodes_type      ON nodes(object_type);
CREATE INDEX IF NOT EXISTS idx_nodes_name      ON nodes(object_type, name);
CREATE INDEX IF NOT EXISTS idx_nodes_name_only ON nodes(name);
CREATE INDEX IF NOT EXISTS idx_edges_source    ON edges(source_id);
CREATE INDEX IF NOT EXISTS idx_edges_target    ON edges(target_id);
CREATE INDEX IF NOT EXISTS idx_chunks_object   ON chunks(object_id);

CREATE TRIGGER IF NOT EXISTS chunks_ai AFTER INSERT ON chunks BEGIN
    INSERT INTO chunks_fts(rowid, content) VALUES (new.rowid, new.content);
END;
CREATE TRIGGER IF NOT EXISTS chunks_ad AFTER DELETE ON chunks BEGIN
    INSERT INTO chunks_fts(chunks_fts, rowid, content) VALUES('delete', old.rowid, old.content);
END;
CREATE TRIGGER IF NOT EXISTS chunks_au AFTER UPDATE ON chunks BEGIN
    INSERT INTO chunks_fts(chunks_fts, rowid, content) VALUES('delete', old.rowid, old.content);
    INSERT INTO chunks_fts(rowid, content) VALUES (new.rowid, new.content);
END;

-- ── ANN vector search (sqlite-vec) ────────────────────────────────────────────
-- Each row maps a chunk rowid → its 256-dim embedding (cosine distance).
-- Rows are inserted explicitly via upsert_chunk_embedding(); not every chunk
-- has a vector immediately after creation (embeddings are populated async).
CREATE VIRTUAL TABLE IF NOT EXISTS chunks_vec USING vec0(
    embedding float[768] distance_metric=cosine
);

-- Keep chunks_vec clean when a chunk is hard-deleted (via cascade or directly).
CREATE TRIGGER IF NOT EXISTS chunks_vec_ad AFTER DELETE ON chunks BEGIN
    DELETE FROM chunks_vec WHERE rowid = old.rowid;
END;
"#;

// ─── Constants & process-level init ───────────────────────────────────────────

/// Number of dimensions produced by the active embedding model.
///
/// Must match the `float[N]` declaration in the `chunks_vec` `vec0` virtual
/// table above.  Currently set for `embed-gemma-300m-FLM` (768-dim).
///
/// **If you switch embedding models**, update this constant *and* recreate the
/// database — `vec0` virtual tables cannot be `ALTER`ed after creation.
pub const EMBEDDING_DIMENSIONS: usize = 768;

/// Maximum number of tokens per stored text chunk.
///
/// Chunks larger than this are split at word boundaries by
/// [`KnowledgeGraph::add_text_chunk`] before being written to storage.
///
/// The llamacpp embedding backends enforce a 512-token physical batch limit.
/// The `len/4` character heuristic used by `estimate_token_count` tends to
/// *under*count real tokens for dense prose (typical variance: 30–50%).  At
/// 350 tokens the heuristic would need to undercount by 46% before a chunk
/// reaches the 512-token server limit, which gives a comfortable safety margin
/// across a wide range of text styles.
pub const MAX_CHUNK_TOKENS: usize = 350;

/// Enable high-quality embedding models (e.g. `Qwen3-Embedding-8B-GGUF`).
///
/// High-quality models produce vectors with a **different dimensionality**
/// (e.g. 4096-dim for Qwen3) that is incompatible with the standard
/// [`EMBEDDING_DIMENSIONS`] index.  Enabling this flag requires a separate
/// `vec0` virtual table and a distinct search path — neither of which is
/// implemented yet.
///
/// **Keep this `false` until the high-quality search pipeline is built.**
/// Flipping it to `true` without the supporting infrastructure will cause
/// model registration to include Qwen3-Embedding-8B-GGUF in the standard
/// 768-dim worker pool, producing dimension-mismatch errors at embedding time.
pub const ENABLE_HIGH_QUALITY_EMBEDDING: bool = false;

/// Guards the one-time sqlite-vec auto-extension registration.
///
/// `sqlite3_auto_extension` is a **process-wide** side-effect; it must be
/// called before the first `Connection::open` and should fire exactly once.
/// Using `Once` makes the intent explicit and prevents redundant FFI calls
/// in tests that spin up many `KnowledgeGraphStorage` instances in sequence.
static SQLITE_VEC_INIT: Once = Once::new();

// ─── Public types ─────────────────────────────────────────────────────────────

/// SQLite-backed storage engine for the knowledge graph.
///
/// Wraps a single `rusqlite::Connection` in `Arc<Mutex<…>>` so the struct is
/// cheaply cloneable and safe to share across threads.
pub struct KnowledgeGraphStorage {
    conn: Arc<Mutex<Connection>>,
}

/// Aggregate statistics about the knowledge graph.
#[derive(Debug, Clone)]
pub struct GraphStats {
    pub node_count: usize,
    pub edge_count: usize,
    pub chunk_count: usize,
    pub total_tokens: usize,
}

// ─── Private helpers ──────────────────────────────────────────────────────────

/// Serialise a `ChunkType` to its snake_case storage string.
fn chunk_type_to_str(ct: &ChunkType) -> &'static str {
    match ct {
        ChunkType::Description => "description",
        ChunkType::SessionNote => "session_note",
        ChunkType::AiGenerated => "ai_generated",
        ChunkType::UserNote => "user_note",
        ChunkType::Imported => "imported",
    }
}

/// Deserialise a `ChunkType` from its stored snake_case string.
///
/// Unknown values fall back to `ChunkType::Description` with a warning
/// rather than panicking, matching the tolerant-reader principle.
fn str_to_chunk_type(s: &str) -> ChunkType {
    match s {
        "session_note" => ChunkType::SessionNote,
        "ai_generated" => ChunkType::AiGenerated,
        "user_note" => ChunkType::UserNote,
        "imported" => ChunkType::Imported,
        "description" => ChunkType::Description,
        other => {
            warn!(
                value = other,
                "Unknown chunk_type in database; defaulting to Description"
            );
            ChunkType::Description
        }
    }
}

/// Build an `ObjectMetadata` from the nine column values returned by every
/// `SELECT … FROM nodes` query.  Centralising this avoids repeating
/// fallible parsing logic across multiple methods.
fn row_to_metadata(
    id_str: String,
    object_type: String,
    schema_name: Option<String>,
    name: String,
    description: Option<String>,
    tags_str: String,
    props_str: String,
    created_at_str: String,
    updated_at_str: String,
) -> Result<ObjectMetadata> {
    Ok(ObjectMetadata {
        id: Uuid::parse_str(&id_str)
            .with_context(|| format!("Invalid UUID in nodes table: '{id_str}'"))?,
        object_type,
        schema_name,
        name,
        description,
        tags: serde_json::from_str(&tags_str)
            .with_context(|| format!("Invalid tags JSON: '{tags_str}'"))?,
        properties: serde_json::from_str(&props_str)
            .with_context(|| format!("Invalid properties JSON: '{props_str}'"))?,
        created_at: chrono::DateTime::parse_from_rfc3339(&created_at_str)
            .with_context(|| format!("Invalid created_at timestamp: '{created_at_str}'"))?
            .with_timezone(&chrono::Utc),
        updated_at: chrono::DateTime::parse_from_rfc3339(&updated_at_str)
            .with_context(|| format!("Invalid updated_at timestamp: '{updated_at_str}'"))?
            .with_timezone(&chrono::Utc),
    })
}

// ─── Implementation ───────────────────────────────────────────────────────────

impl KnowledgeGraphStorage {
    // ── Construction ──────────────────────────────────────────────────────────

    /// Open (or create) the knowledge graph database at `<db_path>/knowledge.db`.
    ///
    /// `db_path` is treated as a directory.  All missing parent directories are
    /// created automatically.  The full SQLite schema (tables, indexes, FTS5
    /// virtual table, and triggers) is applied on every open via
    /// `CREATE … IF NOT EXISTS`, so this method is idempotent.
    pub fn new(db_path: &Path) -> Result<Self> {
        std::fs::create_dir_all(db_path).context("Failed to create database directory")?;

        // Register sqlite-vec as a process-wide SQLite auto-extension so that
        // every subsequent Connection::open automatically gets the vec0
        // virtual-table module.  The Once guard ensures the unsafe FFI call
        // fires exactly once per process — safe even in multi-threaded tests
        // that each create their own KnowledgeGraphStorage on a TempDir.
        SQLITE_VEC_INIT.call_once(|| unsafe {
            sqlite3_auto_extension(Some(std::mem::transmute(sqlite3_vec_init as *const ())));
        });

        let db_file = db_path.join("knowledge.db");
        let conn = Connection::open(&db_file)
            .with_context(|| format!("Failed to open SQLite database at {db_file:?}"))?;

        // Apply WAL mode, FK enforcement, DDL, indexes, FTS triggers, and the
        // chunks_vec vec0 virtual table in one batch.  `execute_batch` uses
        // sqlite3_exec internally and ignores result rows from PRAGMA statements.
        conn.execute_batch(SQL_SCHEMA)
            .context("Failed to initialise database schema")?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    // ── Nodes ─────────────────────────────────────────────────────────────────

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

    // ── Edges ─────────────────────────────────────────────────────────────────

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
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
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

    /// Return the IDs of all nodes reachable in exactly one hop from
    /// `node_id`, following both outgoing and incoming edges.
    ///
    /// Results are deduplicated via `SELECT DISTINCT`.
    pub fn get_neighbors(&self, node_id: ObjectId) -> Result<Vec<ObjectId>> {
        let conn = self.conn.lock().unwrap();
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

    // ── Chunks ────────────────────────────────────────────────────────────────

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
        let conn = self.conn.lock().unwrap();
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

    /// Return all text chunks associated with `node_id`.
    pub fn get_chunks_for_node(&self, node_id: ObjectId) -> Result<Vec<TextChunk>> {
        let conn = self.conn.lock().unwrap();
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

    // ── Semantic (vector) search ──────────────────────────────────────────────

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

    // ── Graph traversal ───────────────────────────────────────────────────────

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

    // ── Statistics ────────────────────────────────────────────────────────────

    /// Return aggregate graph statistics.
    ///
    /// All four queries use indexed `COUNT(*)` or `SUM(…)` — effectively O(1)
    /// regardless of graph size.
    pub fn get_stats(&self) -> Result<GraphStats> {
        let conn = self.conn.lock().unwrap();

        let node_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))
            .context("Failed to count nodes")?;
        let edge_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))
            .context("Failed to count edges")?;
        let chunk_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
            .context("Failed to count chunks")?;
        let total_tokens: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(token_count), 0) FROM chunks",
                [],
                |r| r.get(0),
            )
            .context("Failed to sum token_count")?;

        Ok(GraphStats {
            node_count: node_count as usize,
            edge_count: edge_count as usize,
            chunk_count: chunk_count as usize,
            total_tokens: total_tokens as usize,
        })
    }

    // ── Schemas ───────────────────────────────────────────────────────────────

    /// Retrieve a schema definition by name.  Returns `Ok(None)` if absent.
    pub fn get_schema(&self, name: &str) -> Result<Option<SchemaDefinition>> {
        let conn = self.conn.lock().unwrap();
        let result = conn
            .query_row(
                "SELECT definition FROM schemas WHERE name = ?1",
                params![name],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .context("Failed to query schema by name")?;

        match result {
            None => Ok(None),
            Some(json) => {
                let schema: SchemaDefinition = serde_json::from_str(&json)
                    .context("Failed to deserialise SchemaDefinition from JSON")?;
                Ok(Some(schema))
            }
        }
    }

    /// Persist a schema definition, inserting or replacing by name.
    pub fn save_schema(&self, schema: &SchemaDefinition) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let json = serde_json::to_string(schema)
            .context("Failed to serialise SchemaDefinition to JSON")?;
        conn.execute(
            "INSERT OR REPLACE INTO schemas (name, definition) VALUES (?1, ?2)",
            params![schema.name, json],
        )
        .context("Failed to save schema")?;
        Ok(())
    }

    /// Delete a schema by name.  No-ops silently if the name does not exist.
    pub fn delete_schema(&self, name: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM schemas WHERE name = ?1", params![name])
            .context("Failed to delete schema")?;
        Ok(())
    }

    /// Return the names of all stored schemas, sorted alphabetically.
    pub fn list_schemas(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT name FROM schemas ORDER BY name")
            .context("Failed to prepare list_schemas statement")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;

        let mut names = Vec::new();
        for row in rows {
            names.push(row?);
        }
        Ok(names)
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChunkType, EdgeType, TextChunk};
    use tempfile::TempDir;

    // ── Test fixtures ─────────────────────────────────────────────────────────

    fn create_test_storage() -> (KnowledgeGraphStorage, TempDir) {
        let dir = TempDir::new().expect("TempDir::new failed");
        let storage =
            KnowledgeGraphStorage::new(dir.path()).expect("KnowledgeGraphStorage::new failed");
        (storage, dir)
    }

    // ── Node CRUD ─────────────────────────────────────────────────────────────

    #[test]
    fn test_create_get_node() {
        let (storage, _dir) = create_test_storage();

        let node = ObjectMetadata::new("character".to_string(), "Gandalf".to_string())
            .with_description("A wise and ancient wizard.".to_string());
        let node_id = node.id;

        storage.upsert_node(node.clone()).unwrap();

        // Retrieve by ID.
        let got = storage.get_node(node_id).unwrap().unwrap();
        assert_eq!(got.id, node_id);
        assert_eq!(got.name, "Gandalf");
        assert_eq!(got.object_type, "character");
        assert_eq!(
            got.description,
            Some("A wise and ancient wizard.".to_string())
        );

        // Find by (type, name).
        let by_name = storage.find_nodes_by_name("character", "Gandalf").unwrap();
        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].id, node_id);

        // Find by name only (cross-type).
        let any_type = storage.find_nodes_by_name_only("Gandalf").unwrap();
        assert_eq!(any_type.len(), 1);
        assert_eq!(any_type[0].id, node_id);

        // Wrong type should produce no results.
        assert!(storage
            .find_nodes_by_name("location", "Gandalf")
            .unwrap()
            .is_empty());

        // Unknown ID returns None.
        assert!(storage.get_node(Uuid::new_v4()).unwrap().is_none());

        // Upsert should update without dropping the node.
        let mut updated = got.clone();
        updated.name = "Gandalf the White".to_string();
        storage.upsert_node(updated).unwrap();
        let after_update = storage.get_node(node_id).unwrap().unwrap();
        assert_eq!(after_update.name, "Gandalf the White");

        // get_all_objects should include the node.
        let all = storage.get_all_objects().unwrap();
        assert_eq!(all.len(), 1);
    }

    // ── Edges ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_upsert_get_edges() {
        let (storage, _dir) = create_test_storage();

        let gandalf = ObjectMetadata::new("character".to_string(), "Gandalf".to_string());
        let frodo = ObjectMetadata::new("character".to_string(), "Frodo".to_string());
        storage.upsert_node(gandalf.clone()).unwrap();
        storage.upsert_node(frodo.clone()).unwrap();

        let edge = Edge::new(gandalf.id, frodo.id, EdgeType::from_str("knows"));
        storage.upsert_edge(edge).unwrap();

        // Outgoing from Gandalf.
        let g_edges = storage.get_edges(gandalf.id).unwrap();
        assert_eq!(g_edges.len(), 1);
        assert_eq!(g_edges[0].from, gandalf.id);
        assert_eq!(g_edges[0].to, frodo.id);
        assert_eq!(g_edges[0].edge_type.as_str(), "knows");

        // Incoming to Frodo — same edge, same direction stored.
        let f_edges = storage.get_edges(frodo.id).unwrap();
        assert_eq!(f_edges.len(), 1);
        assert_eq!(f_edges[0].from, gandalf.id);
        assert_eq!(f_edges[0].to, frodo.id);

        // Neighbours.
        let g_neighbours = storage.get_neighbors(gandalf.id).unwrap();
        assert_eq!(g_neighbours.len(), 1);
        assert_eq!(g_neighbours[0], frodo.id);

        let f_neighbours = storage.get_neighbors(frodo.id).unwrap();
        assert_eq!(f_neighbours.len(), 1);
        assert_eq!(f_neighbours[0], gandalf.id);

        // Re-inserting the same triplet should not create a duplicate.
        let edge2 = Edge::new(gandalf.id, frodo.id, EdgeType::from_str("knows"));
        storage.upsert_edge(edge2).unwrap();
        assert_eq!(storage.get_edges(gandalf.id).unwrap().len(), 1);

        // An isolated node has no edges.
        let sam = ObjectMetadata::new("character".to_string(), "Sam".to_string());
        storage.upsert_node(sam.clone()).unwrap();
        assert!(storage.get_edges(sam.id).unwrap().is_empty());
        assert!(storage.get_neighbors(sam.id).unwrap().is_empty());
    }

    // ── Cascade delete ────────────────────────────────────────────────────────

    #[test]
    fn test_delete_node_cascade() {
        let (storage, _dir) = create_test_storage();

        let gandalf = ObjectMetadata::new("character".to_string(), "Gandalf".to_string());
        let frodo = ObjectMetadata::new("character".to_string(), "Frodo".to_string());
        storage.upsert_node(gandalf.clone()).unwrap();
        storage.upsert_node(frodo.clone()).unwrap();

        storage
            .upsert_edge(Edge::new(gandalf.id, frodo.id, EdgeType::from_str("knows")))
            .unwrap();

        let chunk = TextChunk::new(
            gandalf.id,
            "A wizard of the Istari order, one of five sent to Middle-earth.".to_string(),
            ChunkType::Description,
        );
        storage.upsert_chunk(chunk).unwrap();

        // Verify the edge and chunk exist before deletion.
        assert_eq!(storage.get_edges(frodo.id).unwrap().len(), 1);
        assert_eq!(storage.get_chunks_for_node(gandalf.id).unwrap().len(), 1);

        // Delete Gandalf; ON DELETE CASCADE removes the edge and chunk.
        storage.delete_node(gandalf.id).unwrap();

        // Node is gone.
        assert!(storage.get_node(gandalf.id).unwrap().is_none());

        // Edge was cascaded away — Frodo now has no edges.
        assert!(storage.get_edges(frodo.id).unwrap().is_empty());

        // Chunk was cascaded away.
        assert!(storage.get_chunks_for_node(gandalf.id).unwrap().is_empty());

        // Frodo still exists.
        assert!(storage.get_node(frodo.id).unwrap().is_some());

        // Deleting a non-existent node is a no-op.
        storage.delete_node(Uuid::new_v4()).unwrap();
    }

    // ── Stats ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_get_stats() {
        let (storage, _dir) = create_test_storage();

        // Empty graph.
        let empty = storage.get_stats().unwrap();
        assert_eq!(empty.node_count, 0);
        assert_eq!(empty.edge_count, 0);
        assert_eq!(empty.chunk_count, 0);
        assert_eq!(empty.total_tokens, 0);

        let gandalf = ObjectMetadata::new("character".to_string(), "Gandalf".to_string());
        let frodo = ObjectMetadata::new("character".to_string(), "Frodo".to_string());
        storage.upsert_node(gandalf.clone()).unwrap();
        storage.upsert_node(frodo.clone()).unwrap();
        storage
            .upsert_edge(Edge::new(gandalf.id, frodo.id, EdgeType::from_str("knows")))
            .unwrap();

        let chunk = TextChunk::new(
            gandalf.id,
            "A wise wizard of great power and ancient lineage.".to_string(),
            ChunkType::Description,
        );
        storage.upsert_chunk(chunk).unwrap();

        let stats = storage.get_stats().unwrap();
        assert_eq!(stats.node_count, 2);
        assert_eq!(stats.edge_count, 1);
        assert_eq!(stats.chunk_count, 1);
        assert!(stats.total_tokens > 0, "total_tokens should be non-zero");
    }

    // ── Schemas ───────────────────────────────────────────────────────────────

    #[test]
    fn test_schema_operations() {
        let (storage, _dir) = create_test_storage();

        // Nothing yet.
        assert!(storage.get_schema("default").unwrap().is_none());
        assert!(storage.list_schemas().unwrap().is_empty());

        let schema = crate::schema::SchemaDefinition::create_default();
        storage.save_schema(&schema).unwrap();

        // Round-trip.
        let got = storage.get_schema("default").unwrap().unwrap();
        assert_eq!(got.name, "default");
        assert_eq!(got.version, "1.0.0");

        // List.
        let names = storage.list_schemas().unwrap();
        assert_eq!(names.len(), 1);
        assert_eq!(names[0], "default");

        // Save a second schema and verify both are listed alphabetically.
        let mut schema2 = crate::schema::SchemaDefinition::new(
            "alpha".to_string(),
            "0.1".to_string(),
            "Alpha".to_string(),
        );
        schema2.name = "alpha".to_string();
        storage.save_schema(&schema2).unwrap();
        let names2 = storage.list_schemas().unwrap();
        assert_eq!(names2, vec!["alpha", "default"]);

        // Delete.
        storage.delete_schema("default").unwrap();
        assert!(storage.get_schema("default").unwrap().is_none());

        // Deleting again is a no-op.
        storage.delete_schema("default").unwrap();

        // alpha still present.
        assert!(storage.get_schema("alpha").unwrap().is_some());
    }

    // ── FTS5 full-text search ─────────────────────────────────────────────────

    #[test]
    fn test_search_chunks_fts() {
        let (storage, _dir) = create_test_storage();

        // The FTS index is empty — any query should return nothing.
        assert!(storage.search_chunks_fts("wizard", 10).unwrap().is_empty());

        // Insert a node and a chunk.
        let node = ObjectMetadata::new("character".to_string(), "Gandalf".to_string());
        storage.upsert_node(node.clone()).unwrap();

        let chunk = TextChunk::new(
            node.id,
            "A wise wizard with a grey hat and a long oaken staff.".to_string(),
            ChunkType::Description,
        );
        let chunk_id = chunk.id;
        storage.upsert_chunk(chunk).unwrap();

        // Positive match on a word present in the content.
        let results = storage.search_chunks_fts("wizard", 10).unwrap();
        assert_eq!(results.len(), 1, "expected exactly one FTS result");
        assert_eq!(results[0].0, chunk_id, "chunk ID mismatch");
        assert_eq!(results[0].1, node.id, "object ID mismatch");
        assert!(
            results[0].2.contains("wizard"),
            "content should contain the search term"
        );

        // No match for a word not in the content.
        assert!(storage.search_chunks_fts("vampire", 10).unwrap().is_empty());

        // Insert a second chunk for a different node and verify both match "wise".
        let node2 = ObjectMetadata::new("character".to_string(), "Elrond".to_string());
        storage.upsert_node(node2.clone()).unwrap();
        let chunk2 = TextChunk::new(
            node2.id,
            "A wise elven lord who has seen three ages of the world.".to_string(),
            ChunkType::Description,
        );
        storage.upsert_chunk(chunk2).unwrap();

        let multi = storage.search_chunks_fts("wise", 10).unwrap();
        assert_eq!(multi.len(), 2, "both chunks contain 'wise'");

        // Limit parameter is respected.
        let limited = storage.search_chunks_fts("wise", 1).unwrap();
        assert_eq!(limited.len(), 1, "limit=1 should return at most one result");

        // Prefix query.
        let prefix = storage.search_chunks_fts("wiz*", 10).unwrap();
        assert_eq!(prefix.len(), 1, "prefix 'wiz*' should match 'wizard'");
    }

    // ── BFS subgraph expansion ────────────────────────────────────────────────

    #[test]
    fn test_query_subgraph_two_hops() {
        let (storage, _dir) = create_test_storage();

        // Chain:  Gandalf --knows--> Frodo --ally_of--> Sam
        let gandalf = ObjectMetadata::new("character".to_string(), "Gandalf".to_string());
        let frodo = ObjectMetadata::new("character".to_string(), "Frodo".to_string());
        let sam = ObjectMetadata::new("character".to_string(), "Sam".to_string());
        storage.upsert_node(gandalf.clone()).unwrap();
        storage.upsert_node(frodo.clone()).unwrap();
        storage.upsert_node(sam.clone()).unwrap();

        storage
            .upsert_edge(Edge::new(gandalf.id, frodo.id, EdgeType::from_str("knows")))
            .unwrap();
        storage
            .upsert_edge(Edge::new(frodo.id, sam.id, EdgeType::from_str("ally_of")))
            .unwrap();

        // Add text chunks to verify they are collected.
        let chunk_g = TextChunk::new(
            gandalf.id,
            "Bearer of Narya, one of the three elven rings.".to_string(),
            ChunkType::Description,
        );
        storage.upsert_chunk(chunk_g).unwrap();

        // ── 2-hop BFS from Gandalf ────────────────────────────────────────────
        let result = storage.query_subgraph(gandalf.id, 2).unwrap();

        let obj_ids: HashSet<ObjectId> = result.objects.iter().map(|o| o.id).collect();
        assert_eq!(
            obj_ids.len(),
            3,
            "all three nodes should be reachable in 2 hops"
        );
        assert!(
            obj_ids.contains(&gandalf.id),
            "start node must be in result"
        );
        assert!(obj_ids.contains(&frodo.id), "Frodo is 1 hop away");
        assert!(obj_ids.contains(&sam.id), "Sam is 2 hops away");

        // Edges are deduplicated: 2 unique (source, target, type) triples.
        assert_eq!(result.edges.len(), 2, "expected 2 deduplicated edges");

        // Gandalf's chunk should be present.
        assert_eq!(
            result.chunks.len(),
            1,
            "Gandalf's chunk should be collected"
        );
        assert_eq!(result.total_tokens, result.chunks[0].token_count);

        // ── 0-hop BFS: only the start node ───────────────────────────────────
        let result_0 = storage.query_subgraph(gandalf.id, 0).unwrap();
        assert_eq!(result_0.objects.len(), 1);
        assert_eq!(result_0.objects[0].id, gandalf.id);
        // No edges because we only visit the start node (edges are discovered
        // when a node is processed, but hop-0 only processes Gandalf; its
        // outgoing edge is found, but the loop ends before processing Frodo).
        // Actually at hop 0 we DO add the edge (Gandalf→Frodo) because
        // get_edges(Gandalf) returns it — we just don't visit Frodo.
        assert_eq!(
            result_0.edges.len(),
            1,
            "Gandalf's outgoing edge is added at hop 0"
        );

        // ── 1-hop BFS from Frodo: should reach Gandalf and Sam ───────────────
        let result_1 = storage.query_subgraph(frodo.id, 1).unwrap();
        let ids_1: HashSet<ObjectId> = result_1.objects.iter().map(|o| o.id).collect();
        // hop 0 processes Frodo; discovers Gandalf and Sam.
        // hop 1 processes Gandalf and Sam; their edges are already seen.
        assert!(ids_1.contains(&frodo.id), "Frodo is the start node");
        assert!(ids_1.contains(&gandalf.id), "Gandalf is 1 hop from Frodo");
        assert!(ids_1.contains(&sam.id), "Sam is 1 hop from Frodo");
        assert_eq!(
            result_1.edges.len(),
            2,
            "both edges incident on Frodo are returned"
        );

        // ── Isolated node returns only itself ─────────────────────────────────
        let pippin = ObjectMetadata::new("character".to_string(), "Pippin".to_string());
        storage.upsert_node(pippin.clone()).unwrap();
        let result_iso = storage.query_subgraph(pippin.id, 3).unwrap();
        assert_eq!(result_iso.objects.len(), 1);
        assert!(result_iso.edges.is_empty());
    }

    // ── Semantic (vector) search ──────────────────────────────────────────────

    /// Build a unit-length embedding of `dims` where only dimension `hot_dim`
    /// has a non-zero value (1.0).  Produces vectors that are maximally far
    /// from each other (cosine distance ≈ 1.0) while being trivially correct.
    fn one_hot(hot_dim: usize, dims: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; dims];
        v[hot_dim] = 1.0;
        v
    }

    #[test]
    fn test_upsert_and_search_chunk_embedding() {
        let (storage, _dir) = create_test_storage();

        let node = ObjectMetadata::new("character".to_string(), "Hari Seldon".to_string());
        storage.upsert_node(node.clone()).unwrap();

        let chunk = TextChunk::new(
            node.id,
            "The founder of psychohistory.".to_string(),
            ChunkType::Description,
        );
        let chunk_id = chunk.id;
        storage.upsert_chunk(chunk).unwrap();

        let embedding = one_hot(0, EMBEDDING_DIMENSIONS);
        storage
            .upsert_chunk_embedding(chunk_id, &embedding)
            .unwrap();

        // Querying with the exact same vector → distance should be ~0.
        let results = storage.search_chunks_semantic(&embedding, 5).unwrap();
        assert_eq!(results.len(), 1, "expected exactly one semantic result");
        assert_eq!(results[0].0, chunk_id, "chunk_id mismatch");
        assert_eq!(results[0].1, node.id, "object_id mismatch");
        assert!(
            results[0].2.contains("psychohistory"),
            "content should be returned"
        );
        assert!(
            results[0].3 < 1e-4,
            "cosine distance to self should be ~0.0, got {}",
            results[0].3
        );
    }

    #[test]
    fn test_semantic_search_ranking() {
        let (storage, _dir) = create_test_storage();

        // Three orthogonal one-hot vectors → pairwise cosine distance = 1.0.
        // Query on dim 1 → chunk_b (dim 1) is closest, others are equidistant.
        let node = ObjectMetadata::new("world".to_string(), "Foundation".to_string());
        storage.upsert_node(node.clone()).unwrap();

        let make_chunk =
            |content: &str| TextChunk::new(node.id, content.to_string(), ChunkType::Description);

        let chunk_a = make_chunk("The Galactic Empire spans a million worlds.");
        let chunk_b = make_chunk("Psychohistory predicts the fall of civilisation.");
        let chunk_c = make_chunk("Terminus is a planet on the edge of the galaxy.");

        let id_a = chunk_a.id;
        let id_b = chunk_b.id;
        let id_c = chunk_c.id;

        storage.upsert_chunk(chunk_a).unwrap();
        storage.upsert_chunk(chunk_b).unwrap();
        storage.upsert_chunk(chunk_c).unwrap();

        storage
            .upsert_chunk_embedding(id_a, &one_hot(0, EMBEDDING_DIMENSIONS))
            .unwrap();
        storage
            .upsert_chunk_embedding(id_b, &one_hot(1, EMBEDDING_DIMENSIONS))
            .unwrap();
        storage
            .upsert_chunk_embedding(id_c, &one_hot(2, EMBEDDING_DIMENSIONS))
            .unwrap();

        // Query closest to dim 1 → chunk_b must be first.
        let query = one_hot(1, EMBEDDING_DIMENSIONS);
        let results = storage.search_chunks_semantic(&query, 3).unwrap();
        assert_eq!(results.len(), 3, "all three chunks should be returned");
        assert_eq!(results[0].0, id_b, "chunk_b (dim 1) should rank first");
        // The two more-distant chunks can be in either order; just check they
        // appear in the result set.
        let returned_ids: Vec<_> = results.iter().map(|r| r.0).collect();
        assert!(returned_ids.contains(&id_a));
        assert!(returned_ids.contains(&id_c));
        // Results must be sorted by ascending distance.
        for w in results.windows(2) {
            assert!(
                w[0].3 <= w[1].3,
                "results not sorted by distance: {} > {}",
                w[0].3,
                w[1].3
            );
        }
    }

    #[test]
    fn test_semantic_search_limit_respected() {
        let (storage, _dir) = create_test_storage();

        let node = ObjectMetadata::new("faction".to_string(), "Second Foundation".to_string());
        storage.upsert_node(node.clone()).unwrap();

        for i in 0..5 {
            let chunk = TextChunk::new(
                node.id,
                format!("Chunk number {i}."),
                ChunkType::Description,
            );
            let id = chunk.id;
            storage.upsert_chunk(chunk).unwrap();
            storage
                .upsert_chunk_embedding(id, &one_hot(i, EMBEDDING_DIMENSIONS))
                .unwrap();
        }

        let query = one_hot(0, EMBEDDING_DIMENSIONS);
        let limited = storage.search_chunks_semantic(&query, 2).unwrap();
        assert_eq!(limited.len(), 2, "limit=2 must be respected");
    }

    #[test]
    fn test_upsert_embedding_twice_replaces() {
        let (storage, _dir) = create_test_storage();

        let node = ObjectMetadata::new("item".to_string(), "The Mule".to_string());
        storage.upsert_node(node.clone()).unwrap();

        let chunk = TextChunk::new(
            node.id,
            "A mutant with mental powers.".to_string(),
            ChunkType::Description,
        );
        let chunk_id = chunk.id;
        storage.upsert_chunk(chunk).unwrap();

        // Insert embedding pointing at dim 0, then overwrite pointing at dim 1.
        storage
            .upsert_chunk_embedding(chunk_id, &one_hot(0, EMBEDDING_DIMENSIONS))
            .unwrap();
        storage
            .upsert_chunk_embedding(chunk_id, &one_hot(1, EMBEDDING_DIMENSIONS))
            .unwrap();

        // Querying with dim 1 should find the chunk (updated embedding).
        let results = storage
            .search_chunks_semantic(&one_hot(1, EMBEDDING_DIMENSIONS), 5)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, chunk_id);
        assert!(results[0].3 < 1e-4, "distance should be ~0 after update");

        // Querying with dim 0 should now show a larger distance (not ~0).
        let results_old = storage
            .search_chunks_semantic(&one_hot(0, EMBEDDING_DIMENSIONS), 5)
            .unwrap();
        assert_eq!(results_old.len(), 1);
        assert!(
            results_old[0].3 > 0.5,
            "old embedding direction should now be far away"
        );
    }

    #[test]
    fn test_cascade_delete_removes_vec_entry() {
        let (storage, _dir) = create_test_storage();

        let node = ObjectMetadata::new("location".to_string(), "Trantor".to_string());
        storage.upsert_node(node.clone()).unwrap();

        let chunk = TextChunk::new(
            node.id,
            "The capital world of the Galactic Empire.".to_string(),
            ChunkType::Description,
        );
        let chunk_id = chunk.id;
        storage.upsert_chunk(chunk).unwrap();

        let embedding = one_hot(0, EMBEDDING_DIMENSIONS);
        storage
            .upsert_chunk_embedding(chunk_id, &embedding)
            .unwrap();

        // Verify the chunk is findable before deletion.
        let before = storage.search_chunks_semantic(&embedding, 5).unwrap();
        assert_eq!(
            before.len(),
            1,
            "chunk should be in vec index before delete"
        );

        // Deleting the node cascades to chunks, and the chunks_vec_ad trigger
        // then removes the vec row.
        storage.delete_node(node.id).unwrap();

        // The vec index must now be empty.
        let after = storage.search_chunks_semantic(&embedding, 5).unwrap();
        assert!(
            after.is_empty(),
            "vec entry should be removed after cascade delete"
        );
    }

    #[test]
    fn test_upsert_embedding_nonexistent_chunk_errors() {
        let (storage, _dir) = create_test_storage();

        let phantom_id = Uuid::new_v4();
        let embedding = one_hot(0, EMBEDDING_DIMENSIONS);
        let err = storage
            .upsert_chunk_embedding(phantom_id, &embedding)
            .unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "error should mention 'not found', got: {err}"
        );
    }

    #[test]
    fn test_upsert_embedding_dimension_mismatch_errors() {
        let (storage, _dir) = create_test_storage();

        let node = ObjectMetadata::new("character".to_string(), "R. Daneel".to_string());
        storage.upsert_node(node.clone()).unwrap();
        let chunk = TextChunk::new(node.id, "A robot.".to_string(), ChunkType::Description);
        let chunk_id = chunk.id;
        storage.upsert_chunk(chunk).unwrap();

        // Wrong number of dimensions.
        let bad_embedding = vec![0.5f32; 128];
        let err = storage
            .upsert_chunk_embedding(chunk_id, &bad_embedding)
            .unwrap_err();
        assert!(
            err.to_string().contains("dimension mismatch"),
            "error should mention dimension mismatch, got: {err}"
        );
    }

    #[test]
    fn test_semantic_search_empty_table_returns_empty() {
        let (storage, _dir) = create_test_storage();

        // No embeddings stored at all — must return Ok([]), not an error.
        let query = one_hot(0, EMBEDDING_DIMENSIONS);
        let results = storage.search_chunks_semantic(&query, 10).unwrap();
        assert!(
            results.is_empty(),
            "empty vec table should return empty results"
        );
    }
}
