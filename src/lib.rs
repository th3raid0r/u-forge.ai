//! u-forge.ai - Universe Forge
//! 
//! A local-first TTRPG worldbuilding application with AI-powered knowledge graphs.
//! 
//! This crate provides the core storage and graph processing functionality for
//! managing interconnected worldbuilding data including characters, locations,
//! factions, items, events, and session notes.

pub mod types;
pub mod storage;
pub mod embeddings; // Added embeddings module
pub mod vector_search; // Added vector_search module
pub mod embedding_queue; // Added embedding_queue module
pub mod schema; // Added schema module
pub mod schema_manager; // Added schema_manager module
pub mod schema_ingestion; // Added schema_ingestion module
pub mod data_ingestion; // Added data_ingestion module

pub use types::*;
pub use storage::KnowledgeGraphStorage;
pub use embeddings::{EmbeddingManager, EmbeddingProvider, FastEmbedModel}; // Re-export key embedding types
pub use vector_search::{VectorSearchEngine, VectorSearchConfig, HybridSearchResult, SemanticSearchResult, ExactSearchResult}; // Re-export key vector_search types
pub use embedding_queue::{EmbeddingQueue, EmbeddingQueueBuilder, EmbeddingProgress, RequestStatus}; // Re-export key embedding_queue types
pub use schema::{SchemaDefinition, ObjectTypeSchema, PropertySchema, EdgeTypeSchema, ValidationResult}; // Re-export key schema types
pub use schema_manager::{SchemaManager, SchemaStats}; // Re-export key schema_manager types
pub use schema_ingestion::SchemaIngestion; // Re-export schema ingestion functionality

use anyhow::Result;
use std::path::Path;
use std::sync::Arc;

/// Main knowledge graph interface combining storage and query capabilities
pub struct KnowledgeGraph {
    storage: Arc<KnowledgeGraphStorage>,
    embedding_manager: Arc<EmbeddingManager>,
    schema_manager: Arc<SchemaManager>,
}

impl KnowledgeGraph {
    /// Create a new knowledge graph instance
    ///
    /// # Arguments
    /// * `db_path` - Path to the RocksDB database.
    /// * `embedding_cache_dir` - Optional path to cache downloaded embedding models.
    pub fn new<P: AsRef<Path>>(db_path: P, embedding_cache_dir: Option<P>) -> Result<Self> {
        let storage = Arc::new(KnowledgeGraphStorage::new(db_path.as_ref())?);
        let cache_path_buf = embedding_cache_dir.map(|p| p.as_ref().to_path_buf());
        let embedding_manager = Arc::new(EmbeddingManager::try_new_local_default(cache_path_buf)?);
        let schema_manager = Arc::new(SchemaManager::new(storage.clone()));
        Ok(Self { storage, embedding_manager, schema_manager })
    }

    /// Get a reference to the embedding provider
    pub fn get_embedding_provider(&self) -> Arc<dyn EmbeddingProvider> {
        self.embedding_manager.get_provider()
    }

    /// Add a new object to the knowledge graph
    pub fn add_object(&self, metadata: ObjectMetadata) -> Result<ObjectId> {
        let id = metadata.id;
        self.storage.upsert_node(metadata)?;
        Ok(id)
    }

    /// Get an object by its ID
    pub fn get_object(&self, id: ObjectId) -> Result<Option<ObjectMetadata>> {
        self.storage.get_node(id)
    }

    /// Get all objects from the knowledge graph
    pub fn get_all_objects(&self) -> Result<Vec<ObjectMetadata>> {
        self.storage.get_all_objects()
    }

    /// Update an existing object
    pub fn update_object(&self, mut metadata: ObjectMetadata) -> Result<()> {
        metadata.touch(); // Update the modified timestamp
        self.storage.upsert_node(metadata)
    }

    /// Delete an object and all its relationships
    pub fn delete_object(&self, id: ObjectId) -> Result<()> {
        self.storage.delete_node(id)
    }

    /// Create a relationship between two objects
    pub fn connect_objects(&self, from: ObjectId, to: ObjectId, edge_type: EdgeType) -> Result<()> {
        let edge = Edge::new(from, to, edge_type);
        self.storage.upsert_edge(edge)
    }

    /// Create a relationship between two objects using a string edge type
    pub fn connect_objects_str(&self, from: ObjectId, to: ObjectId, edge_type: &str) -> Result<()> {
        let edge = Edge::new(from, to, EdgeType::from_str(edge_type));
        self.storage.upsert_edge(edge)
    }

