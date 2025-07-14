# CLAUDE.md - Technical Implementation Guide

This document provides technical implementation patterns and guidance for working with the u-forge.ai codebase.

## Codebase Architecture Patterns

### RocksDB Integration Pattern
```rust
// Always use column families for data separation
let cf_nodes = db.cf_handle("nodes").unwrap();
let cf_chunks = db.cf_handle("chunks").unwrap();
let cf_edges = db.cf_handle("edges").unwrap();

// Use transactions for atomic operations
let mut batch = WriteBatch::default();
batch.put_cf(&cf_nodes, key, value);
batch.put_cf(&cf_edges, edge_key, edge_value);
db.write(batch)?;
```

### Error Handling Patterns
```rust
// Use Result<T, E> everywhere, never panic in library code
pub fn get_node(&self, id: &str) -> Result<Option<Node>, StorageError> {
    match self.db.get_cf(&self.cf_nodes, id) {
        Ok(Some(bytes)) => Ok(Some(bincode::deserialize(&bytes)?)),
        Ok(None) => Ok(None),
        Err(e) => Err(StorageError::Database(e)),
    }
}

// Use anyhow::Result for most error handling in this codebase
pub async fn embed(&self, text: &str) -> Result<Vec<f32>> {
    let documents = vec![text];
    let embeddings: Vec<FastEmbedEmbedding> = self.model.embed(documents, None)
        .map_err(|e| anyhow!("FastEmbed embedding failed: {}", e))?;
    // ...
}
```

### String-Based Edge Type Patterns
```rust
// Use string-based edge types for maximum schema flexibility
graph.connect_objects_str(character_id, spell_id, "learned_spell")?;
graph.connect_objects_str(faction_id, territory_id, "controls_territory")?;
graph.connect_objects_str(npc_id, quest_id, "offers_quest")?;

// Weighted relationships with strings
graph.connect_objects_weighted_str(enemy_id, player_id, "sworn_enemy_of", 0.9)?;

// Edge type creation from strings
let edge_type = EdgeType::from_str("governs"); // Preferred method
let edge = Edge::new(ruler_id, kingdom_id, edge_type);

// Schema-defined edge validation
let edge_schema = EdgeTypeSchema::new(
    "learned_spell".to_string(),
    "Character knowledge of magical spells".to_string(),
)
.with_source_types(vec!["character".to_string()])
.with_target_types(vec!["spell".to_string()])
.with_property("proficiency_level".to_string(), 
    PropertySchema::string("Level of spell mastery"));
```

### Embedding Queue Pattern (Background Processing)
```rust
// Background processing for UI responsiveness
pub async fn embed_text(&self, text: String, chunk_id: ChunkId, object_id: ObjectId) -> Result<RequestId> {
    let request_id = RequestId::new_v4();
    let request = EmbeddingRequest {
        id: request_id,
        text,
        chunk_id,
        object_id,
        response_sender: response_tx,
    };
    
    self.sender.send(QueueMessage::Single(request)).await?;
    Ok(request_id)
}

// Always validate dimensions before insertion
fn validate_embedding(&self, embedding: &[f32]) -> Result<(), VectorError> {
    if embedding.len() != self.expected_dimensions {
        return Err(VectorError::DimensionMismatch {
            expected: self.expected_dimensions,
            actual: embedding.len(),
        });
    }
    Ok(())
}
```

## Testing Patterns

### Unit Test Structure
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    
    fn setup_test_storage() -> (KnowledgeGraph, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let kg = KnowledgeGraph::new(temp_dir.path(), None).unwrap();
        (kg, temp_dir)
    }
    
    #[tokio::test]
    async fn test_node_crud_operations() {
        let (graph, _temp) = setup_test_storage();
        
        // Test creation using ObjectBuilder pattern
        let gandalf_id = ObjectBuilder::character("Gandalf".to_string())
            .with_description("A wise wizard".to_string())
            .add_to_graph(&graph)
            .unwrap();
        
        // Test retrieval
        let retrieved = graph.get_object(gandalf_id).unwrap().unwrap();
        assert_eq!(retrieved.name, "Gandalf");
        
        // Test deletion
        graph.delete_object(gandalf_id).unwrap();
        assert!(graph.get_object(gandalf_id).unwrap().is_none());
    }
}
```

### Mock Provider Pattern for Testing
```rust
struct MockEmbeddingProvider {
    dimensions: usize,
}

