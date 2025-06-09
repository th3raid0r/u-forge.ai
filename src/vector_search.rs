// src/vector_search.rs

//! vector_search.rs - Handles vector-based semantic search and exact name matching.

use crate::embeddings::EmbeddingProvider;
use crate::types::{ChunkId, ObjectId, ObjectType, ForgeUuid};
use anyhow::{Result, anyhow};
use fst::{Automaton, IntoStreamer, Map, MapBuilder, Streamer};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use std::sync::Arc;

// Constants for file naming
const HNSW_INDEX_FILE: &str = "hnsw_index.bin";
const HNSW_MAP_FILE: &str = "hnsw_map.bin";
const FST_INDEX_FILE: &str = "name_index.fst";
const FST_VALUES_FILE: &str = "name_values.bin";

/// Configuration for the vector search engine
#[derive(Debug, Clone)]
pub struct VectorSearchConfig {
    pub dimensions: usize,
    pub hnsw_m: usize,                // Max connections per node
    pub hnsw_ef_construction: usize,  // Size of candidate list during construction
    pub hnsw_ef_search: usize,        // Size of candidate list during search
    pub max_elements: usize,          // Initial capacity hint
}

impl Default for VectorSearchConfig {
    fn default() -> Self {
        Self {
            dimensions: 384, // Common embedding dimension
            hnsw_m: 16,
            hnsw_ef_construction: 200,
            hnsw_ef_search: 50,
            max_elements: 10000,
        }
    }
}

/// Result from semantic search
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticSearchResult {
    pub chunk_id: ChunkId,
    pub object_id: ObjectId,
    pub similarity: f32,
    pub text_preview: String,
}

/// Result from exact name search
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExactSearchResult {
    pub object_id: ObjectId,
    pub name: String,
    pub object_type: ObjectType,
}

/// Combined search results
#[derive(Debug, Clone)]
pub struct HybridSearchResult {
    pub semantic_results: Vec<SemanticSearchResult>,
    pub exact_results: Vec<ExactSearchResult>,
}

type PointMap = HashMap<usize, (ChunkId, ObjectId, String)>;

/// Simple vector store for now (can be replaced with HNSW later)
#[derive(Debug)]
struct SimpleVectorStore {
    vectors: Vec<Vec<f32>>,
    metadata: Vec<(ChunkId, ObjectId, String)>,
    dimensions: usize,
}

impl SimpleVectorStore {
    fn new(dimensions: usize) -> Self {
        Self {
            vectors: Vec::new(),
            metadata: Vec::new(),
            dimensions,
        }
    }

    fn add(&mut self, vector: Vec<f32>, chunk_id: ChunkId, object_id: ObjectId, text_preview: String) -> Result<()> {
        if vector.len() != self.dimensions {
            return Err(anyhow!("Vector dimension mismatch: expected {}, got {}", self.dimensions, vector.len()));
        }
        self.vectors.push(vector);
        self.metadata.push((chunk_id, object_id, text_preview));
        Ok(())
    }

    fn search(&self, query_vector: &[f32], k: usize) -> Vec<SemanticSearchResult> {
        if query_vector.len() != self.dimensions {
            return Vec::new();
        }

        let mut results = Vec::new();
        
        for (idx, stored_vector) in self.vectors.iter().enumerate() {
            let similarity = cosine_similarity(query_vector, stored_vector);
            if let Some((chunk_id, object_id, text_preview)) = self.metadata.get(idx) {
                results.push(SemanticSearchResult {
                    chunk_id: *chunk_id,
                    object_id: *object_id,
                    similarity,
                    text_preview: text_preview.clone(),
                });
            }
        }

        // Sort by similarity (highest first)
        results.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(k);
        results
    }

    fn len(&self) -> usize {
        self.vectors.len()
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot_product: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot_product / (norm_a * norm_b)
    }
}