    /// Create a weighted relationship between two objects  
    pub fn connect_objects_weighted(
        &self,
        from: ObjectId,
        to: ObjectId,
        edge_type: EdgeType,
        weight: f32,
    ) -> Result<()> {
        let edge = Edge::new(from, to, edge_type).with_weight(weight);
        self.storage.upsert_edge(edge)
    }

    /// Create a weighted relationship between two objects using a string edge type
    pub fn connect_objects_weighted_str(
        &self,
        from: ObjectId,
        to: ObjectId,
        edge_type: &str,
        weight: f32,
    ) -> Result<()> {
        let edge = Edge::new(from, to, EdgeType::from_str(edge_type)).with_weight(weight);
        self.storage.upsert_edge(edge)
    }

    /// Get all relationships for an object
    pub fn get_relationships(&self, id: ObjectId) -> Result<Vec<Edge>> {
        self.storage.get_edges(id)
    }

    /// Get neighboring objects (1-hop traversal)
    pub fn get_neighbors(&self, id: ObjectId) -> Result<Vec<ObjectId>> {
        self.storage.get_neighbors(id)
    }

    /// Add text content to an object
    pub fn add_text_chunk(&self, object_id: ObjectId, content: String, chunk_type: ChunkType) -> Result<ChunkId> {
        let chunk = TextChunk::new(object_id, content, chunk_type);
        let chunk_id = chunk.id;
        self.storage.upsert_chunk(chunk)?;
        Ok(chunk_id)
    }

    /// Get all text chunks for an object
    pub fn get_text_chunks(&self, object_id: ObjectId) -> Result<Vec<TextChunk>> {
        self.storage.get_chunks_for_node(object_id)
    }

    /// Find objects by name (exact match)
    pub fn find_by_name(&self, object_type: &str, name: &str) -> Result<Vec<ObjectMetadata>> {
        self.storage.find_nodes_by_name(object_type, name)
    }

    /// Query a subgraph starting from a given object
    pub fn query_subgraph(&self, start: ObjectId, max_hops: usize) -> Result<QueryResult> {
        self.storage.query_subgraph(start, max_hops)
    }

    /// Get statistics about the knowledge graph
    pub fn get_stats(&self) -> Result<storage::GraphStats> {
        self.storage.get_stats()
    }

    /// Get the schema manager
    pub fn get_schema_manager(&self) -> Arc<SchemaManager> {
        self.schema_manager.clone()
    }

    /// Validate an object against its schema
    pub async fn validate_object(&self, object: &ObjectMetadata) -> Result<ValidationResult> {
        self.schema_manager.validate_object(object).await
    }

    /// Add an object with schema validation
    pub async fn add_object_validated(&self, metadata: ObjectMetadata) -> Result<ObjectId> {
        let validation_result = self.validate_object(&metadata).await?;
        if !validation_result.valid {
            return Err(anyhow::anyhow!("Object validation failed: {:?}", validation_result.errors));
        }
        
        let id = metadata.id;
        self.storage.upsert_node(metadata)?;
        Ok(id)
    }

    /// Register a new object type in the default schema
    pub async fn register_object_type(&self, type_name: &str, type_schema: ObjectTypeSchema) -> Result<()> {
        self.schema_manager.register_object_type("default", type_name, type_schema).await
    }

    /// Register a new edge type in the default schema
    pub async fn register_edge_type(&self, edge_name: &str, edge_schema: EdgeTypeSchema) -> Result<()> {
        self.schema_manager.register_edge_type("default", edge_name, edge_schema).await
    }

    /// Get schema statistics
    pub async fn get_schema_stats(&self, schema_name: &str) -> Result<SchemaStats> {
        self.schema_manager.get_schema_stats(schema_name).await
    }

    /// List all available schemas
    pub async fn list_schemas(&self) -> Result<Vec<String>> {
        self.schema_manager.list_schemas().await
    }
}

/// Builder for creating TTRPG objects with common patterns
pub struct ObjectBuilder {
    metadata: ObjectMetadata,
}