#[async_trait]
impl EmbeddingProvider for MockEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        // Return deterministic mock embeddings for testing
        let mut embedding = vec![0.1; self.dimensions];
        embedding[0] = text.len() as f32 / 100.0; // Make it text-dependent
        Ok(embedding)
    }
    
    async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let mut results = Vec::new();
        for text in texts {
            results.push(self.embed(&text).await?);
        }
        Ok(results)
    }
    
    fn dimensions(&self) -> Result<usize> { Ok(self.dimensions) }
    fn max_tokens(&self) -> Result<usize> { Ok(512) }
    // ... other trait methods
}
```

## Vector Search Implementation

### HNSW Integration (Current Working Implementation)
```rust
// Initialize HNSW with proper parameters
let hnsw = Hnsw::<f32, DistL2>::new(
    config.hnsw_m,              // M - number of connections (default: 16)
    config.max_elements,        // capacity
    16,                         // ef_construction (fixed)
    config.hnsw_ef_search,      // ef_search (default: 32)
    DistL2 {}                   // L2 distance function
);

// Insert with proper error handling
pub fn add(&mut self, vector: Vec<f32>) -> Result<usize> {
    if vector.len() != self.dimensions {
        return Err(anyhow!("Vector dimension mismatch"));
    }
    
    let id = self.next_id;
    self.hnsw.insert((&vector, id))?;
    self.metadata.insert(id, vector);
    self.next_id += 1;
    Ok(id)
}

// Search with configurable parameters
pub fn search(&self, query: &[f32], k: usize) -> Result<Vec<(usize, f32)>> {
    if query.len() != self.dimensions {
        return Err(anyhow!("Query dimension mismatch"));
    }
    
    let results = self.hnsw.search(query, k, 32); // ef_search = 32
    Ok(results.into_iter()
        .map(|neighbor| (neighbor.d_id, neighbor.distance))
        .collect())
}
```

### FST Integration for Exact Matching
```rust
// Build FST index for exact name matching
pub fn rebuild_name_index(&mut self, objects: Vec<(String, ObjectId, ObjectType)>) -> Result<()> {
    let mut builder = fst::MapBuilder::memory();
    let mut value_store = Vec::new();
    
    for (i, (name, object_id, object_type)) in objects.iter().enumerate() {
        builder.insert(&name.to_lowercase(), i as u64)?;
        value_store.push(ExactSearchResult {
            object_id: *object_id,
            name: name.clone(),
            object_type: object_type.clone(),
        });
    }
    
    let fst_bytes = builder.into_inner()?;
    let fst = fst::Map::new(fst_bytes)?;
    
    self.name_fst = Some(fst);
    self.fst_value_store = value_store;
    Ok(())
}
```

### Hybrid Search Pattern
```rust
pub async fn search_hybrid(&mut self, query: &str, k: usize) -> Result<HybridSearchResult> {
    // Run semantic search
    let semantic_future = self.search_semantic(query, k);
    
    // Run exact search in parallel
    let exact_results = self.search_exact(query, k);
    
    // Await semantic results
    let semantic_results = semantic_future.await?;
    
    Ok(HybridSearchResult {
        semantic_results,
        exact_results,
    })
}
```

## Embedding Provider Patterns

### FastEmbed Integration
```rust
// Always use proper model initialization
pub fn new(model_type: FastEmbedModel, cache_dir: Option<PathBuf>, show_download_progress: bool) -> Result<Self> {
    let embedding_model = model_type.to_embedding_model();
    let mut init_options = InitOptions {
        model_name: embedding_model.clone(),
        show_download_progress,
        ..Default::default()
    };
    
    if let Some(cache_path) = cache_dir {
        init_options.cache_dir = cache_path;
    }

    let model = TextEmbedding::try_new(init_options)
        .map_err(|e| anyhow!("Failed to initialize FastEmbed model: {}", e))?;
    
    let model_info = TextEmbedding::get_model_info(&embedding_model).clone();
    Ok(Self { model, model_info, model_type })
}

// Batch processing for efficiency
pub async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
    let text_slices: Vec<&str> = texts.iter().map(AsRef::as_ref).collect();
    let embeddings: Vec<FastEmbedEmbedding> = self.model.embed(text_slices, None)
        .map_err(|e| anyhow!("FastEmbed batch embedding failed: {}", e))?;
    Ok(embeddings)
}
```

## Background Processing Patterns

### Embedding Queue Implementation
```rust
// Non-blocking embedding submission
pub async fn embed_text(&self, text: String, chunk_id: ChunkId, object_id: ObjectId) -> Result<RequestId> {
    let request_id = RequestId::new_v4();
    let (response_tx, response_rx) = oneshot::channel();
    
    let request = EmbeddingRequest {
        id: request_id,
        text,
        chunk_id,
        object_id,
        response_sender: response_tx,
    };
    
    self.sender.send(QueueMessage::Single(request)).await
        .map_err(|_| anyhow!("Failed to send embedding request"))?;
    
    Ok(request_id)
}

