//! u-forge.ai — Universe Forge
//!
//! Local-first TTRPG worldbuilding tool with AI-powered knowledge graphs.
//!
//! # Architecture
//!
//! The public API centres on [`KnowledgeGraph`], which owns:
//! * [`KnowledgeGraphStorage`] — SQLite-backed persistence (nodes, edges, chunks, schemas,
//!   full-text search via FTS5).
//! * [`SchemaManager`] — runtime schema registration and property validation.
//!
//! Embeddings are handled separately by [`LemonadeProvider`] and are *not* coupled to the
//! storage layer, making it straightforward to use the graph without a running Lemonade
//! Server (e.g. in tests).  AI capabilities are opt-in via [`InferenceQueue`].

#[cfg(test)]
pub(crate) mod test_helpers;

pub mod ai;
pub mod builder;
pub mod config;
pub mod error;
pub mod graph;
pub mod ingest;
pub mod lemonade;
pub mod queue;
pub mod rag;
pub mod schema;
pub mod search;
pub(crate) mod text;
pub mod types;

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use ai::embeddings::{
    EmbeddingModelInfo, EmbeddingProvider, EmbeddingProviderType,
    LemonadeProvider,
};
pub use builder::ObjectBuilder;
pub use config::{AppConfig, ChatConfig, ChatDevice, ChatDeviceConfig, EmbeddingDeviceConfig, ModelConfig, ModelLoadParams};
pub use graph::{
    GraphStats, KnowledgeGraphStorage, DEFAULT_EMBEDDING_CONTEXT_TOKENS, EMBEDDING_DIMENSIONS,
    HIGH_QUALITY_EMBEDDING_DIMENSIONS, MAX_CHUNK_TOKENS,
};
pub use lemonade::{
    ChatChoice, ChatCompletionResponse, ChatMessage, ChatRequest, ChatUsage, StreamToken,
    GpuResourceManager, GpuWorkload, KokoroVoice, LemonadeChatProvider, LemonadeHealth,
    LemonadeSttProvider, LemonadeTtsProvider, LlmGuard, LoadedModelEntry, ModelLoadOptions,
    SttGuard, TranscriptionResult, load_model,
};
pub use ingest::{
    build_hq_embed_queue, embed_all_chunks, setup_and_index, DataIngestion, EmbeddingResult,
    EmbeddingTarget, IngestionStats, SetupResult,
};
pub use schema::{
    EdgeTypeSchema, ObjectTypeSchema, PropertySchema, SchemaDefinition, ValidationResult,
    SchemaIngestion, SchemaManager, SchemaStats,
};
pub use rag::{build_rag_messages, format_search_context, RagContext};
pub use search::{
    search_hybrid, ConnectedNode, HybridSearchConfig, NodeSearchResult, SearchSources,
};
pub use types::*;

// ── Facade ────────────────────────────────────────────────────────────────────

use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use text::split_text;

/// Central knowledge graph interface.
///
/// Composes storage and schema management.  Embedding / vector search are
/// intentionally *not* members of this struct — they are opt-in via
/// [`InferenceQueue`](queue::InferenceQueue) so that the graph can be used
/// synchronously in tests and CLI tooling without a running Lemonade Server.
///
/// # Example
/// ```no_run
/// use u_forge_core::{KnowledgeGraph, ObjectBuilder};
/// let graph = KnowledgeGraph::new("./data/kg").unwrap();
/// let id = ObjectBuilder::character("Gandalf".to_string())
///     .with_description("A wizard of great power".to_string())
///     .add_to_graph(&graph)
///     .unwrap();
/// ```
// KnowledgeGraph is Send + Sync:
//   - KnowledgeGraphStorage wraps rusqlite::Connection in Arc<parking_lot::Mutex<Connection>> (see graph/storage.rs)
//   - SchemaManager holds Arc<KnowledgeGraphStorage> + DashMap (both Send + Sync)
// This means Arc<KnowledgeGraph> is a valid axum State<T> type for Phase 3.
pub struct KnowledgeGraph {
    storage: Arc<KnowledgeGraphStorage>,
    schema_manager: Arc<SchemaManager>,
}

impl KnowledgeGraph {
    /// Open (or create) a knowledge graph at `db_path`.
    ///
    /// `db_path` should be a directory; the SQLite file is created at
    /// `<db_path>/knowledge.db`.
    pub fn new<P: AsRef<Path>>(db_path: P) -> Result<Self> {
        let storage = Arc::new(KnowledgeGraphStorage::new(db_path.as_ref())?);
        let schema_manager = Arc::new(SchemaManager::new(storage.clone()));
        Ok(Self {
            storage,
            schema_manager,
        })
    }

    // ── Node / object operations ──────────────────────────────────────────────