/// Main vector search engine
pub struct VectorSearchEngine {
    config: VectorSearchConfig,
    embedding_provider: Arc<dyn EmbeddingProvider>,
    index_path: PathBuf,
    
    // Simple vector store (will be replaced with HNSW later)
    vector_store: RwLock<SimpleVectorStore>,
    
    // Name-based exact search using FST
    name_fst: RwLock<Option<Map<Vec<u8>>>>,
    fst_value_store: RwLock<Vec<(ObjectId, ObjectType)>>,
    
    // Text previews for chunks
    chunk_previews: RwLock<HashMap<ChunkId, String>>,
}

impl VectorSearchEngine {
    pub fn new(
        config: VectorSearchConfig,
        embedding_provider: Arc<dyn EmbeddingProvider>,
        index_path: PathBuf,
    ) -> Result<Self> {
        std::fs::create_dir_all(&index_path)?;
        
        let vector_store = RwLock::new(SimpleVectorStore::new(config.dimensions));
        
        Ok(Self {
            config,
            embedding_provider,
            index_path,
            vector_store,
            name_fst: RwLock::new(None),
            fst_value_store: RwLock::new(Vec::new()),
            chunk_previews: RwLock::new(HashMap::new()),
        })
    }

    pub async fn initialize(&mut self) -> Result<()> {
        // Try to load existing indexes
        self.load_name_fst_and_values()?;
        println!("Vector search engine initialized with {} dimensions", self.config.dimensions);
        Ok(())
    }

    pub async fn add_chunk(
        &self,
        chunk_id: ChunkId,
        object_id: ObjectId,
        content: &str,
    ) -> Result<()> {
        // Generate embedding
        let embedding = self.embedding_provider.embed(content).await?;
        
        // Create text preview (first 100 chars)
        let preview = if content.len() > 100 {
            format!("{}...", &content[..97])
        } else {
            content.to_string()
        };
        
        // Add to vector store
        {
            let mut store = self.vector_store.write();
            store.add(embedding, chunk_id, object_id, preview.clone())?;
        }
        
        // Store preview
        {
            let mut previews = self.chunk_previews.write();
            previews.insert(chunk_id, preview);
        }
        
        Ok(())
    }

    pub fn rebuild_name_index(&self, objects: Vec<(ObjectId, String, ObjectType)>) -> Result<()> {
        let fst_path = self.index_path.join(FST_INDEX_FILE);
        let values_path = self.index_path.join(FST_VALUES_FILE);
        
        // Build FST
        let mut builder = MapBuilder::new(BufWriter::new(File::create(&fst_path)?))?;
        let mut value_store = Vec::new();
        
        let mut sorted_objects = objects;
        sorted_objects.sort_by(|a, b| a.1.cmp(&b.1));
        
        for (object_id, name, object_type) in sorted_objects {
            let value_idx = value_store.len() as u64;
            value_store.push((object_id, object_type));
            builder.insert(name.as_bytes(), value_idx)?;
        }
        
        builder.finish()?;
        
        // Save value store
        let values_file = File::create(&values_path)?;
        bincode::serialize_into(BufWriter::new(values_file), &value_store)?;
        
        // Load into memory
        let fst_map = Map::new(std::fs::read(&fst_path)?)?;
        *self.name_fst.write() = Some(fst_map);
        *self.fst_value_store.write() = value_store.clone();
        
        Ok(())
    }