// Progress tracking pattern
pub async fn try_recv_progress(&self) -> Option<EmbeddingProgress> {
    self.progress_receiver.try_recv().ok()
}

// Worker loop pattern with proper error handling
async fn worker_loop(
    mut receiver: mpsc::Receiver<QueueMessage>,
    provider: Arc<dyn EmbeddingProvider>,
    progress_sender: watch::Sender<Option<EmbeddingProgress>>,
    request_status: Arc<Mutex<HashMap<RequestId, RequestStatus>>>,
) {
    while let Some(message) = receiver.recv().await {
        match message {
            QueueMessage::Single(request) => {
                Self::process_single_request(request, &provider, &progress_sender, &request_status).await;
            }
            QueueMessage::Batch(request) => {
                Self::process_batch_request(request, &provider, &progress_sender, &request_status).await;
            }
            QueueMessage::Cancel(request_id) => {
                // Handle cancellation
            }
            QueueMessage::Shutdown => break,
        }
    }
}
```

## Storage Patterns

### Column Family Usage
```rust
// Always use appropriate column families
const CF_NODES: &str = "nodes";      // ObjectMetadata storage
const CF_CHUNKS: &str = "chunks";    // TextChunk storage  
const CF_EDGES: &str = "edges";      // Edge storage
const CF_NAMES: &str = "names";      // Name-based lookups

// Proper initialization with all column families
pub fn new(path: impl AsRef<Path>) -> Result<Self> {
    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);
    
    let cfs = vec![CF_NODES, CF_CHUNKS, CF_EDGES, CF_NAMES];
    let db = DB::open_cf(&opts, path, cfs)?;
    
    Ok(Self { db })
}
```

### Adjacency List Pattern
```rust
// Efficient edge storage using adjacency lists
fn update_adjacency_lists(&self, edge: &Edge) -> Result<()> {
    // Update outgoing edges for source node
    let mut source_adj = self.get_adjacency_list(edge.from)?;
    source_adj.outgoing.push(edge.to);
    self.store_adjacency_list(edge.from, &source_adj)?;
    
    // Update incoming edges for target node
    let mut target_adj = self.get_adjacency_list(edge.to)?;
    target_adj.incoming.push(edge.from);
    self.store_adjacency_list(edge.to, &target_adj)?;
    
    Ok(())
}
```

## Performance Monitoring Patterns

### Timing Instrumentation
```rust
use std::time::Instant;

// Macro for consistent operation timing
macro_rules! time_operation {
    ($name:expr, $items:expr, $operation:block) => {
        {
            let start = Instant::now();
            let result = $operation;
            let duration = start.elapsed();
            
            tracing::debug!(
                operation = $name,
                duration_ms = duration.as_millis(),
                items = $items,
                items_per_sec = $items as f64 / duration.as_secs_f64(),
                "Operation completed"
            );
            
            result
        }
    };
}

// Usage in embedding operations
pub async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
    time_operation!("embed_batch", texts.len(), {
        let text_slices: Vec<&str> = texts.iter().map(AsRef::as_ref).collect();
        self.model.embed(text_slices, None)
            .map_err(|e| anyhow!("FastEmbed batch embedding failed: {}", e))
    })
}
```

## Build and Environment Patterns

### Environment Configuration
```rust
use std::env;
use std::path::PathBuf;

pub struct Config {
    pub log_level: String,
    pub db_path: PathBuf,
    pub model_cache_dir: PathBuf,
    pub embedding_model: FastEmbedModel,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            log_level: env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
            db_path: env::var("U_FORGE_DB_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("./data")),
            model_cache_dir: env::var("U_FORGE_MODEL_CACHE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("./models")),
            embedding_model: match env::var("U_FORGE_EMBEDDING_MODEL").as_deref() {
                Ok("all-minilm-l6-v2") => FastEmbedModel::AllMiniLmL6V2,
                Ok("bge-base-en-v1.5") => FastEmbedModel::BgeBaseEnV15,
                Ok("bge-large-en-v1.5") => FastEmbedModel::BgeLargeEnV15,
                _ => FastEmbedModel::BgeSmallEnV15, // Default
            },
        }
    }
}
```

### Test Helper Patterns
```rust
// Consistent test cache directory
fn get_test_cache_dir() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test_model_cache");
    std::fs::create_dir_all(&path).expect("Failed to create test model cache dir");
    path
}

