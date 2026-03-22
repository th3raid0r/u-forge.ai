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
//! Embeddings are handled separately by [`EmbeddingManager`] / [`LemonadeProvider`] and
//! are *not* coupled to the storage layer, making it straightforward to use the graph
//! without a running Lemonade Server (e.g. in tests).

#[cfg(test)]
pub(crate) mod test_helpers;

pub mod data_ingestion;
pub mod embedding_queue;
pub mod embeddings;
pub mod hardware;
pub mod inference_queue;
pub mod lemonade;
pub mod schema;
pub mod schema_ingestion;
pub mod schema_manager;
pub mod storage;
pub mod transcription;
pub mod types;

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use embedding_queue::{
    EmbeddingProgress, EmbeddingQueue, EmbeddingQueueBuilder, RequestStatus,
};
pub use embeddings::{
    EmbeddingManager, EmbeddingModelInfo, EmbeddingProvider, EmbeddingProviderType,
    LemonadeProvider,
};
pub use lemonade::{
    ChatChoice, ChatCompletionResponse, ChatMessage, ChatRequest, ChatUsage, GpuResourceManager,
    GpuWorkload, KokoroVoice, LemonadeChatProvider, LemonadeModelEntry, LemonadeModelRegistry,
    LemonadeStack, LemonadeSttProvider, LemonadeTtsProvider, LlmGuard, ModelRole, SttGuard,
    TranscriptionResult,
};
pub use schema::{
    EdgeTypeSchema, ObjectTypeSchema, PropertySchema, SchemaDefinition, ValidationResult,
};
pub use schema_ingestion::SchemaIngestion;
pub use schema_manager::{SchemaManager, SchemaStats};
pub use storage::{
    GraphStats, KnowledgeGraphStorage, EMBEDDING_DIMENSIONS, ENABLE_HIGH_QUALITY_EMBEDDING,
    MAX_CHUNK_TOKENS,
};
pub use types::*;

// ── Facade ────────────────────────────────────────────────────────────────────

use anyhow::Result;
use std::path::Path;
use std::sync::Arc;

// ── Text splitting ────────────────────────────────────────────────────────────

/// Split `text` into pieces of at most [`MAX_CHUNK_TOKENS`] tokens, breaking
/// only at whitespace boundaries.
///
/// Uses the same `len / 4` heuristic as [`estimate_token_count`] so that the
/// token budget is always consistent with what is stored in [`TextChunk::token_count`].
///
/// [`estimate_token_count`]: crate::types
fn split_text(text: &str) -> Vec<String> {
    // 4 chars per token mirrors estimate_token_count in types.rs.
    let max_chars = MAX_CHUNK_TOKENS * 4;

    if text.len() <= max_chars {
        let trimmed = text.trim().to_string();
        return if trimmed.is_empty() {
            vec![]
        } else {
            vec![trimmed]
        };
    }

    let mut pieces: Vec<String> = Vec::new();
    let mut remaining = text.trim();

    while !remaining.is_empty() {
        if remaining.len() <= max_chars {
            pieces.push(remaining.to_string());
            break;
        }

        // Find the last whitespace at or before the max_chars boundary so we
        // never cut through a word.
        let boundary = &remaining[..max_chars];
        let split_at = boundary.rfind(char::is_whitespace).unwrap_or(max_chars); // no whitespace found → hard cut

        let piece = remaining[..split_at].trim().to_string();
        if !piece.is_empty() {
            pieces.push(piece);
        }

        // Advance past the split point, skipping leading whitespace.
        remaining = remaining[split_at..].trim_start();
    }

    pieces
}

