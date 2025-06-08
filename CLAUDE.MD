# CLAUDE.md - u-forge.ai Implementation Guide

## Project Overview

**u-forge.ai** (Universe Forge) is a local-first, cross-platform TTRPG worldbuilding application that combines object-oriented note-taking with AI-powered knowledge graphs. It fills a critical market gap by providing deep LLM integration, semantic search over massive datasets (1B+ tokens), and real-time session capture—all while keeping user data completely local.

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

### Sprint 1: Storage Foundation
- [ ] RocksDB integration with column families
- [ ] Compressed sparse row (CSR) edge storage
- [ ] Crash recovery and WAL checkpointing
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

### Sprint 4: AI Integration & Session Capture
- [ ] Multi-provider LLM client (OpenAI, Anthropic, local)
- [ ] OS keystore integration for API keys
- [ ] Whisper-cpp audio transcription
- [ ] Entity detection and node promotion workflow

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
- Graceful degradation when API keys unavailable
- Clear UX indication of online vs offline capabilities

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

### v0.2 Enhancements
- WebGL graph canvas for >5K node visualizations
- Advanced prompt templating system
- Git integration for version control
- Mobile companion app for session notes

### v0.3 Collaboration Features
- CRDT-based multi-user sync
- Read-only sharing links for players
- Collaborative session editing

## Implementation Notes for LLM Assistant

When helping implement this system:

1. **Prioritize Correctness**: Data corruption is unacceptable; prefer safe, well-tested patterns
2. **Measure Everything**: Include timing instrumentation from day one
3. **Error Handling**: Graceful degradation when AI services unavailable
4. **Memory Management**: Profile regularly on 8GB systems
5. **Cross-Platform Testing**: Linux/macOS/Windows compatibility from start
6. **Documentation**: Inline code documentation for complex graph algorithms

The goal is a professional-grade local-first application that feels as fast as native tools while providing AI capabilities that rival cloud services—all without compromising user data sovereignty.
