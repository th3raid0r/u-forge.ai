use crate::types::{Edge, ObjectId, ObjectMetadata, QueryResult, TextChunk};
use anyhow::{Context, Result};
use rocksdb::{ColumnFamilyDescriptor, DB, Options, WriteBatch};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use uuid::Uuid;

/// Column family names for different data types
const CF_NODES: &str = "nodes";
const CF_CHUNKS: &str = "chunks";
const CF_EDGES: &str = "edges";
const CF_NAMES: &str = "names";
const CF_SCHEMAS: &str = "schemas";

/// Storage engine for the knowledge graph using RocksDB
pub struct KnowledgeGraphStorage {
    db: Arc<DB>,
}

/// Compressed adjacency list for efficient edge storage
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AdjacencyList {
    outgoing: Vec<Edge>,
    incoming: Vec<Edge>,
}

impl AdjacencyList {
    fn new() -> Self {
        Self {
            outgoing: Vec::new(),
            incoming: Vec::new(),
        }
    }
}

/// Statistics about the knowledge graph
#[derive(Debug, Clone)]
pub struct GraphStats {
    pub node_count: usize,
    pub edge_count: usize,
    pub chunk_count: usize,
    pub total_tokens: usize,
}

impl KnowledgeGraphStorage {
    /// Create a new knowledge graph storage instance
    pub fn new<P: AsRef<Path>>(db_path: P) -> Result<Self> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);

        // Configure for performance
        opts.set_max_background_jobs(4);
        opts.set_bytes_per_sync(1048576); // 1MB
        opts.set_write_buffer_size(64 * 1024 * 1024); // 64MB
        opts.set_max_write_buffer_number(3);
        opts.set_target_file_size_base(64 * 1024 * 1024); // 64MB

        // Column family configurations
        let cf_descriptors = vec![
            ColumnFamilyDescriptor::new(CF_NODES, Options::default()),
            ColumnFamilyDescriptor::new(CF_CHUNKS, Options::default()),
            ColumnFamilyDescriptor::new(CF_EDGES, Options::default()),
            ColumnFamilyDescriptor::new(CF_NAMES, Options::default()),
            ColumnFamilyDescriptor::new(CF_SCHEMAS, Options::default()),
        ];

        let db = DB::open_cf_descriptors(&opts, db_path, cf_descriptors)
            .context("Failed to open RocksDB database")?;

        Ok(Self { db: Arc::new(db) })
    }

    /// Insert or update a node in the graph
    pub fn upsert_node(&self, metadata: ObjectMetadata) -> Result<()> {
        let _cf_nodes = self.get_cf(CF_NODES)?;
        let cf_names = self.get_cf(CF_NAMES)?;

        let key = metadata.id.as_bytes();
        let value = serde_json::to_vec(&metadata).context("Failed to serialize node metadata")?;

        let mut batch = WriteBatch::default();

        // Store node metadata
        batch.put_cf(&_cf_nodes, key, value);

        // Index by name for fast lookups
        let name_key = format!("{}:{}", metadata.object_type, metadata.name);
        batch.put_cf(&cf_names, name_key.as_bytes(), key);

        self.db
            .write(batch)
            .context("Failed to write node to database")?;

        Ok(())
    }

    /// Get a node by its ID
    /// Retrieve a node by its ID
    pub fn get_node(&self, id: ObjectId) -> Result<Option<ObjectMetadata>> {
        let cf_nodes = self.get_cf(CF_NODES)?;
        let key = id.as_bytes();

        match self.db.get_cf(cf_nodes, key)? {
            Some(value) => {
                let metadata: ObjectMetadata =
                    serde_json::from_slice(&value).context("Failed to deserialize node metadata")?;
                Ok(Some(metadata))
            }
            None => Ok(None),
        }
    }

    /// Get all objects from the knowledge graph
    pub fn get_all_objects(&self) -> Result<Vec<ObjectMetadata>> {
        let cf = self.get_cf(CF_NODES)?;
        let mut objects = Vec::new();
        let iter = self.db.iterator_cf(cf, rocksdb::IteratorMode::Start);

        for item in iter {
            match item {
                Ok((_key, value)) => {
                    let metadata: ObjectMetadata = serde_json::from_slice(&value)
                        .context("Failed to deserialize ObjectMetadata")?;
                    objects.push(metadata);
                }
                Err(e) => {
                    // Log error or handle as appropriate
                    eprintln!("Error iterating CF_NODES: {:?}", e);
                }
            }
        }
        Ok(objects)
    }

    /// Find nodes by name (exact match)
    pub fn find_nodes_by_name(&self, object_type: &str, name: &str) -> Result<Vec<ObjectMetadata>> {
        let cf_names = self.get_cf(CF_NAMES)?;
        let _cf_nodes = self.get_cf(CF_NODES)?;

        let name_key = format!("{}:{}", object_type, name);
        let mut results = Vec::new();

        if let Some(id_bytes) = self.db.get_cf(cf_names, name_key.as_bytes())? {
            if let Ok(id) = Uuid::from_slice(&id_bytes) {
                if let Some(metadata) = self.get_node(id)? {
                    results.push(metadata);
                }
            }
        }

        Ok(results)
    }

    /// Add or update an edge between two nodes
    pub fn upsert_edge(&self, edge: Edge) -> Result<()> {
        let cf_edges = self.get_cf(CF_EDGES)?;

        // Get or create adjacency lists for both nodes
        let mut from_adj = self
            .get_adjacency_list(edge.from)?
            .unwrap_or_else(AdjacencyList::new);
        let mut to_adj = self
            .get_adjacency_list(edge.to)?
            .unwrap_or_else(AdjacencyList::new);

        // Remove existing edge if it exists (to prevent duplicates)
        from_adj
            .outgoing
            .retain(|e| !(e.to == edge.to && e.edge_type == edge.edge_type));
        to_adj
            .incoming
            .retain(|e| !(e.from == edge.from && e.edge_type == edge.edge_type));

        // Add the new edge
        from_adj.outgoing.push(edge.clone());
        to_adj.incoming.push(edge.clone());

        // Write updated adjacency lists
        let mut batch = WriteBatch::default();

        let from_key = edge.from.as_bytes();
        let from_value =
            bincode::serialize(&from_adj).context("Failed to serialize from adjacency list")?;
        batch.put_cf(cf_edges, from_key, from_value);

        let to_key = edge.to.as_bytes();
        let to_value =
            bincode::serialize(&to_adj).context("Failed to serialize to adjacency list")?;
        batch.put_cf(cf_edges, to_key, to_value);

        self.db
            .write(batch)
            .context("Failed to write edge to database")?;

        Ok(())
    }

    /// Get all edges for a node (both incoming and outgoing)
    pub fn get_edges(&self, node_id: ObjectId) -> Result<Vec<Edge>> {
        let adj_list = self.get_adjacency_list(node_id)?;
        match adj_list {
            Some(adj) => {
                let mut edges = adj.outgoing;
                edges.extend(adj.incoming);
                Ok(edges)
            }
            None => Ok(Vec::new()),
        }
    }

    /// Get neighbors of a node (1-hop traversal)
    pub fn get_neighbors(&self, node_id: ObjectId) -> Result<Vec<ObjectId>> {
        let adj_list = self.get_adjacency_list(node_id)?;
        match adj_list {
            Some(adj) => {
                let mut neighbors = Vec::new();
                for edge in &adj.outgoing {
                    neighbors.push(edge.to);
                }
                for edge in &adj.incoming {
                    neighbors.push(edge.from);
                }
                neighbors.sort();
                neighbors.dedup();
                Ok(neighbors)
            }
            None => Ok(Vec::new()),
        }
    }

    /// Add a text chunk for a node
    pub fn upsert_chunk(&self, chunk: TextChunk) -> Result<()> {
        let cf_chunks = self.get_cf(CF_CHUNKS)?;
        let key = chunk.id.as_bytes();
        let value = bincode::serialize(&chunk).context("Failed to serialize text chunk")?;

        self.db
            .put_cf(cf_chunks, key, value)
            .context("Failed to write chunk to database")?;

        Ok(())
    }

    /// Get text chunks for a node
    pub fn get_chunks_for_node(&self, node_id: ObjectId) -> Result<Vec<TextChunk>> {
        let cf_chunks = self.get_cf(CF_CHUNKS)?;
        let mut chunks = Vec::new();

        let iter = self.db.iterator_cf(cf_chunks, rocksdb::IteratorMode::Start);
        for item in iter {
            let (_, value) = item?;
            if let Ok(chunk) = bincode::deserialize::<TextChunk>(&value) {
                if chunk.object_id == node_id {
                    chunks.push(chunk);
                }
            }
        }

        Ok(chunks)
    }

    /// Get basic statistics about the knowledge graph
    pub fn get_stats(&self) -> Result<GraphStats> {
        let cf_nodes = self.get_cf(CF_NODES)?;
        let cf_chunks = self.get_cf(CF_CHUNKS)?;
        let cf_edges = self.get_cf(CF_EDGES)?;

        let node_count = self
            .db
            .iterator_cf(cf_nodes, rocksdb::IteratorMode::Start)
            .count();
        let chunk_count = self
            .db
            .iterator_cf(cf_chunks, rocksdb::IteratorMode::Start)
            .count();

        // Count edges by iterating over adjacency lists
        let mut edge_count = 0;
        let mut total_tokens = 0;

        let edge_iter = self.db.iterator_cf(cf_edges, rocksdb::IteratorMode::Start);
        for item in edge_iter {
            let (_, value) = item?;
            if let Ok(adj_list) = bincode::deserialize::<AdjacencyList>(&value) {
                edge_count += adj_list.outgoing.len();
            }
        }

        // Count total tokens in chunks
        let chunk_iter = self.db.iterator_cf(cf_chunks, rocksdb::IteratorMode::Start);
        for item in chunk_iter {
            let (_, value) = item?;
            if let Ok(chunk) = bincode::deserialize::<TextChunk>(&value) {
                total_tokens += chunk.token_count;
            }
        }

        Ok(GraphStats {
            node_count,
            edge_count,
            chunk_count,
            total_tokens,
        })
    }

    /// Perform a simple graph traversal query
    pub fn query_subgraph(&self, start_node: ObjectId, max_hops: usize) -> Result<QueryResult> {
        let mut result = QueryResult::new();
        let mut visited = std::collections::HashSet::new();
        let mut current_nodes = vec![start_node];

        for _hop in 0..=max_hops {
            let mut next_nodes = Vec::new();

            for node_id in current_nodes {
                if visited.contains(&node_id) {
                    continue;
                }
                visited.insert(node_id);

                // Add node metadata
                if let Some(metadata) = self.get_node(node_id)? {
                    result.add_object(metadata);
                }

                // Add edges and find neighbors
                let edges = self.get_edges(node_id)?;
                for edge in edges {
                    result.add_edge(edge.clone());
                    if !visited.contains(&edge.to) {
                        next_nodes.push(edge.to);
                    }
                    if !visited.contains(&edge.from) {
                        next_nodes.push(edge.from);
                    }
                }

                // Add text chunks
                let chunks = self.get_chunks_for_node(node_id)?;
                for chunk in chunks {
                    result.add_chunk(chunk);
                }
            }

            current_nodes = next_nodes;
            if current_nodes.is_empty() {
                break;
            }
        }

        Ok(result)
    }

    /// Delete a node and all its edges and chunks
    pub fn delete_node(&self, node_id: ObjectId) -> Result<()> {
        let cf_nodes = self.get_cf(CF_NODES)?;
        let cf_chunks = self.get_cf(CF_CHUNKS)?;
        let cf_edges = self.get_cf(CF_EDGES)?;
        let cf_names = self.get_cf(CF_NAMES)?;

        let mut batch = WriteBatch::default();

        // Get node metadata first for name cleanup
        if let Some(metadata) = self.get_node(node_id)? {
            let name_key = format!("{}:{}", metadata.object_type.as_str(), metadata.name);
            batch.delete_cf(cf_names, name_key.as_bytes());
        }

        // Delete node metadata
        batch.delete_cf(cf_nodes, node_id.as_bytes());

        // Delete adjacency list
        batch.delete_cf(cf_edges, node_id.as_bytes());

        // Delete associated chunks
        let chunks = self.get_chunks_for_node(node_id)?;
        for chunk in chunks {
            batch.delete_cf(cf_chunks, chunk.id.as_bytes());
        }

        // Remove references to this node from other nodes' adjacency lists
        // This is expensive but necessary for consistency
        let iter = self.db.iterator_cf(cf_edges, rocksdb::IteratorMode::Start);
        for item in iter {
            let (key_bytes, value) = item?;
            if let Ok(mut adj_list) = bincode::deserialize::<AdjacencyList>(&value) {
                let original_len = adj_list.outgoing.len() + adj_list.incoming.len();
                adj_list.outgoing.retain(|e| e.to != node_id);
                adj_list.incoming.retain(|e| e.from != node_id);

                // Only update if we removed edges
                if adj_list.outgoing.len() + adj_list.incoming.len() < original_len {
                    let updated_value = bincode::serialize(&adj_list)?;
                    batch.put_cf(cf_edges, &key_bytes, updated_value);
                }
            }
        }

        self.db
            .write(batch)
            .context("Failed to delete node from database")?;

        Ok(())
    }

    /// Get column family handle
    fn get_cf(&self, name: &str) -> Result<&rocksdb::ColumnFamily> {
        self.db
            .cf_handle(name)
            .ok_or_else(|| anyhow::anyhow!("Column family '{}' not found", name))
    }

    /// Get adjacency list for a node
    fn get_adjacency_list(&self, node_id: ObjectId) -> Result<Option<AdjacencyList>> {
        let cf_edges = self.get_cf(CF_EDGES)?;
        let key = node_id.as_bytes();

        match self.db.get_cf(cf_edges, key)? {
            Some(value) => {
                let adj_list: AdjacencyList =
                    bincode::deserialize(&value).context("Failed to deserialize adjacency list")?;
                Ok(Some(adj_list))
            }
            None => Ok(None),
        }
    }

    /// Get a schema by name
    pub fn get_schema(&self, name: &str) -> Result<Option<crate::schema::SchemaDefinition>> {
        let cf = self.get_cf(CF_SCHEMAS)?;
        let key = name.as_bytes();
        
        match self.db.get_cf(&cf, key)? {
            Some(data) => {
                let schema: crate::schema::SchemaDefinition = bincode::deserialize(&data)
                    .context("Failed to deserialize schema")?;
                Ok(Some(schema))
            }
            None => Ok(None),
        }
    }

    /// Save a schema
    pub fn save_schema(&self, schema: &crate::schema::SchemaDefinition) -> Result<()> {
        let cf = self.get_cf(CF_SCHEMAS)?;
        let key = schema.name.as_bytes();
        let value = bincode::serialize(schema)
            .context("Failed to serialize schema")?;
        
        self.db.put_cf(&cf, key, value)
            .context("Failed to save schema")?;
        
        Ok(())
    }

    /// Delete a schema
    pub fn delete_schema(&self, name: &str) -> Result<()> {
        let cf = self.get_cf(CF_SCHEMAS)?;
        let key = name.as_bytes();
        
        self.db.delete_cf(&cf, key)
            .context("Failed to delete schema")?;
        
        Ok(())
    }

    /// List all schema names
    pub fn list_schemas(&self) -> Result<Vec<String>> {
        let cf = self.get_cf(CF_SCHEMAS)?;
        let mut schemas = Vec::new();
        
        let iter = self.db.iterator_cf(&cf, rocksdb::IteratorMode::Start);
        for item in iter {
            let (key, _) = item?;
            let name = String::from_utf8(key.to_vec())
                .context("Invalid UTF-8 in schema name")?;
            schemas.push(name);
        }
        
        Ok(schemas)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EdgeType, ObjectType};
    use tempfile::TempDir;

    fn create_test_storage() -> (KnowledgeGraphStorage, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let storage = KnowledgeGraphStorage::new(temp_dir.path()).unwrap();
        (storage, temp_dir)
    }

    #[test]
    fn test_node_operations() {
        let (storage, _temp_dir) = create_test_storage();

        let metadata = ObjectMetadata::new("character".to_string(), "Gandalf".to_string())
            .with_description("A wise wizard".to_string());
        let node_id = metadata.id;

        // Insert node
        storage.upsert_node(metadata.clone()).unwrap();

        // Retrieve node
        let retrieved = storage.get_node(node_id).unwrap().unwrap();
        assert_eq!(retrieved.name, "Gandalf");
        assert_eq!(retrieved.object_type, "character");

        // Find by name
        let found = storage.find_nodes_by_name("character", "Gandalf").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, node_id);
    }

    #[test]
    fn test_edge_operations() {
        let (storage, _temp_dir) = create_test_storage();

        // Create two nodes
        let gandalf = ObjectMetadata::new("character".to_string(), "Gandalf".to_string());
        let frodo = ObjectMetadata::new("character".to_string(), "Frodo".to_string());

        storage.upsert_node(gandalf.clone()).unwrap();
        storage.upsert_node(frodo.clone()).unwrap();

        // Create edge
        let edge = Edge::new(gandalf.id, frodo.id, EdgeType::from_str("knows"));
        storage.upsert_edge(edge.clone()).unwrap();

        // Retrieve edges
        let gandalf_edges = storage.get_edges(gandalf.id).unwrap();
        assert_eq!(gandalf_edges.len(), 1);
        assert_eq!(gandalf_edges[0].to, frodo.id);
        assert_eq!(gandalf_edges[0].edge_type, EdgeType::from_str("knows"));

        let frodo_edges = storage.get_edges(frodo.id).unwrap();
        assert_eq!(frodo_edges.len(), 1);
        assert_eq!(frodo_edges[0].from, gandalf.id);

        // Test neighbors
        let gandalf_neighbors = storage.get_neighbors(gandalf.id).unwrap();
        assert_eq!(gandalf_neighbors.len(), 1);
        assert_eq!(gandalf_neighbors[0], frodo.id);
    }

    #[test]
    fn test_chunk_operations() {
        let (storage, _temp_dir) = create_test_storage();

        let metadata = ObjectMetadata::new("location".to_string(), "Shire".to_string());
        storage.upsert_node(metadata.clone()).unwrap();

        let chunk = TextChunk::new(
            metadata.id,
            "A peaceful land of rolling hills and green fields.".to_string(),
            crate::types::ChunkType::Description,
        );

        storage.upsert_chunk(chunk.clone()).unwrap();

        let retrieved_chunks = storage.get_chunks_for_node(metadata.id).unwrap();
        assert_eq!(retrieved_chunks.len(), 1);
        assert_eq!(retrieved_chunks[0].content, chunk.content);
    }

    #[test]
    fn test_subgraph_query() {
        let (storage, _temp_dir) = create_test_storage();

        // Create a small graph: Gandalf -> Frodo -> Sam
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

        // Query subgraph starting from Gandalf with max 2 hops
        let result = storage.query_subgraph(gandalf.id, 2).unwrap();

        assert_eq!(result.objects.len(), 3); // All three characters
        assert_eq!(result.edges.len(), 4); // 2 edges * 2 (incoming + outgoing)
    }

    #[test]
    fn test_node_deletion() {
        let (storage, _temp_dir) = create_test_storage();

        let gandalf = ObjectMetadata::new("character".to_string(), "Gandalf".to_string());
        let frodo = ObjectMetadata::new("character".to_string(), "Frodo".to_string());

        storage.upsert_node(gandalf.clone()).unwrap();
        storage.upsert_node(frodo.clone()).unwrap();
        storage
            .upsert_edge(Edge::new(gandalf.id, frodo.id, EdgeType::from_str("knows")))
            .unwrap();

        // Delete Gandalf
        storage.delete_node(gandalf.id).unwrap();

        // Verify deletion
        assert!(storage.get_node(gandalf.id).unwrap().is_none());

        // Verify edge cleanup
        let frodo_edges = storage.get_edges(frodo.id).unwrap();
        assert_eq!(frodo_edges.len(), 0);
    }

    #[test]
    fn test_stats() {
        let (storage, _temp_dir) = create_test_storage();

        let gandalf = ObjectMetadata::new("character".to_string(), "Gandalf".to_string());
        let frodo = ObjectMetadata::new("character".to_string(), "Frodo".to_string());

        storage.upsert_node(gandalf.clone()).unwrap();
        storage.upsert_node(frodo.clone()).unwrap();
        storage
            .upsert_edge(Edge::new(gandalf.id, frodo.id, EdgeType::from_str("knows")))
            .unwrap();

        let chunk = TextChunk::new(
            gandalf.id,
            "A wise wizard of great power.".to_string(),
            crate::types::ChunkType::Description,
        );
        storage.upsert_chunk(chunk.clone()).unwrap();

        let stats = storage.get_stats().unwrap();
        assert_eq!(stats.node_count, 2);
        assert_eq!(stats.edge_count, 1);
        assert_eq!(stats.chunk_count, 1);
        assert!(stats.total_tokens > 0);
    }

    #[test]
    fn test_schema_operations() {
        let (storage, _temp) = create_test_storage();
        
        // Create a test schema
        let schema = crate::schema::SchemaDefinition::create_default();
        
        // Save schema
        storage.save_schema(&schema).unwrap();
        
        // Retrieve schema
        let retrieved = storage.get_schema("default").unwrap().unwrap();
        assert_eq!(retrieved.name, "default");
        assert_eq!(retrieved.version, "1.0.0");
        
        // List schemas
        let schemas = storage.list_schemas().unwrap();
        assert!(schemas.contains(&"default".to_string()));
        
        // Delete schema
        storage.delete_schema("default").unwrap();
        assert!(storage.get_schema("default").unwrap().is_none());
    }
}