impl ObjectBuilder {
    /// Create a new character
    pub fn character(name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new("character".to_string(), name),
        }
    }

    /// Create a new location
    pub fn location(name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new("location".to_string(), name),
        }
    }

    /// Create a new faction
    pub fn faction(name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new("faction".to_string(), name),
        }
    }

    /// Create a new item
    pub fn item(name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new("item".to_string(), name),
        }
    }

    /// Create a new event
    pub fn event(name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new("event".to_string(), name),
        }
    }

    /// Create a new session
    pub fn session(name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new("session".to_string(), name),
        }
    }

    /// Create a new object with custom type
    pub fn custom(object_type: String, name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new(object_type, name),
        }
    }

    /// Add a description
    pub fn with_description(mut self, description: String) -> Self {
        self.metadata = self.metadata.with_description(description);
        self
    }

    /// Add a property
    pub fn with_property(mut self, key: String, value: String) -> Self {
        self.metadata = self.metadata.with_property(key, value);
        self
    }

    /// Add a property with JSON value
    pub fn with_json_property(mut self, key: String, value: serde_json::Value) -> Self {
        self.metadata = self.metadata.with_json_property(key, value);
        self
    }

    /// Add a tag
    pub fn with_tag(mut self, tag: String) -> Self {
        self.metadata.add_tag(tag);
        self
    }

    /// Build the object metadata
    pub fn build(self) -> ObjectMetadata {
        self.metadata
    }

    /// Build and add to the knowledge graph
    pub fn add_to_graph(self, graph: &KnowledgeGraph) -> Result<ObjectId> {
        graph.add_object(self.metadata)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::path::PathBuf;

    fn get_test_embedding_cache_dir() -> PathBuf {
        std::env::var("FASTEMBED_CACHE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("target")
                    .join("test_model_cache");
                std::fs::create_dir_all(&path).expect("Failed to create test model cache dir for lib.rs");
                path
            })
    }

    fn create_test_graph() -> (KnowledgeGraph, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let cache_dir = get_test_embedding_cache_dir();
        let graph = KnowledgeGraph::new(temp_dir.path(), Some(&cache_dir)).unwrap();
        (graph, temp_dir)
    }

    async fn create_test_graph_async() -> (KnowledgeGraph, TempDir) {
        create_test_graph()
    }

    #[test]
    fn test_basic_graph_operations() {
        let (graph, _temp_dir) = create_test_graph();

        // Create two characters using the builder
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

        // Connect them with a relationship
        graph.connect_objects_str(gandalf_id, frodo_id, "knows").unwrap();

        // Verify the objects exist
        let gandalf = graph.get_object(gandalf_id).unwrap().unwrap();
        assert_eq!(gandalf.name, "Gandalf");
        assert_eq!(gandalf.object_type, "character");

        let frodo = graph.get_object(frodo_id).unwrap().unwrap();
        assert_eq!(frodo.name, "Frodo Baggins");

        // Verify the relationship
        let gandalf_relationships = graph.get_relationships(gandalf_id).unwrap();
        assert_eq!(gandalf_relationships.len(), 1);
        assert_eq!(gandalf_relationships[0].to, frodo_id);
        assert_eq!(gandalf_relationships[0].edge_type, EdgeType::from_str("knows"));

        // Test neighbors
        let gandalf_neighbors = graph.get_neighbors(gandalf_id).unwrap();
        assert_eq!(gandalf_neighbors.len(), 1);
        assert_eq!(gandalf_neighbors[0], frodo_id);

        // Add text content
        let chunk_id = graph.add_text_chunk(
            gandalf_id,
            "Gandalf appeared at Bilbo's birthday party in a spectacular fashion.".to_string(),
            ChunkType::UserNote,
        ).unwrap();

        let chunks = graph.get_text_chunks(gandalf_id).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].id, chunk_id);

        // Test subgraph query
        let subgraph = graph.query_subgraph(gandalf_id, 1).unwrap();
        assert_eq!(subgraph.objects.len(), 2); // Gandalf and Frodo
        assert_eq!(subgraph.edges.len(), 2); // Edge appears twice (outgoing from Gandalf, incoming to Frodo)
        assert_eq!(subgraph.chunks.len(), 1); // Gandalf's text chunk

        // Test stats
        let stats = graph.get_stats().unwrap();
        assert_eq!(stats.node_count, 2);
        assert_eq!(stats.edge_count, 1);
        assert_eq!(stats.chunk_count, 1);
        assert!(stats.total_tokens > 0);
    }

    #[test]
    fn test_find_by_name() {
        let (graph, _temp_dir) = create_test_graph();

        let _gandalf_id = ObjectBuilder::character("Gandalf".to_string())
            .add_to_graph(&graph)
            .unwrap();

        let found = graph.find_by_name("character", "Gandalf").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Gandalf");
    }

    #[test]
    fn test_weighted_relationships() {
        let (graph, _temp_dir) = create_test_graph();

        let sauron_id = ObjectBuilder::character("Sauron".to_string()).add_to_graph(&graph).unwrap();
        let frodo_id = ObjectBuilder::character("Frodo".to_string()).add_to_graph(&graph).unwrap();

        // Create a strong enemy relationship
        graph.connect_objects_weighted_str(sauron_id, frodo_id, "enemy_of", 0.9).unwrap();

        let relationships = graph.get_relationships(sauron_id).unwrap();
        assert_eq!(relationships.len(), 1);
        assert_eq!(relationships[0].weight, 0.9);
        assert_eq!(relationships[0].edge_type, EdgeType::from_str("enemy_of"));
    }

    #[test]
    fn test_complex_world_scenario() {
        let (graph, _temp_dir) = create_test_graph();

        // Create a small Middle-earth scenario
        let shire_id = ObjectBuilder::location("The Shire".to_string())
            .with_description("A peaceful land of rolling green hills".to_string())
            .add_to_graph(&graph)
            .unwrap();

        let bag_end_id = ObjectBuilder::location("Bag End".to_string())
            .with_description("Bilbo's hobbit hole".to_string())
            .add_to_graph(&graph)
            .unwrap();

        let frodo_id = ObjectBuilder::character("Frodo Baggins".to_string())
            .with_description("A hobbit of the Shire".to_string())
            .add_to_graph(&graph)
            .unwrap();

        let ring_id = ObjectBuilder::item("The One Ring".to_string())
            .with_description("The master ring of power".to_string())
            .add_to_graph(&graph)
            .unwrap();

        let fellowship_id = ObjectBuilder::faction("Fellowship of the Ring".to_string())
            .with_description("Nine companions united to destroy the Ring".to_string())
            .add_to_graph(&graph)
            .unwrap();

        // Create relationships
        graph.connect_objects_str(bag_end_id, shire_id, "located_in").unwrap();
        graph.connect_objects_str(frodo_id, bag_end_id, "located_in").unwrap();
        graph.connect_objects_str(frodo_id, ring_id, "owned_by").unwrap();
        graph.connect_objects_str(frodo_id, fellowship_id, "member_of").unwrap();

        // Query the subgraph around Frodo
        let frodo_world = graph.query_subgraph(frodo_id, 2).unwrap();
        
        // Should include Frodo, Bag End, The Shire, The Ring, and Fellowship
        assert_eq!(frodo_world.objects.len(), 5);
        
        // Verify we can find all the connections
        assert!(frodo_world.edges.len() >= 4); // At least our 4 relationships
        
        let stats = graph.get_stats().unwrap();
        assert_eq!(stats.node_count, 5);
        assert_eq!(stats.edge_count, 4);
    }

    #[tokio::test]
    async fn test_schema_integration() {
        let (graph, _temp_dir) = create_test_graph_async().await;

        // Register a custom spell object type
        let spell_schema = ObjectTypeSchema::new("spell".to_string(), "A magical spell".to_string())
            .with_property("level".to_string(), PropertySchema::number("Spell level"))
            .with_property("school".to_string(), PropertySchema::string("School of magic"))
            .with_required_property("level".to_string());

        graph.register_object_type("spell", spell_schema).await.unwrap();

        // Create a spell object
        let spell = ObjectBuilder::custom("spell".to_string(), "Fireball".to_string())
            .with_json_property("level".to_string(), serde_json::Value::Number(serde_json::Number::from(3)))
            .with_json_property("school".to_string(), serde_json::Value::String("Evocation".to_string()))
            .build();

        // Validate and add the spell
        let validation_result = graph.validate_object(&spell).await.unwrap();
        assert!(validation_result.valid);

        let spell_id = graph.add_object_validated(spell).await.unwrap();

        // Verify it was added
        let retrieved_spell = graph.get_object(spell_id).unwrap().unwrap();
        assert_eq!(retrieved_spell.object_type, "spell");
        assert_eq!(retrieved_spell.name, "Fireball");

        // Test schema stats
        let stats = graph.get_schema_stats("default").await.unwrap();
        assert!(stats.object_type_count >= 7); // 6 default + 1 spell
    }

    #[tokio::test]
    async fn test_validation_failure() {
        let (graph, _temp_dir) = create_test_graph_async().await;

        // Create an invalid object (unknown type)
        let invalid_object = ObjectMetadata::new("unknown_type".to_string(), "Test".to_string());

        let validation_result = graph.validate_object(&invalid_object).await.unwrap();
        assert!(!validation_result.valid);
        assert!(!validation_result.errors.is_empty());

        // Should fail to add
        let result = graph.add_object_validated(invalid_object).await;
        assert!(result.is_err());
    }
}