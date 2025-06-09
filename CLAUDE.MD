# CLAUDE.md - u-forge.ai Implementation Guide

## Project Overview

**u-forge.ai** (Universe Forge) is a local-first, cross-platform TTRPG worldbuilding application that combines object-oriented note-taking with AI-powered knowledge graphs. The core application is open source (MIT) and provides essential worldbuilding tools for pen-and-paper GMs, while premium integrations for VTT platforms are offered as separate commercial products.

## Business Model & Feature Separation

**Core Philosophy**: Free for pen-and-paper, premium for digital tools.

### Core Application (MIT License - Always Free)
- Local-first data storage and privacy
- Basic AI integration (user's own API keys)
- Visual knowledge graph with TTRPG schemas
- Semantic search over large datasets
- Local session recording and transcription for in-person table games
- Speaker identification and smart note promotion from session audio
- Import/export capabilities
- Cross-platform desktop application

### Premium Products (Separate Commercial Licenses)
- **Virtual Session Recording**: Enhanced transcription for online games with multi-platform support (Discord voice + FoundryVTT text + Roll20 dice, etc.)
- **Multi-Platform Integration**: Unified session capture that correlates voice chat, text messages, dice rolls, and character actions across different platforms
- **Cloud Sync Service**: Multi-device access, collaboration, shared campaigns
- **U-Store**: Official licensed content marketplace where publishers can offer pre-built knowledge graphs for their copyrighted IP (D&D modules, Pathfinder APs, etc.)
- **Mobile Companion App**: Session notes, quick lookups during games

### Core Value Proposition
- **Local-first**: No servers, user owns their data and API keys
- **AI-augmented**: Context-aware LLM integration for worldbuilding
- **Graph-native**: Visual knowledge graph with TTRPG-specific schemas
- **Performance-first**: Sub-second queries over billion-token datasets
- **Session-aware**: Audio transcription and auto-promotion of campaign notes

## Technical Architecture

### Stack Decision Matrix
- **Core Engine**: Rust (performance, memory safety, cross-platform)
- **UI Framework**: Tauri + Svelte (native performance, web flexibility)
- **Storage**: RocksDB (proven scale, crash recovery, column families)
- **Vector Search**: hnswlib-rs (memory-mapped, hot-swappable indexes)
- **Graph Processing**: Custom CSR + petgraph (on-disk + in-memory hybrid)

### Data Model

```rust
// Core object types
enum ObjectType {
    Character,
    Location,
    Faction,
    Item,
    Event,
    Session,
    CustomType(String)
}

// Storage layout in RocksDB column families
cf_nodes:   ObjectID -> ObjectMetadata (JSON)
cf_chunks:  ChunkID -> TextBlob (≤1000 tokens)
cf_edges:   ObjectID -> CompressedAdjacencyList (CSR format)
cf_names:   FST index for exact proper noun lookups
cf_vectors: ChunkID -> EmbeddingPointer (HNSW index reference)
```

### Query Pipeline (Target: <300ms end-to-end)

1. **ANN Phase** (~2ms): Query HNSW index for top-K semantically similar chunks
2. **Graph Expansion** (~10ms): Multi-get RocksDB for 1-2 hop neighbors via CSR
3. **Context Assembly** (~20ms): Stream chunks through token-aware iterator to 1K budget
4. **LLM Submission**: Direct API call with assembled context

### Write Path & Concurrency

```rust
// Single-writer pattern with async background processing
UI Event -> Write Queue (tokio::mpsc) -> {
    1. Immediate: RocksDB transaction (metadata + chunks + edges)
    2. Async: LLM embedding computation
    3. Completion: HNSW index update + WAL checkpoint
}
```

## Implementation Priorities

### Sprint 1: Storage Foundation ✅ COMPLETED
- [x] RocksDB integration with column families
- [x] Adjacency list edge storage (implemented, CSR for future optimization)
- [x] Crash recovery and WAL checkpointing (built on RocksDB guarantees)
- [x] Core graph operations with comprehensive testing
- [ ] Benchmark against 50GB synthetic dataset

### Sprint 2: Vector Search & Index Management ⚠️ PARTIALLY COMPLETED
- [x] FastEmbed-rs integration with local embedding models (BGE-small-en-v1.5)
- [x] Basic vector search with cosine similarity (HNSW fallback due to API issues)
- [x] FST integration for exact name matching
- [x] Query pipeline implementation with hybrid search
- [x] Enhanced demonstration with semantic search capabilities
- [ ] Memory-mapped HNSW integration (blocked by hnsw_rs API compatibility)
- [ ] Background embedder with backpressure
- [ ] Latency optimization and performance testing

### Sprint 3: Core UI & Graph Canvas
- [ ] Tauri application shell with Svelte frontend
- [ ] Interactive D3.js graph canvas (≤2K nodes initially)
- [ ] CRUD operations for TTRPG objects
- [ ] Keyboard navigation and accessibility

### Sprint 4: AI Integration & Local Session Capture
- [ ] Multi-provider LLM client (OpenAI, Anthropic, local)
- [ ] OS keystore integration for API keys
- [ ] Whisper-cpp audio transcription for local table recording
- [ ] Speaker identification and diarization for in-person sessions
- [ ] Entity detection and automatic note promotion from transcripts
- [ ] Session-to-graph linking (characters mentioned, locations visited, etc.)

## Key Design Decisions & Rationale

### Why RocksDB over sled?
- **Maturity**: Production-proven at scale vs beta software
- **Recovery**: Snapshotting + WAL for sub-second crash recovery
- **Column Families**: Separate hot/cold data with different caching strategies
- **Multi-writer Ready**: Easier CRDT integration in future versions

### Why Memory-Mapped HNSW?
- **Scale**: 2M vectors × 384 dims = ~8GB; must avoid loading into RAM
- **Hot Swap**: Background index rebuilds without UI freeze
- **Persistence**: Survives application restarts without rebuild

### Why Tauri over Electron?
- **Performance**: Native binary vs V8 overhead
- **Size**: ~50MB vs ~200MB distribution
- **Security**: Rust backend vs Node.js attack surface
- **Linux Support**: Better Wayland/X11 compatibility

## Cross-Platform Packaging Strategy

### Linux (Primary)
- **Flatpak**: Main distribution (runtime bundling, universal compatibility)
- **DEB/RPM**: Optional for power users via cargo-deb

### macOS/Windows
- **Native Installers**: DMG/MSI via Tauri bundler
- **Code Signing**: Required for modern OS compatibility
- **Auto-update**: GitHub Releases + Tauri updater

**Note**: Premium integrations will use separate distribution channels (FoundryVTT marketplace, Discord bot directory, etc.)

## AI Integration Architecture

### Local-First Embedding Strategy

**Core Philosophy**: Prioritize local-first solutions with cloud APIs as optional enhancements.

#### Primary Local Embedding Solutions (Ranked by Priority)

1. **FastEmbed-rs** (Recommended Primary)
   - **Pros**: Pure Rust, ONNX-based, no tokio dependency, batch processing with rayon
   - **Models**: 15+ pre-trained models including BGE, E5, Nomic, MiniLM variants
   - **Performance**: Optimized for production use with ONNX Runtime
   - **Size**: Models range from 23MB (all-minilm) to 1.3GB (multilingual-e5-large)
   - **Local**: 100% local inference, no network requirements

2. **Ollama Integration** (Secondary/Optional)
   - **Pros**: User-friendly model management, REST API, supports nomic-embed-text
   - **Models**: nomic-embed-text (274MB), mxbai-embed-large, snowflake-arctic-embed
   - **Performance**: Good for development, may have higher latency than FastEmbed
   - **Dependency**: Requires separate Ollama installation

3. **Cloud APIs** (Tertiary/Optional)
   - OpenAI, Anthropic as fallback options
   - User-provided API keys stored in OS keystore
   - Clear UX indication when using external services

### Multi-Provider Support
```rust
// Multi-tier embedding provider system
trait EmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;
    async fn embed_batch(&self, texts: Vec<&str>) -> Result<Vec<Vec<f32>>>;
    fn dimensions(&self) -> usize;
    fn max_tokens(&self) -> usize;
    fn provider_type(&self) -> EmbeddingProviderType;
}

enum EmbeddingProviderType {
    Local(LocalEmbeddingModel),
    Ollama(OllamaConfig),
    Cloud(CloudProvider),
}

// Primary implementation using FastEmbed-rs
struct FastEmbedProvider {
    model: TextEmbedding,
    model_info: ModelInfo,
}

// Secondary implementation for Ollama users
struct OllamaEmbeddingProvider {
    client: OllamaClient,
    model_name: String,
}

trait LLMProvider {
    async fn generate(&self, prompt: &str) -> Result<String>;
    fn count_tokens(&self, text: &str) -> usize;
}

// Implementations: OpenAI, Anthropic, local llama-cpp
```

### Local Fallback Strategy
- Bundle optional 3-4GB GGUF model for offline "explain node" functionality
- Local Whisper model for session transcription without internet dependency
- Graceful degradation when API keys unavailable
- Clear UX indication of online vs offline capabilities
- Session recording works completely offline for maximum privacy

## Performance Requirements & Testing

### Scale Targets
- **Dataset Size**: 1 billion tokens (~2M chunks)
- **Query Latency**: <300ms context assembly
- **Memory Usage**: <4GB on 8GB systems
- **Storage Footprint**: ~10GB for full dataset + indexes

### Benchmark Scenarios
- Cold start recovery after crash
- Concurrent read/write under high embedding load
- Graph traversal performance on highly connected networks
- Cross-platform UI responsiveness with large datasets

## Security & Privacy Model

### Data Sovereignty
- All user data stored locally in RocksDB
- No telemetry or analytics collection
- Import/export for user-controlled backups

### API Key Management
- OS keystore integration (libsecret/Keychain/CredentialManager)
- Environment variable fallback for headless usage
- Clear audit trail of API usage and costs

## Future Roadmap Considerations

### Sprint 2: Vector Search & Index Management - Implementation Details

#### Revised Sprint 2 Goals

1. **Foundation** (Week 1):
   - FastEmbed-rs integration with BGE-small-en-v1.5 (default)
   - Basic embedding provider trait system
   - Local HNSW index using hnsw_rs

2. **Multi-Provider Support** (Week 2):
   - Ollama integration for advanced users
   - Provider auto-detection and fallback
   - Configuration system for model selection

3. **Search Integration** (Week 3):
   - FST exact matching for TTRPG entities
   - Hybrid search (semantic + exact)
   - Query pipeline with context assembly

4. **Performance & UX** (Week 4):
   - Background embedding with backpressure
   - Progress indicators for model downloads
   - Memory-mapped vector storage

#### Model Selection Strategy

**Default Configuration** (Automatic):
- **Primary**: FastEmbed BGE-small-en-v1.5 (133MB, 384 dimensions)
- **Reasoning**: Good balance of quality, size, and performance
- **Fallback**: all-MiniLM-L6-v2 (80MB, 384 dimensions) if BGE unavailable

**Advanced Configuration** (User Choice):
- **Quality-First**: BGE-large-en-v1.5 (1.3GB, 1024 dimensions)
- **Size-Constrained**: all-MiniLM-L6-v2 (80MB, 384 dimensions)  
- **Multilingual**: multilingual-e5-base (1.1GB, 768 dimensions)
- **Ollama**: nomic-embed-text via local Ollama installation

#### Memory and Performance Targets

**Local Model Constraints**:
- **Model Storage**: 100MB-1.3GB depending on choice
- **Runtime Memory**: <500MB for embedding computation
- **Vector Index**: Memory-mapped to handle large datasets
- **Total Budget**: <2GB additional memory usage on 8GB systems

**Performance Expectations**:
- **FastEmbed**: ~100-500 docs/second on CPU
- **Ollama**: ~50-200 docs/second (varies by model)
- **Hybrid Search**: <300ms end-to-end including graph expansion

### v0.2 Core Enhancements (Open Source)
- WebGL graph canvas for >5K node visualizations
- Git integration for version control
- Enhanced import/export capabilities
- Performance optimizations for massive datasets

### v0.3 Premium Ecosystem (Commercial Products)
- Virtual session recording with multi-platform support (Discord + FoundryVTT + Roll20 + others)
- Cross-platform integration for mixed voice/text/dice streams with temporal correlation
- Cloud sync and collaboration features for distributed teams
- U-Store marketplace launch with partnerships for licensed publisher content
- Mobile companion app for session notes and quick lookups

### U-Store Business Model
The U-Store represents a revenue-sharing marketplace where major publishers can offer official, pre-built knowledge graphs for their copyrighted content:

- **Publishers**: Upload comprehensive knowledge graphs for their settings (Forgotten Realms, Golarion, etc.)
- **Content**: Pre-populated characters, locations, lore, relationships, and canonical information
- **Revenue Split**: Publishers receive majority share, u-forge.ai takes platform fee
- **Integration**: Seamless import into user's local worldbuilding environment
- **Examples**: Official D&D Curse of Strahd knowledge graph, Pathfinder Kingmaker campaign setting, etc.
- **Value Proposition**: Drop-in play for existing IP without hours of manual data entry

## Build Requirements & Environment

### Required Environment Variables
The project requires specific GCC versions for RocksDB compilation:

```bash
export CC=gcc-13
export CXX=g++-13
```

### Build Commands
```bash
# Development build
CC=gcc-13 CXX=g++-13 cargo build

# Run tests
CC=gcc-13 CXX=g++-13 cargo test --lib

# Run demonstration
CC=gcc-13 CXX=g++-13 cargo run
```

### Build Script
A `build.sh` script is provided for convenience:
```bash
chmod +x build.sh
./build.sh
```

## Sprint 1 Implementation Results

### ✅ Completed Core Features
- **RocksDB Storage Engine**: Column families for nodes, edges, chunks, and name indexing
- **Graph Data Model**: TTRPG-specific object types (Character, Location, Faction, Item, Event, Session)
- **Relationship Management**: Weighted edges with metadata and adjacency list storage
- **Text Chunk Support**: Token-aware content storage for future AI integration
- **Query Engine**: Multi-hop graph traversal with configurable depth limits
- **Builder Pattern API**: Fluent interface for creating interconnected worldbuilding objects
- **Comprehensive Testing**: 14 unit and integration tests covering all core functionality

### 🧪 Test Coverage
- Node CRUD operations
- Edge creation and traversal
- Text chunk management
- Subgraph queries
- Node deletion with relationship cleanup
- Graph statistics
- Complex worldbuilding scenarios (Middle-earth demo)

### 📊 Demonstration Results
The working prototype successfully manages a 12-node knowledge graph with:
- 19 relationships between entities
- 3 text chunks (115 tokens)
- Sub-millisecond query performance
- Full relationship integrity

### Key Implementation Decisions Made
1. **Bincode 1.3**: Used for serde compatibility instead of 2.0 (different trait requirements)
2. **Adjacency Lists**: Implemented for edge storage (simpler than CSR, adequate for initial scale)
3. **Cloned Edges**: Edge structs are cloned during insertion to avoid borrow checker issues
4. **Column Family Strategy**: Separate CFs for different data types enable independent optimization
5. **FastEmbed-rs Primary**: Chosen over Ollama/cloud APIs for default embedding provider
6. **Local Model Storage**: Models downloaded to local cache, memory-mapped for efficiency
7. **Hybrid Search**: FST + HNSW combination for comprehensive TTRPG entity retrieval

## Current Implementation Status & Known Issues

### ✅ Completed Components (Prototype State)
- **Storage Engine**: RocksDB-backed knowledge graph with CRUD operations
- **Embeddings**: FastEmbed integration with BGE-small-en-v1.5 model
- **Basic Search**: Simple cosine similarity vector search
- **Exact Matching**: FST (Finite State Transducer) for name-based search
- **Type System**: Complete TTRPG object model (Character, Location, Faction, etc.)
- **Demo Application**: CLI demonstration of core capabilities

### 🚨 Critical Technical Challenges

#### HNSW Vector Search Integration Failure

**Problem Summary**: The initial plan to use `hnsw_rs` v0.3.x for approximate nearest neighbor search encountered extensive API compatibility issues that forced a fallback to simple cosine similarity search.

**Detailed Technical Issues**:

1. **Breaking API Changes in hnsw_rs v0.3.x**:
   ```rust
   // OLD API (expected):
   hnsw.nb_elements()  // -> usize
   hnsw.add_point(vector, id)
   
   // NEW API (actual):
   hnsw.get_nb_point()  // different method name
   hnsw.insert((vector, id))  // different signature
   ```

2. **Lifetime Parameter Requirements**:
   ```rust
   // Expected:
   pub struct VectorSearchEngine {
       hnsw: Arc<RwLock<Hnsw<f32, DistDot>>>,
   }
   
   // Required:
   pub struct VectorSearchEngine<'a> {
       hnsw: Arc<RwLock<Hnsw<'a, f32, DistDot>>>,
   }
   ```
   This lifetime requirement propagated through the entire codebase, requiring extensive refactoring.

3. **Search Result Structure Changes**:
   ```rust
   // Expected fields:
   result.idx  // point ID
   result.dist // distance
   
   // Actual fields:
   result.p_id     // point ID (renamed)
   result.d_id     // data ID (new field)
   result.distance // distance (renamed)
   ```

4. **Serialization Support Removed**:
   - `hnsw_rs::Hnsw` no longer implements `Serialize`/`Deserialize`
   - Index persistence requires custom implementation
   - No built-in memory mapping support

5. **Search API Overhaul**:
   ```rust
   // Expected:
   let results = hnsw.search(&query, k, ef, &mut searcher);
   
   // Actual:
   let results = hnsw.search(&query, k, ef); // 3 params, not 4
   // Searcher pattern completely different
   ```

**Impact**: These issues required ~6 hours of attempted fixes across multiple API versions. The breaking changes were too extensive for a rapid resolution.

**Current Workaround**: 
- Implemented `SimpleVectorStore` with cosine similarity
- Maintains same interface as planned HNSW integration
- Performance acceptable for small-to-medium datasets (< 10K vectors)
- Enables continued development while HNSW integration is resolved

**Recommended Solutions for Future Work**:

1. **Evaluate Alternative Libraries**:
   - `candle-hnsw` (newer, better Rust integration)
   - `hora` (HNSW in Rust with stable API)
   - `hnswlib` (C++ with Rust bindings)

2. **Version Pinning Strategy**:
   - Pin to specific `hnsw_rs` version if API stabilizes
   - Create abstraction layer for vector search to isolate changes

3. **Custom Implementation**:
   - Implement basic HNSW from scratch for full control
   - Focus on TTRPG use cases (small-medium scale, insert-heavy workload)

#### Additional Technical Debt

1. **FST Stream Iteration**: Required importing `Streamer` trait due to API changes
2. **Model Info Access**: FastEmbed `get_model_info()` API inconsistencies
3. **Borrow Checker Issues**: Multiple ownership issues resolved through cloning (performance cost)
4. **Test Coverage**: Current tests use mock providers; real provider tests limited by model download time

### 🔧 Immediate Next Steps for HNSW Resolution

1. **Benchmark Current Performance**: Measure cosine similarity performance vs. required scale
2. **Evaluate Alternatives**: Test `candle-hnsw` and `hora` libraries
3. **API Abstraction**: Create `VectorIndex` trait to isolate implementation details
4. **Performance Testing**: Establish baseline metrics for vector search at different scales

### ⚠️ Development Environment Requirements

**Critical**: The following environment variables are required for RocksDB compilation:
```bash
export CC=gcc-13
export CXX=g++-13
```

**Build Process**: First build takes ~10 minutes due to RocksDB + FastEmbed compilation.

## Implementation Notes for LLM Assistant

When helping implement this system:

1. **Prioritize Correctness**: Data corruption is unacceptable; prefer safe, well-tested patterns
2. **Measure Everything**: Include timing instrumentation from day one  
3. **Error Handling**: Graceful degradation when AI services unavailable
4. **Memory Management**: Profile regularly on 8GB systems
5. **Cross-Platform Testing**: Linux/macOS/Windows compatibility from start
6. **Documentation**: Inline code documentation for complex graph algorithms
7. **Feature Boundaries**: Keep premium features (VTT integrations, cloud sync, advanced AI) out of core codebase
8. **Build Environment**: Always use `CC=gcc-13 CXX=g++-13` for RocksDB compilation compatibility
9. **Dependency Management**: Prefer stable crate versions (bincode 1.3 vs 2.0) for API stability
10. **Testing First**: Implement comprehensive unit tests before moving to next sprint
11. **Local-First Priority**: Implement FastEmbed-rs before cloud APIs; ensure offline functionality
12. **Model Management**: Handle model downloads gracefully with progress indicators and fallbacks
13. **Vector Search Priority**: Resolve HNSW integration before scaling beyond prototype
14. **API Abstraction**: Create stable interfaces to isolate external dependency changes

**Core Product Goal**: A professional-grade local-first application that feels as fast as native tools while providing essential AI-assisted worldbuilding capabilities and comprehensive session recording for in-person tabletop groups—all without compromising user data sovereignty.

**Current Reality**: Early prototype demonstrating core concepts with simplified vector search. Suitable for development and testing, not production use.

**Premium Products Goal**: Seamless multi-platform integrations for online gaming, enhanced virtual session recording that combines voice and text streams, and the U-Store marketplace for official licensed content that enhances the core experience for digital gaming groups while maintaining clear value separation from the free tier.