/// Central knowledge graph interface.
///
/// Composes storage and schema management.  Embedding / vector search are
/// intentionally *not* members of this struct — they are opt-in via
/// [`EmbeddingManager`] so that the graph can be used synchronously in tests
/// and CLI tooling without a running Lemonade Server.
///
/// # Example
/// ```no_run
/// use u_forge_ai::{KnowledgeGraph, ObjectBuilder};
/// let graph = KnowledgeGraph::new("./data/kg").unwrap();
/// let id = ObjectBuilder::character("Gandalf".to_string())
///     .with_description("A wizard of great power".to_string())
///     .add_to_graph(&graph)
///     .unwrap();
/// ```
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

    // ── Edge / relationship operations ────────────────────────────────────────

    /// Create a typed relationship between two objects.
    pub fn connect_objects(&self, from: ObjectId, to: ObjectId, edge_type: EdgeType) -> Result<()> {
        self.storage.upsert_edge(Edge::new(from, to, edge_type))
    }

    /// Create a relationship using a plain string edge type.
    pub fn connect_objects_str(&self, from: ObjectId, to: ObjectId, edge_type: &str) -> Result<()> {
        self.storage
            .upsert_edge(Edge::new(from, to, EdgeType::from_str(edge_type)))
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
            .upsert_edge(Edge::new(from, to, EdgeType::from_str(edge_type)).with_weight(weight))
    }

    /// All edges incident to `id` (both outgoing and incoming).
    pub fn get_relationships(&self, id: ObjectId) -> Result<Vec<Edge>> {
        self.storage.get_edges(id)
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
    /// `MAX_CHUNK_TOKENS` (currently 500 tokens ≈ 2 000 characters).
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
    /// `query_embedding.len()` must equal [`EMBEDDING_DIMENSIONS`] (currently 256).
    pub fn search_chunks_semantic(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<(ChunkId, ObjectId, String, f32)>> {
        self.storage.search_chunks_semantic(query_embedding, limit)
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
    pub async fn list_schemas(&self) -> Result<Vec<String>> {
        self.schema_manager.list_schemas().await
    }
}

// ── ObjectBuilder ─────────────────────────────────────────────────────────────

/// Fluent builder for constructing [`ObjectMetadata`] with TTRPG-friendly
/// convenience constructors.
///
/// # Example
/// ```no_run
/// use u_forge_ai::ObjectBuilder;
/// let obj = ObjectBuilder::character("Gandalf".to_string())
///     .with_description("A wizard".to_string())
///     .with_property("race".to_string(), "Maiar".to_string())
///     .with_tag("wizard".to_string())
///     .build();
/// ```
pub struct ObjectBuilder {
    metadata: ObjectMetadata,
}

impl ObjectBuilder {
    pub fn character(name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new("character".to_string(), name),
        }
    }

    pub fn location(name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new("location".to_string(), name),
        }
    }

    pub fn faction(name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new("faction".to_string(), name),
        }
    }

    pub fn item(name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new("item".to_string(), name),
        }
    }

    pub fn event(name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new("event".to_string(), name),
        }
    }

    pub fn session(name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new("session".to_string(), name),
        }
    }

    pub fn custom(object_type: String, name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new(object_type, name),
        }
    }

    pub fn with_description(mut self, description: String) -> Self {
        self.metadata.description = Some(description);
        self
    }

    pub fn with_property(mut self, key: String, value: String) -> Self {
        self.metadata = self.metadata.with_property(key, value);
        self
    }

    pub fn with_json_property(mut self, key: String, value: serde_json::Value) -> Self {
        self.metadata = self.metadata.with_json_property(key, value);
        self
    }

    pub fn with_tag(mut self, tag: String) -> Self {
        self.metadata.add_tag(tag);
        self
    }

    /// Consume the builder and return the finished [`ObjectMetadata`].
    pub fn build(self) -> ObjectMetadata {
        self.metadata
    }

    /// Build and immediately insert into `graph`.  Returns the new [`ObjectId`].
    pub fn add_to_graph(self, graph: &KnowledgeGraph) -> Result<ObjectId> {
        graph.add_object(self.build())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_graph() -> (KnowledgeGraph, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let graph = KnowledgeGraph::new(temp_dir.path()).unwrap();
        (graph, temp_dir)
    }

    async fn create_test_graph_async() -> (KnowledgeGraph, TempDir) {
        create_test_graph()
    }

    // ── Basic CRUD ────────────────────────────────────────────────────────────

    #[test]
    fn test_basic_graph_operations() {
        let (graph, _tmp) = create_test_graph();

        let gandalf_id = ObjectBuilder::character("Gandalf".to_string())
            .with_description("A wise wizard of great power".to_string())
            .with_property("race".to_string(), "Maiar".to_string())
            .with_tag("wizard".to_string())
            .add_to_graph(&graph)
            .unwrap();

        let frodo_id = ObjectBuilder::character("Frodo Baggins".to_string())
            .with_description("A brave hobbit from the Shire".to_string())
            .with_property("race".to_string(), "Hobbit".to_string())
            .with_tag("ringbearer".to_string())
            .add_to_graph(&graph)
            .unwrap();

        graph
            .connect_objects_str(gandalf_id, frodo_id, "knows")
            .unwrap();

        let gandalf = graph.get_object(gandalf_id).unwrap().unwrap();
        assert_eq!(gandalf.name, "Gandalf");
        assert_eq!(gandalf.object_type, "character");

        let frodo = graph.get_object(frodo_id).unwrap().unwrap();
        assert_eq!(frodo.name, "Frodo Baggins");

        // Relationship
        let rels = graph.get_relationships(gandalf_id).unwrap();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].to, frodo_id);
        assert_eq!(rels[0].edge_type, EdgeType::from_str("knows"));

        // Neighbours
        let neighbours = graph.get_neighbors(gandalf_id).unwrap();
        assert_eq!(neighbours.len(), 1);
        assert_eq!(neighbours[0], frodo_id);

        // Text chunk
        let chunk_ids = graph
            .add_text_chunk(
                gandalf_id,
                "Gandalf appeared at Bilbo's birthday party.".to_string(),
                ChunkType::UserNote,
            )
            .unwrap();
        assert_eq!(chunk_ids.len(), 1);
        let chunk_id = chunk_ids[0];
        let chunks = graph.get_text_chunks(gandalf_id).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].id, chunk_id);

        // Subgraph
        let sg = graph.query_subgraph(gandalf_id, 1).unwrap();
        assert_eq!(sg.objects.len(), 2);
        assert_eq!(sg.edges.len(), 1);
        assert_eq!(sg.chunks.len(), 1);

        // Stats
        let stats = graph.get_stats().unwrap();
        assert_eq!(stats.node_count, 2);
        assert_eq!(stats.edge_count, 1);
        assert_eq!(stats.chunk_count, 1);
        assert!(stats.total_tokens > 0);
    }

    #[test]
    fn test_find_by_name() {
        let (graph, _tmp) = create_test_graph();
        ObjectBuilder::character("Gandalf".to_string())
            .add_to_graph(&graph)
            .unwrap();

        let found = graph.find_by_name("character", "Gandalf").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Gandalf");

        // find_by_name_only (type-agnostic)
        let found_any = graph.find_by_name_only("Gandalf").unwrap();
        assert_eq!(found_any.len(), 1);
    }

    #[test]
    fn test_weighted_relationships() {
        let (graph, _tmp) = create_test_graph();

        let sauron_id = ObjectBuilder::character("Sauron".to_string())
            .add_to_graph(&graph)
            .unwrap();
        let frodo_id = ObjectBuilder::character("Frodo".to_string())
            .add_to_graph(&graph)
            .unwrap();

        graph
            .connect_objects_weighted_str(sauron_id, frodo_id, "enemy_of", 0.9)
            .unwrap();

        let rels = graph.get_relationships(sauron_id).unwrap();
        assert_eq!(rels.len(), 1);
        assert!((rels[0].weight - 0.9).abs() < 1e-6);
        assert_eq!(rels[0].edge_type, EdgeType::from_str("enemy_of"));
    }

    #[test]
    fn test_complex_world_scenario() {
        let (graph, _tmp) = create_test_graph();

        let shire_id = ObjectBuilder::location("The Shire".to_string())
            .add_to_graph(&graph)
            .unwrap();
        let bag_end_id = ObjectBuilder::location("Bag End".to_string())
            .add_to_graph(&graph)
            .unwrap();
        let frodo_id = ObjectBuilder::character("Frodo Baggins".to_string())
            .add_to_graph(&graph)
            .unwrap();
        let ring_id = ObjectBuilder::item("The One Ring".to_string())
            .add_to_graph(&graph)
            .unwrap();
        let fellowship_id = ObjectBuilder::faction("Fellowship of the Ring".to_string())
            .add_to_graph(&graph)
            .unwrap();

        graph
            .connect_objects_str(bag_end_id, shire_id, "located_in")
            .unwrap();
        graph
            .connect_objects_str(frodo_id, bag_end_id, "located_in")
            .unwrap();
        graph
            .connect_objects_str(frodo_id, ring_id, "owned_by")
            .unwrap();
        graph
            .connect_objects_str(frodo_id, fellowship_id, "member_of")
            .unwrap();

        let frodo_world = graph.query_subgraph(frodo_id, 2).unwrap();
        assert_eq!(frodo_world.objects.len(), 5);
        assert!(frodo_world.edges.len() >= 4);

        let stats = graph.get_stats().unwrap();
        assert_eq!(stats.node_count, 5);
        assert_eq!(stats.edge_count, 4);
    }

    #[test]
    fn test_fts_search() {
        let (graph, _tmp) = create_test_graph();

        let obj_id = ObjectBuilder::character("Saruman".to_string())
            .add_to_graph(&graph)
            .unwrap();

        graph
            .add_text_chunk(
                obj_id,
                "Saruman the White was the head of the Istari order.".to_string(),
                ChunkType::Description,
            )
            .unwrap();
        // add_text_chunk returns Vec<ChunkId>; short content → single chunk.

        // FTS5 exact-word search
        let results = graph.search_chunks_fts("Istari", 5).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1, obj_id);
        assert!(results[0].2.contains("Istari"));

        // No match
        let empty = graph.search_chunks_fts("dragon", 5).unwrap();
        assert!(empty.is_empty());
    }

    // ── split_text ────────────────────────────────────────────────────────────

    #[test]
    fn test_split_text_short_content_is_not_split() {
        // Content well under MAX_CHUNK_TOKENS (500 tokens = 2000 chars).
        let pieces = split_text("A short description.");
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0], "A short description.");
    }

    #[test]
    fn test_split_text_empty_and_whitespace_produce_no_pieces() {
        assert!(split_text("").is_empty());
        assert!(split_text("   \n\t  ").is_empty());
    }

    #[test]
    fn test_split_text_exact_boundary_is_not_split() {
        // A string of exactly MAX_CHUNK_TOKENS * 4 chars must NOT be split.
        let max_chars = MAX_CHUNK_TOKENS * 4;
        let content = "a".repeat(max_chars);
        let pieces = split_text(&content);
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].len(), max_chars);
    }

    #[test]
    fn test_split_text_long_content_splits_on_word_boundary() {
        // Build a string of 10 words each 399 chars + 1 space = 4000 chars total
        // (≈ 1000 tokens).  With a 2000-char window the split should fall at the
        // space between words 5 and 6, keeping pieces well under the limit.
        let word = "x".repeat(399);
        let content = (0..10).map(|_| word.as_str()).collect::<Vec<_>>().join(" ");
        assert!(
            content.len() > 2000,
            "pre-condition: content must exceed limit"
        );

        let pieces = split_text(&content);
        assert!(pieces.len() >= 2, "long content must be split");
        for piece in &pieces {
            // Each piece must be within the 2000-char budget.
            assert!(
                piece.len() <= 2000,
                "piece too long ({} chars): {:?}",
                piece.len(),
                &piece[..piece.len().min(40)]
            );
            // No piece should be empty.
            assert!(!piece.is_empty());
        }
        // Reassembling (with a space) must cover all original words.
        let rejoined = pieces.join(" ");
        let original_words: Vec<_> = content.split_whitespace().collect();
        let rejoined_words: Vec<_> = rejoined.split_whitespace().collect();
        assert_eq!(original_words, rejoined_words);
    }

    #[test]
    fn test_split_text_hard_cut_when_no_whitespace() {
        // A single 4000-char token-free string (no spaces): must be hard-cut
        // at max_chars since there is no whitespace boundary to use.
        let content = "z".repeat(4000);
        let pieces = split_text(&content);
        assert!(
            pieces.len() >= 2,
            "must hard-cut oversized no-whitespace content"
        );
        for piece in &pieces {
            assert!(piece.len() <= 2000);
            assert!(!piece.is_empty());
        }
    }

    #[test]
    fn test_split_text_leading_trailing_whitespace_is_trimmed() {
        let pieces = split_text("  hello world  ");
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0], "hello world");
    }

    #[test]
    fn test_add_text_chunk_long_content_stored_as_multiple_chunks() {
        let (graph, _tmp) = create_test_graph();
        let obj_id = ObjectBuilder::character("Verbose".to_string())
            .add_to_graph(&graph)
            .unwrap();

        // 5000-char content: ~1250 tokens → must split into at least 3 chunks
        // (ceiling of 1250 / 500 = 3).
        let long_content = "word ".repeat(1000); // 5000 chars
        let chunk_ids = graph
            .add_text_chunk(obj_id, long_content.clone(), ChunkType::Description)
            .unwrap();

        assert!(
            chunk_ids.len() >= 3,
            "expected ≥3 chunks for 5000-char content, got {}",
            chunk_ids.len()
        );

        // All chunks must be retrievable and within the token budget.
        let stored = graph.get_text_chunks(obj_id).unwrap();
        assert_eq!(stored.len(), chunk_ids.len());
        for chunk in &stored {
            assert!(
                chunk.token_count <= MAX_CHUNK_TOKENS,
                "chunk exceeds MAX_CHUNK_TOKENS: {} tokens",
                chunk.token_count
            );
        }

        // The concatenated content must cover all original words (modulo
        // trimming at boundaries).
        let original_words: Vec<_> = long_content.split_whitespace().collect();
        let stored_words: Vec<_> = stored
            .iter()
            .flat_map(|c| c.content.split_whitespace())
            .collect();
        assert_eq!(
            original_words, stored_words,
            "stored chunks must cover all original words in order"
        );
    }

    // ── Schema integration ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_schema_integration() {
        let (graph, _tmp) = create_test_graph_async().await;

        let spell_schema =
            ObjectTypeSchema::new("spell".to_string(), "A magical spell".to_string())
                .with_property("level".to_string(), PropertySchema::number("Spell level"))
                .with_property(
                    "school".to_string(),
                    PropertySchema::string("School of magic"),
                )
                .with_required_property("level".to_string());

        graph
            .register_object_type("spell", spell_schema)
            .await
            .unwrap();

        let spell = ObjectBuilder::custom("spell".to_string(), "Fireball".to_string())
            .with_json_property(
                "level".to_string(),
                serde_json::Value::Number(serde_json::Number::from(3)),
            )
            .with_json_property(
                "school".to_string(),
                serde_json::Value::String("Evocation".to_string()),
            )
            .build();

        let validation = graph.validate_object(&spell).await.unwrap();
        assert!(
            validation.valid,
            "Expected valid spell: {:?}",
            validation.errors
        );

        let spell_id = graph.add_object_validated(spell).await.unwrap();
        let retrieved = graph.get_object(spell_id).unwrap().unwrap();
        assert_eq!(retrieved.name, "Fireball");
        assert_eq!(retrieved.object_type, "spell");

        let stats = graph.get_schema_stats("default").await.unwrap();
        assert!(stats.object_type_count >= 7); // 6 built-in + "spell"
    }

    #[tokio::test]
    async fn test_validation_failure() {
        let (graph, _tmp) = create_test_graph_async().await;

        let bad = ObjectMetadata::new("unknown_type_xyz".to_string(), "Test".to_string());
        let result = graph.validate_object(&bad).await.unwrap();
        assert!(!result.valid);
        assert!(!result.errors.is_empty());

        let insert_result = graph.add_object_validated(bad).await;
        assert!(insert_result.is_err());
    }
}