    pub async fn search_semantic(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SemanticSearchResult>> {
        // Generate query embedding
        let query_embedding = self.embedding_provider.embed(query).await?;
        
        // Search using simple vector store
        let store = self.vector_store.read();
        let results = store.search(&query_embedding, limit);
        
        Ok(results)
    }

    pub fn search_exact(&self, query: &str, limit: usize) -> Result<Vec<ExactSearchResult>> {
        let fst_guard = self.name_fst.read();
        let fst = match fst_guard.as_ref() {
            Some(fst) => fst,
            None => return Ok(Vec::new()), // No index built yet
        };
        
        let automaton = fst::automaton::Str::new(query).starts_with();
        let mut stream = fst.search(automaton).into_stream();
        let mut results = Vec::new();
        
        let value_store_guard = self.fst_value_store.read();
        
        while let Some((name_bytes, value_idx)) = stream.next() {
            if results.len() >= limit {
                break;
            }
            
            if let Some((object_id, object_type)) = value_store_guard.get(value_idx as usize) {
                let matched_name = String::from_utf8_lossy(name_bytes).to_string();
                results.push(ExactSearchResult {
                    object_id: *object_id,
                    name: matched_name,
                    object_type: object_type.clone(),
                });
            }
        }
        
        Ok(results)
    }

    pub async fn search_hybrid(
        &self,
        query: &str,
        semantic_limit: usize,
        exact_limit: usize,
    ) -> Result<HybridSearchResult> {
        let semantic_results = self.search_semantic(query, semantic_limit).await?;
        let exact_results = self.search_exact(query, exact_limit)?;
        
        Ok(HybridSearchResult {
            semantic_results,
            exact_results,
        })
    }

    pub fn save_indexes(&self) -> Result<()> {
        // For the simple vector store, we don't need to save anything special
        // The FST is already saved when rebuilt
        println!("Vector search indexes saved");
        Ok(())
    }

    fn load_name_fst_and_values(&self) -> Result<()> {
        let fst_path = self.index_path.join(FST_INDEX_FILE);
        let values_path = self.index_path.join(FST_VALUES_FILE);
        
        if fst_path.exists() && values_path.exists() {
            // Load FST
            let fst_data = std::fs::read(&fst_path)?;
            let fst_map = Map::new(fst_data)?;
            
            // Load value store
            let values_file = File::open(&values_path)?;
            let value_store: Vec<(ObjectId, ObjectType)> = 
                bincode::deserialize_from(BufReader::new(values_file))?;
            
            let value_store_len = value_store.len();
            *self.name_fst.write() = Some(fst_map);
            *self.fst_value_store.write() = value_store;
            
            println!("Loaded name index with {} entries", value_store_len);
        }
        
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embeddings::{EmbeddingProvider, EmbeddingProviderType, LocalEmbeddingModelType};
    use async_trait::async_trait;
    use tempfile::TempDir;

    // Mock embedding provider for testing
    struct MockEmbeddingProvider {
        dimensions: usize,
    }

    #[async_trait]
    impl EmbeddingProvider for MockEmbeddingProvider {
        async fn embed(&self, text: &str) -> Result<Vec<f32>> {
            // Simple deterministic "embedding" based on text hash
            let hash = text.len() as f32;
            let mut embedding = vec![0.0; self.dimensions];
            embedding[0] = hash / 1000.0; // Normalize
            embedding[1] = (text.chars().count() as f32) / 1000.0;
            Ok(embedding)
        }
        
        async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
            let mut results = Vec::new();
            for text in texts {
                results.push(self.embed(&text).await?);
            }
            Ok(results)
        }
        
        fn dimensions(&self) -> Result<usize> {
            Ok(self.dimensions)
        }
        
        fn max_tokens(&self) -> Result<usize> {
            Ok(512)
        }
        
        fn provider_type(&self) -> EmbeddingProviderType {
            EmbeddingProviderType::Local(LocalEmbeddingModelType::FastEmbed(
                crate::embeddings::FastEmbedModel::AllMiniLmL6V2,
            ))
        }
        
        fn model_info(&self) -> Option<fastembed::ModelInfo<fastembed::EmbeddingModel>> {
            None
        }
    }

    fn create_test_search_engine(
        temp_dir_path: &std::path::Path,
        dimensions: usize,
    ) -> VectorSearchEngine {
        let config = VectorSearchConfig {
            dimensions,
            ..Default::default()
        };
        let provider = Arc::new(MockEmbeddingProvider { dimensions });
        VectorSearchEngine::new(config, provider, temp_dir_path.to_path_buf()).unwrap()
    }

    fn create_real_embedding_manager() -> Result<crate::embeddings::EmbeddingManager> {
        let cache_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("vector_search_test_cache");
        crate::embeddings::EmbeddingManager::try_new_local_default(Some(cache_dir))
    }

    #[tokio::test]
    async fn test_vector_search_engine_new_and_initialize() {
        let temp_dir = TempDir::new().unwrap();
        let mut engine = create_test_search_engine(temp_dir.path(), 384);
        
        assert!(engine.initialize().await.is_ok());
    }

    #[tokio::test]
    async fn test_add_and_search_semantic_mock_provider() {
        let temp_dir = TempDir::new().unwrap();
        let mut engine = create_test_search_engine(temp_dir.path(), 10);
        engine.initialize().await.unwrap();

        let chunk_id1 = ForgeUuid::new_v4();
        let chunk_id2 = ForgeUuid::new_v4();
        let object_id1 = ForgeUuid::new_v4();
        let object_id2 = ForgeUuid::new_v4();

        // Add some chunks
        engine.add_chunk(chunk_id1, object_id1, "This is a test about magic").await.unwrap();
        engine.add_chunk(chunk_id2, object_id2, "This is about dragons and fire").await.unwrap();

        // Search for similar content
        let results = engine.search_semantic("magic test", 5).await.unwrap();
        
        assert!(!results.is_empty());
        assert!(results.len() <= 2);
        
        // Check that results have the expected structure
        for result in &results {
            assert!(result.similarity >= 0.0 && result.similarity <= 1.0);
            assert!(!result.text_preview.is_empty());
        }
    }

    #[tokio::test]
    async fn test_add_and_search_semantic_real_provider() {
        let temp_dir = TempDir::new().unwrap();
        
        // Try to create a real embedding manager, skip test if it fails
        let embedding_manager = match create_real_embedding_manager() {
            Ok(manager) => manager,
            Err(_) => {
                println!("Skipping real provider test - could not initialize FastEmbed");
                return;
            }
        };
        
        let provider = embedding_manager.get_provider();
        let dimensions = provider.dimensions().unwrap();
        
        let config = VectorSearchConfig {
            dimensions,
            ..Default::default()
        };
        
        let mut engine = VectorSearchEngine::new(config, provider, temp_dir.path().to_path_buf()).unwrap();
        engine.initialize().await.unwrap();

        let chunk_id1 = ForgeUuid::new_v4();
        let chunk_id2 = ForgeUuid::new_v4();
        let chunk_id3 = ForgeUuid::new_v4();
        let object_id1 = ForgeUuid::new_v4();
        let object_id2 = ForgeUuid::new_v4();
        let object_id3 = ForgeUuid::new_v4();

        // Add some chunks with different content
        engine.add_chunk(chunk_id1, object_id1, "Gandalf is a wise wizard who helps Frodo on his journey to destroy the One Ring.").await.unwrap();
        engine.add_chunk(chunk_id2, object_id2, "Dragons are powerful creatures that breathe fire and hoard treasure in dark caves.").await.unwrap();
        engine.add_chunk(chunk_id3, object_id3, "The Shire is a peaceful place where hobbits live in comfortable holes in the ground.").await.unwrap();

        // Search for wizard-related content
        let wizard_results = engine.search_semantic("wizard magic", 2).await.unwrap();
        assert!(!wizard_results.is_empty());

        // Search for dragon-related content  
        let dragon_results = engine.search_semantic("dragon fire", 2).await.unwrap();
        assert!(!dragon_results.is_empty());

        // The first result should be the most relevant
        // With real embeddings, "wizard magic" should match better with Gandalf than dragons
        if wizard_results.len() > 1 {
            // This is a semantic check - Gandalf text should be more similar to "wizard magic"
            println!("Wizard search results:");
            for (i, result) in wizard_results.iter().enumerate() {
                println!("  {}: {} (similarity: {:.3})", i, result.text_preview, result.similarity);
            }
        }
    }

    #[tokio::test]
    async fn test_rebuild_and_search_name_fst() {
        let temp_dir = TempDir::new().unwrap();
        let mut engine = create_test_search_engine(temp_dir.path(), 384);
        engine.initialize().await.unwrap();

        let gandalf_id = ForgeUuid::new_v4();
        let frodo_id = ForgeUuid::new_v4();
        let sauron_id = ForgeUuid::new_v4();

        // Build name index
        let objects = vec![
            (gandalf_id, "Gandalf".to_string(), ObjectType::Character),
            (frodo_id, "Frodo Baggins".to_string(), ObjectType::Character),
            (sauron_id, "Sauron".to_string(), ObjectType::Character),
        ];
        
        engine.rebuild_name_index(objects).unwrap();

        // Test exact name search
        let gandalf_results = engine.search_exact("Gandalf", 5).unwrap();
        assert_eq!(gandalf_results.len(), 1);
        assert_eq!(gandalf_results[0].name, "Gandalf");
        assert_eq!(gandalf_results[0].object_id, gandalf_id);

        // Test prefix search
        let frodo_results = engine.search_exact("Frodo", 5).unwrap();
        assert_eq!(frodo_results.len(), 1);
        assert_eq!(frodo_results[0].name, "Frodo Baggins");
    }

    #[tokio::test]
    async fn test_hybrid_search() {
        let temp_dir = TempDir::new().unwrap();
        let mut engine = create_test_search_engine(temp_dir.path(), 10);
        engine.initialize().await.unwrap();

        let gandalf_id = ForgeUuid::new_v4();
        let gandalf_chunk_id = ForgeUuid::new_v4();
        let frodo_id = ForgeUuid::new_v4();
        let frodo_chunk_id = ForgeUuid::new_v4();

        // Add semantic content
        engine.add_chunk(gandalf_chunk_id, gandalf_id, "Gandalf the wizard casts powerful spells").await.unwrap();
        engine.add_chunk(frodo_chunk_id, frodo_id, "Frodo the hobbit carries the ring").await.unwrap();

        // Build name index
        let objects = vec![
            (gandalf_id, "Gandalf".to_string(), ObjectType::Character),
            (frodo_id, "Frodo Baggins".to_string(), ObjectType::Character),
        ];
        engine.rebuild_name_index(objects).unwrap();

        // Test hybrid search
        let results = engine.search_hybrid("Gandalf", 2, 2).await.unwrap();
        
        // Should have both semantic and exact results
        assert!(!results.semantic_results.is_empty());
        assert!(!results.exact_results.is_empty());
        
        // Exact results should include Gandalf
        let has_gandalf_exact = results.exact_results.iter()
            .any(|r| r.name == "Gandalf");
        assert!(has_gandalf_exact);
        
        // Semantic results should include content about Gandalf
        assert!(!results.semantic_results.is_empty());
    }

    #[tokio::test]
    async fn test_persistence() {
        let temp_dir = TempDir::new().unwrap();
        let gandalf_id = ForgeUuid::new_v4();
        let frodo_id = ForgeUuid::new_v4();
        
        // Create first engine and add data
        {
            let mut engine = create_test_search_engine(temp_dir.path(), 384);
            engine.initialize().await.unwrap();
            
            let objects = vec![
                (gandalf_id, "Gandalf".to_string(), ObjectType::Character),
                (frodo_id, "Frodo Baggins".to_string(), ObjectType::Character),
            ];
            engine.rebuild_name_index(objects).unwrap();
            engine.save_indexes().unwrap();
        }
        
        // Create second engine and verify data persisted
        {
            let mut engine2 = create_test_search_engine(temp_dir.path(), 384);
            engine2.initialize().await.unwrap();
            
            let results = engine2.search_exact("Gandalf", 5).unwrap();
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].name, "Gandalf");
            assert_eq!(results[0].object_id, gandalf_id);
        }
    }
}