    /// Persist a new object, returning its [`ObjectId`].
    pub fn add_object(&self, metadata: ObjectMetadata) -> Result<ObjectId> {
        let id = metadata.id;
        self.storage.upsert_node(metadata)?;
        Ok(id)
    }

    /// Retrieve an object by its [`ObjectId`], or `None` if it does not exist.
    pub fn get_object(&self, id: ObjectId) -> Result<Option<ObjectMetadata>> {
        self.storage.get_node(id)
    }

    /// Return every object stored in the graph.
    pub fn get_all_objects(&self) -> Result<Vec<ObjectMetadata>> {
        self.storage.get_all_objects()
    }

    /// Overwrite an existing object's metadata (updates `updated_at`).
    pub fn update_object(&self, mut metadata: ObjectMetadata) -> Result<()> {
        metadata.touch();
        self.storage.upsert_node(metadata)
    }

    /// Delete an object and, via `ON DELETE CASCADE`, all its edges and chunks.
    pub fn delete_object(&self, id: ObjectId) -> Result<()> {
        self.storage.delete_node(id)
    }

    /// Delete all data from the graph (nodes, edges, chunks, schemas, vectors).
    pub fn clear_all(&self) -> Result<()> {
        self.storage.clear_all()
    }

    // ── Edge / relationship operations ────────────────────────────────────────

    /// Create a typed relationship between two objects.
    pub fn connect_objects(&self, from: ObjectId, to: ObjectId, edge_type: EdgeType) -> Result<()> {
        self.storage.upsert_edge(Edge::new(from, to, edge_type))
    }

    /// Create a relationship using a plain string edge type.
    pub fn connect_objects_str(&self, from: ObjectId, to: ObjectId, edge_type: &str) -> Result<()> {
        self.storage
            .upsert_edge(Edge::new(from, to, EdgeType::new(edge_type)))
    }

    /// Create a weighted relationship.
    pub fn connect_objects_weighted(
        &self,
        from: ObjectId,
        to: ObjectId,
        edge_type: EdgeType,
        weight: f32,
    ) -> Result<()> {
        self.storage
            .upsert_edge(Edge::new(from, to, edge_type).with_weight(weight))
    }

    /// Create a weighted relationship using a plain string edge type.
    pub fn connect_objects_weighted_str(
        &self,
        from: ObjectId,
        to: ObjectId,
        edge_type: &str,
        weight: f32,
    ) -> Result<()> {
        self.storage
            .upsert_edge(Edge::new(from, to, EdgeType::new(edge_type)).with_weight(weight))
    }

    /// All edges incident to `id` (both outgoing and incoming).
    pub fn get_relationships(&self, id: ObjectId) -> Result<Vec<Edge>> {
        self.storage.get_edges(id)
    }

