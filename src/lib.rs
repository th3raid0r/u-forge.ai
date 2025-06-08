//! u-forge.ai - Universe Forge
//! 
//! A local-first TTRPG worldbuilding application with AI-powered knowledge graphs.
//! 
//! This crate provides the core storage and graph processing functionality for
//! managing interconnected worldbuilding data including characters, locations,
//! factions, items, events, and session notes.

pub mod types;
pub mod storage;

pub use types::*;
pub use storage::KnowledgeGraphStorage;

use anyhow::Result;
use std::path::Path;

/// Main knowledge graph interface combining storage and query capabilities
pub struct KnowledgeGraph {
    storage: KnowledgeGraphStorage,
}

impl KnowledgeGraph {
    /// Create a new knowledge graph instance
    pub fn new<P: AsRef<Path>>(db_path: P) -> Result<Self> {
        let storage = KnowledgeGraphStorage::new(db_path)?;
        Ok(Self { storage })
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
    pub fn find_by_name(&self, object_type: ObjectType, name: &str) -> Result<Vec<ObjectMetadata>> {
        self.storage.find_nodes_by_name(object_type.as_str(), name)
    }

    /// Query a subgraph starting from a given object
    pub fn query_subgraph(&self, start: ObjectId, max_hops: usize) -> Result<QueryResult> {
        self.storage.query_subgraph(start, max_hops)
    }

    /// Get statistics about the knowledge graph
    pub fn get_stats(&self) -> Result<storage::GraphStats> {
        self.storage.get_stats()
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
            metadata: ObjectMetadata::new(ObjectType::Character, name),
        }
    }

    /// Create a new location
    pub fn location(name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new(ObjectType::Location, name),
        }
    }

    /// Create a new faction
    pub fn faction(name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new(ObjectType::Faction, name),
        }
    }

    /// Create a new item
    pub fn item(name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new(ObjectType::Item, name),
        }
    }

    /// Create a new event
    pub fn event(name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new(ObjectType::Event, name),
        }
    }

    /// Create a new session
    pub fn session(name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new(ObjectType::Session, name),
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

    fn create_test_graph() -> (KnowledgeGraph, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let graph = KnowledgeGraph::new(temp_dir.path()).unwrap();
        (graph, temp_dir)
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
        graph.connect_objects(gandalf_id, frodo_id, EdgeType::Knows).unwrap();

        // Verify the objects exist
        let gandalf = graph.get_object(gandalf_id).unwrap().unwrap();
        assert_eq!(gandalf.name, "Gandalf");
        assert_eq!(gandalf.object_type, ObjectType::Character);

        let frodo = graph.get_object(frodo_id).unwrap().unwrap();
        assert_eq!(frodo.name, "Frodo Baggins");

        // Verify the relationship
        let gandalf_relationships = graph.get_relationships(gandalf_id).unwrap();
        assert_eq!(gandalf_relationships.len(), 1);
        assert_eq!(gandalf_relationships[0].to, frodo_id);
        assert_eq!(gandalf_relationships[0].edge_type, EdgeType::Knows);

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

        let found = graph.find_by_name(ObjectType::Character, "Gandalf").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Gandalf");
    }

    #[test]
    fn test_weighted_relationships() {
        let (graph, _temp_dir) = create_test_graph();

        let sauron_id = ObjectBuilder::character("Sauron".to_string()).add_to_graph(&graph).unwrap();
        let frodo_id = ObjectBuilder::character("Frodo".to_string()).add_to_graph(&graph).unwrap();

        // Create a strong enemy relationship
        graph.connect_objects_weighted(sauron_id, frodo_id, EdgeType::EnemyOf, 0.9).unwrap();

        let relationships = graph.get_relationships(sauron_id).unwrap();
        assert_eq!(relationships.len(), 1);
        assert_eq!(relationships[0].weight, 0.9);
        assert_eq!(relationships[0].edge_type, EdgeType::EnemyOf);
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
        graph.connect_objects(bag_end_id, shire_id, EdgeType::LocatedIn).unwrap();
        graph.connect_objects(frodo_id, bag_end_id, EdgeType::LocatedIn).unwrap();
        graph.connect_objects(frodo_id, ring_id, EdgeType::OwnedBy).unwrap();
        graph.connect_objects(frodo_id, fellowship_id, EdgeType::MemberOf).unwrap();

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
}