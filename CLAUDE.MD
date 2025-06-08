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

### Sprint 2: Vector Search & Index Management
- [ ] Memory-mapped HNSW with hot-swap rebuild capability
- [ ] Background embedder with backpressure
- [ ] FST integration for exact name matching
- [ ] Query pipeline implementation and latency testing

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

### Multi-Provider Support
```rust
trait LLMProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;
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

### 🔧 Key Implementation Decisions Made
1. **Bincode 1.3**: Used for serde compatibility instead of 2.0 (different trait requirements)
2. **Adjacency Lists**: Implemented for edge storage (simpler than CSR, adequate for initial scale)
3. **Cloned Edges**: Edge structs are cloned during insertion to avoid borrow checker issues
4. **Column Family Strategy**: Separate CFs for different data types enable independent optimization

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

**Core Product Goal**: A professional-grade local-first application that feels as fast as native tools while providing essential AI-assisted worldbuilding capabilities and comprehensive session recording for in-person tabletop groups—all without compromising user data sovereignty.

**Premium Products Goal**: Seamless multi-platform integrations for online gaming, enhanced virtual session recording that combines voice and text streams, and the U-Store marketplace for official licensed content that enhances the core experience for digital gaming groups while maintaining clear value separation from the free tier.