    /// Format all edges incident on `node` as human-readable `"From edgeType To"` strings.
    ///
    /// Endpoint names are resolved by looking up the connected node; edges
    /// whose endpoints cannot be resolved are silently dropped.
    pub fn edge_display_lines(&self, node: &ObjectMetadata) -> Vec<String> {
        self.get_relationships(node.id)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| {
                let from = if e.from == node.id {
                    node.name.clone()
                } else {
                    self.get_object(e.from).ok().flatten()?.name
                };
                let to = if e.to == node.id {
                    node.name.clone()
                } else {
                    self.get_object(e.to).ok().flatten()?.name
                };
                Some(format!("{} {} {}", from, e.edge_type.as_str(), to))
            })
            .collect()
    }

    /// Return every edge in the graph in a single query.
    ///
    /// Prefer this over repeated `get_relationships()` calls when building a
    /// full graph snapshot.
    pub fn get_all_edges(&self) -> Result<Vec<Edge>> {
        self.storage.get_all_edges()
    }

    /// Return a page of nodes ordered by name.
    ///
    /// Use for incremental full-graph snapshots without loading all nodes at once.
    pub fn get_nodes_paginated(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<ObjectMetadata>> {
        self.storage.get_nodes_paginated(offset, limit)
    }

    /// IDs of every object directly connected to `id` (1-hop neighbours).
    pub fn get_neighbors(&self, id: ObjectId) -> Result<Vec<ObjectId>> {
        self.storage.get_neighbors(id)
    }

    // ── Chunk / text operations ───────────────────────────────────────────────

    /// Attach text to an object, splitting into ≤[`MAX_CHUNK_TOKENS`] pieces at
    /// word boundaries as needed.
    ///
    /// Each piece is stored as a separate [`TextChunk`] row, FTS5-indexed
    /// automatically via the `chunks_ai` trigger.  Returns the [`ChunkId`] of
    /// every piece created, in order.  The vast majority of calls produce a
    /// single-element `Vec`; splitting only occurs when `content` exceeds
    /// `MAX_CHUNK_TOKENS` (currently 500 tokens ≈ 1 500 characters).
    pub fn add_text_chunk(
        &self,
        object_id: ObjectId,
        content: String,
        chunk_type: ChunkType,
    ) -> Result<Vec<ChunkId>> {
        let pieces = split_text(&content);
        let mut ids = Vec::with_capacity(pieces.len());
        for piece in pieces {
            let chunk = TextChunk::new(object_id, piece, chunk_type.clone());
            ids.push(chunk.id);
            self.storage.upsert_chunk(chunk)?;
        }
        Ok(ids)
    }

    /// Attach a pre-embedded text chunk to an object in one call.
    ///
    /// Because the caller supplies a single pre-computed embedding vector, the
    /// content must fit within a single chunk (≤ [`MAX_CHUNK_TOKENS`] tokens).
    /// Use [`add_text_chunk`](Self::add_text_chunk) followed by
    /// [`upsert_chunk_embedding`](Self::upsert_chunk_embedding) for content that
    /// may need splitting.
    ///
    /// Returns an error if `content` would be split into more than one chunk or
    /// if `embedding.len() != EMBEDDING_DIMENSIONS`.
    pub fn add_text_chunk_with_embedding(
        &self,
        object_id: ObjectId,
        content: String,
        chunk_type: ChunkType,
        embedding: &[f32],
    ) -> Result<ChunkId> {
        let pieces = split_text(&content);
        if pieces.len() > 1 {
            return Err(anyhow::anyhow!(
                "add_text_chunk_with_embedding: content splits into {} chunks \
                 (max tokens per chunk: {}). Use add_text_chunk + upsert_chunk_embedding \
                 for long content.",
                pieces.len(),
                MAX_CHUNK_TOKENS,
            ));
        }
        let text = pieces.into_iter().next().unwrap_or_default();
        let chunk = TextChunk::new(object_id, text, chunk_type);
        let chunk_id = chunk.id;
        self.storage.upsert_chunk(chunk)?;
        self.storage.upsert_chunk_embedding(chunk_id, embedding)?;
        Ok(chunk_id)
    }

    /// Store or update the embedding vector for an existing chunk.
    ///
    /// The chunk must already exist (created via [`add_text_chunk`](Self::add_text_chunk)
    /// or [`add_text_chunk_with_embedding`](Self::add_text_chunk_with_embedding)).
    /// `embedding.len()` must equal [`EMBEDDING_DIMENSIONS`] (currently 256).
    pub fn upsert_chunk_embedding(&self, chunk_id: ChunkId, embedding: &[f32]) -> Result<()> {
        self.storage.upsert_chunk_embedding(chunk_id, embedding)
    }

    /// All text chunks belonging to `object_id`.
    pub fn get_text_chunks(&self, object_id: ObjectId) -> Result<Vec<TextChunk>> {
        self.storage.get_chunks_for_node(object_id)
    }

    /// All chunks that have no 768-dim embedding in `chunks_vec` yet.
    ///
    /// Use this for incremental embedding passes: only process what's new
    /// rather than re-embedding the entire graph on each call.
    pub fn get_unembedded_chunks(&self) -> Result<Vec<TextChunk>> {
        self.storage.get_unembedded_chunks()
    }

    /// All chunks that have no 4096-dim embedding in `chunks_vec_hq` yet.
    pub fn get_unembedded_chunks_hq(&self) -> Result<Vec<TextChunk>> {
        self.storage.get_unembedded_chunks_hq()
    }

    // ── Search ────────────────────────────────────────────────────────────────

    /// Exact name lookup scoped to a single object type.
    pub fn find_by_name(&self, object_type: &str, name: &str) -> Result<Vec<ObjectMetadata>> {
        self.storage.find_nodes_by_name(object_type, name)
    }

    /// Exact name lookup across **all** object types.
    ///
    /// O(log N) via the `idx_nodes_name_only` index — slower than
    /// [`find_by_name`](Self::find_by_name) but useful when the type is unknown
    /// (e.g. cross-session edge resolution, BUG-7 fix).
    pub fn find_by_name_only(&self, name: &str) -> Result<Vec<ObjectMetadata>> {
        self.storage.find_nodes_by_name_only(name)
    }

    /// Full-text search over chunk content using SQLite FTS5.
    ///
    /// `query` accepts the full FTS5 query syntax (phrase, prefix, boolean, etc.).
    /// Returns at most `limit` results as `(chunk_id, object_id, content)` tuples,
    /// ordered by relevance rank.
    pub fn search_chunks_fts(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(ChunkId, ObjectId, String)>> {
        self.storage.search_chunks_fts(query, limit)
    }

    /// Approximate nearest-neighbour search over stored chunk embeddings.
    ///
    /// Queries the `chunks_vec` sqlite-vec virtual table for the `limit` closest
    /// chunks to `query_embedding` by cosine distance.  Only chunks that have
    /// been indexed via [`upsert_chunk_embedding`](Self::upsert_chunk_embedding)
    /// or [`add_text_chunk_with_embedding`](Self::add_text_chunk_with_embedding)
    /// are candidates — unembedded chunks are invisible to this method.
    ///
    /// Returns `(chunk_id, object_id, content, distance)` tuples ordered by
    /// ascending cosine distance (`0.0` = identical, `2.0` = maximally
    /// dissimilar).  Returns an empty `Vec` (not an error) when no embeddings
    /// are stored yet.
    ///
    /// `query_embedding.len()` must equal [`EMBEDDING_DIMENSIONS`] (currently 768).
    pub fn search_chunks_semantic(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<(ChunkId, ObjectId, String, f32)>> {
        self.storage.search_chunks_semantic(query_embedding, limit)
    }

    // ── High-quality (4096-dim) embedding methods ────────────────────────────

    /// Store or update the high-quality embedding vector for an existing chunk.
    ///
    /// Writes to the `chunks_vec_hq` (4096-dim) index.
    /// `embedding.len()` must equal [`HIGH_QUALITY_EMBEDDING_DIMENSIONS`].
    pub fn upsert_chunk_embedding_hq(&self, chunk_id: ChunkId, embedding: &[f32]) -> Result<()> {
        self.storage.upsert_chunk_embedding_hq(chunk_id, embedding)
    }

    /// Approximate nearest-neighbour search over the high-quality embedding index.
    ///
    /// Queries `chunks_vec_hq` (4096-dim) instead of `chunks_vec` (768-dim).
    pub fn search_chunks_semantic_hq(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<(ChunkId, ObjectId, String, f32)>> {
        self.storage.search_chunks_semantic_hq(query_embedding, limit)
    }

    // ── Graph traversal ───────────────────────────────────────────────────────

    /// BFS subgraph rooted at `start`, expanding up to `max_hops` hops.
    pub fn query_subgraph(&self, start: ObjectId, max_hops: usize) -> Result<QueryResult> {
        self.storage.query_subgraph(start, max_hops)
    }

    // ── Statistics ────────────────────────────────────────────────────────────

    /// Counts of nodes, edges, chunks, and total tokens.  O(1) via SQL aggregates.
    pub fn get_stats(&self) -> Result<GraphStats> {
        self.storage.get_stats()
    }

    // ── Layout persistence ────────────────────────────────────────────────────

    /// Persist canvas positions for the graph-view UI.
    ///
    /// `positions` is a slice of `(node_id, x, y)` triples.  Each call is an
    /// upsert — existing rows are updated in place.
    pub fn save_layout(&self, positions: &[(ObjectId, f32, f32)]) -> Result<()> {
        self.storage.save_layout(positions)
    }

    /// Load all previously saved canvas positions as an `ObjectId → (x, y)` map.
    ///
    /// Returns an empty map when no positions have been saved yet.
    pub fn load_layout(&self) -> Result<HashMap<ObjectId, (f32, f32)>> {
        self.storage.load_layout()
    }

    // ── Schema ────────────────────────────────────────────────────────────────

    /// Access the underlying [`SchemaManager`].
    pub fn get_schema_manager(&self) -> Arc<SchemaManager> {
        self.schema_manager.clone()
    }

    /// Validate `object` against its registered schema.
    pub async fn validate_object(&self, object: &ObjectMetadata) -> Result<ValidationResult> {
        self.schema_manager.validate_object(object).await
    }

    /// Persist `metadata` only if it passes schema validation.
    pub async fn add_object_validated(&self, metadata: ObjectMetadata) -> Result<ObjectId> {
        let result = self.validate_object(&metadata).await?;
        if !result.valid {
            return Err(anyhow::anyhow!(
                "Object validation failed: {:?}",
                result.errors
            ));
        }
        let id = metadata.id;
        self.storage.upsert_node(metadata)?;
        Ok(id)
    }

    /// Register a new object type in the `"default"` schema.
    pub async fn register_object_type(
        &self,
        type_name: &str,
        type_schema: ObjectTypeSchema,
    ) -> Result<()> {
        self.schema_manager
            .register_object_type("default", type_name, type_schema)
            .await
    }

    /// Register a new edge type in the `"default"` schema.
    pub async fn register_edge_type(
        &self,
        edge_name: &str,
        edge_schema: EdgeTypeSchema,
    ) -> Result<()> {
        self.schema_manager
            .register_edge_type("default", edge_name, edge_schema)
            .await
    }

    /// Schema-level statistics for the named schema.
    pub async fn get_schema_stats(&self, schema_name: &str) -> Result<SchemaStats> {
        self.schema_manager.get_schema_stats(schema_name).await
    }

    /// Names of all schemas currently persisted.
    pub fn list_schemas(&self) -> Result<Vec<String>> {
        self.schema_manager.list_schemas()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