// Real provider testing (with caching)
async fn create_real_embedding_manager() -> EmbeddingManager {
    let cache_dir = get_test_cache_dir();
    EmbeddingManager::try_new_local_default(Some(cache_dir))
        .expect("Failed to create real embedding manager")
}

// Temporary directory pattern for isolated tests
fn create_test_graph() -> (KnowledgeGraph, TempDir) {
    let temp_dir = TempDir::new().unwrap();
    let cache_dir = get_test_cache_dir();
    let graph = KnowledgeGraph::new(temp_dir.path(), Some(&cache_dir)).unwrap();
    (graph, temp_dir)
}
```

## Critical Implementation Notes

### Edge Type Flexibility
- **String-based relationships**: All edge types use `EdgeType::Custom(String)` for maximum schema flexibility
- **No enum constraints**: Relationships like `"governs"`, `"led_by"`, `"trades_with"` preserve semantic meaning
- **Schema validation**: Edge type definitions provide constraints without hardcoded enums
- **Backward compatibility**: Legacy enum variants deprecated but functional

### RocksDB Compilation Requirements
- **Always set**: `CC=gcc-13 CXX=g++-13` before building
- **First build**: Takes ~10 minutes due to RocksDB compilation
- **Column families**: Must be created at DB open time, cannot be added later
- **Write batches**: Use for atomic multi-key operations

### FastEmbed Integration
- **Model loading**: First use downloads model to cache (~133MB for BGE-small)
- **Thread safety**: FastEmbed models are Send + Sync, wrap in Arc for sharing
- **Batch processing**: Always prefer `embed_batch()` over multiple `embed()` calls
- **Error handling**: Network errors during model download must be handled gracefully

### HNSW Vector Search
- **Working status**: HNSW integration is functional and tested
- **Parameters**: M=16, ef_construction=16, ef_search=32 provide good balance
- **Memory usage**: Indexes are memory-mapped for efficiency
- **Persistence**: Use `save_to_file()` and `try_load_from_file()` for index persistence

### Async Patterns
- **Background embedding**: Use `EmbeddingQueue` for non-blocking operations
- **Progress tracking**: Implement with `tokio::sync::watch` for UI updates
- **Cancellation**: Use `oneshot::channel` for request/response patterns
- **Backpressure**: EmbeddingQueue implements bounded channels automatically

### Testing Strategy
- **Unit tests**: Use mock providers for fast, deterministic tests
- **Integration tests**: Use temporary directories for RocksDB instances
- **Real provider tests**: Cache models in `target/test_model_cache` for CI efficiency
- **Performance tests**: Include timing assertions for critical paths (embeddings, search)
- **Cross-platform**: Test on Linux primarily, validate others as needed

## CI & Build Infrastructure

### License Compliance
- A CI job `check_qt_license` runs `./scripts/verify_qt_license.sh` and fails if any GPL-only Qt modules are detected.
- The script echoes the resolved `QT_VERSION`, lists linked Qt libraries, and aborts on GPL components unless an explicit `QT_GPL_OK=1` env flag is set.

### Build Cache
- CI enables `sccache` (or `ccache` on macOS) for both Rust (`RUSTC_WRAPPER`) and C++ (`CC`, `CXX` wrappers).  
- Cache keys: `${{ runner.os }}-cargo-${{ hashFiles('**/Cargo.lock') }}` and `${{ runner.os }}-cpp-${{ hashFiles('**/CMakeLists.txt', '**/*.pro') }}`.  
- Saved cache cuts full rebuild time from ~20 min ➜ 4 min.

### Headless Qt Tests
- All QtTest and QML tests run with `QT_QPA_PLATFORM=offscreen`.  
- For OpenGL scenes (e.g., GraphView), the CI image installs `mesa-utils` and sets `LIBGL_ALWAYS_SOFTWARE=1`.

### Generated Code Directory
- Any path matching `cpp/generated/**` is **auto-generated** by CXX-Qt—agents must not edit these files.  
- The directory is listed in `.gitignore` and excluded from lint checks.

### Current Performance Metrics
- **Object insertion**: ~1ms average (RocksDB)
- **Embedding generation**: ~50ms per text chunk (BGE-small on CPU)
- **HNSW search**: ~5-10ms for 10K vectors
- **Memory usage**: ~500MB for demo dataset with embeddings

This document should be updated as implementation patterns evolve. Focus on the "how" of implementation rather than the "what" or "why" of features.