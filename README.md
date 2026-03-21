# u-forge.ai (Universe Forge)

> **Your worlds, your data, your way.** A local-first TTRPG worldbuilding tool powered by AI.

![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-lightgrey.svg)
![Status](https://img.shields.io/badge/status-Early%20Prototype-red.svg)

## What is u-forge.ai?

u-forge.ai (Universe Forge) is a **local-first TTRPG worldbuilding tool** that gives game masters a private, AI-assisted knowledge graph for managing worlds — characters, locations, factions, quests, and their relationships — with semantic and exact-name search.

**⚠️ Status: Early prototype. No GUI yet; everything runs as a CLI demo.**

### Vision

- Build rich, interconnected worlds with characters, locations, factions, and lore
- AI-powered semantic search across your entire world
- Session capture with automatic transcription *(planned)*
- Interactive graph visualization *(planned)*
- All data stays local — no cloud dependency

## Current Prototype Features

**What works today:**
- RocksDB-backed knowledge graph with 5 column families (nodes, edges, chunks, names, schemas)
- Local text embeddings via FastEmbed (NomicEmbedTextV15, 768-dim) — **being replaced by Lemonade Server**
- HNSW approximate nearest-neighbor semantic search — **being replaced by sqlite-vec**
- FST-based exact/prefix name matching
- Flexible JSON schema system with validation (13 TTRPG schemas included)
- JSONL data ingestion with two-pass node+edge import
- Async embedding background queue (well-architected but not yet integrated)
- `ObjectBuilder` fluent API for constructing graph objects
- CLI demo with Foundation universe (Asimov) sample data

**Known bugs — see [ARCHITECTURE.md](ARCHITECTURE.md) §Known Bugs for the full list.**

## Architecture Direction

We are migrating away from in-process AI dependencies toward **[Lemonade Server](https://github.com/lemonade-sdk/lemonade)** — an open-source, OpenAI-compatible local AI inference server by AMD that provides hardware-tuned LLM runtimes for Mac, Linux, and Windows.

| Current | Target |
|---|---|
| FastEmbed (in-process ONNX) | Lemonade Server `/api/v1/embeddings` |
| HNSW (`hnsw_rs`) with broken persistence | SQLite + `sqlite-vec` |
| FST prefix matching only | SQLite FTS5 (full-text search) |
| RocksDB (requires gcc-13) | SQLite (bundled, zero system deps) |
| No reranking | Lemonade Server `/api/v1/reranking` |
| No LLM integration | Lemonade Server `/api/v1/chat/completions` |
| CLI demo only | axum HTTP/WebSocket server → web UI |

**See [migration.md](migration.md) for the phased implementation plan.**

## Technical Stack

### Current
- **Language:** Rust (single crate, lib + example)
- **Storage:** RocksDB 0.23 with 5 column families
- **Embeddings:** FastEmbed 5.0 (NomicEmbedTextV15, 768-dim)
- **Vector Search:** hnsw_rs 0.3 (DistL2) + FST 0.4
- **Async:** Tokio 1.45
- **Serialization:** bincode 1.3 (nodes, edges, chunks) + serde_json (schemas)

### Target
- **Language:** Rust (cargo workspace)
- **Storage:** SQLite + FTS5 + sqlite-vec
- **AI Backend:** Lemonade Server (OpenAI-compatible HTTP API)
- **Server:** axum with REST + WebSocket
- **UI:** Web-based (React or Svelte)

## Development Setup

### Prerequisites
- Rust toolchain (stable)
- GCC 13+ (for RocksDB — **will be eliminated after SQLite migration**)
- [Lemonade Server](https://github.com/lemonade-sdk/lemonade) running locally (optional — falls back to FastEmbed)

### Quick Start
```bash
# Set environment (required for RocksDB compilation)
source env.sh

# Build (~10 min first time due to RocksDB)
cargo build

# Run tests
cargo test

# Run CLI demo
cargo run --example cli_demo
```

### Environment Variables
```bash
source env.sh                              # Sets CC, CXX, cache paths
export UFORGE_SCHEMA_DIR="./defaults/schemas"    # Schema directory
export UFORGE_DATA_FILE="./defaults/data/memory.json"  # Data file
export LEMONADE_URL="http://localhost:8000/api/v1"     # Lemonade Server (when available)
```

## Project Layout

```
u-forge.ai/
├── src/
│   ├── lib.rs              # KnowledgeGraph facade + ObjectBuilder
│   ├── types.rs            # Domain types (ObjectMetadata, Edge, TextChunk, etc.)
│   ├── storage.rs          # RocksDB persistence (5 column families)
│   ├── embeddings.rs       # EmbeddingProvider trait + FastEmbed impl
│   ├── vector_search.rs    # HNSW + FST hybrid search engine
│   ├── schema.rs           # Schema definition types
│   ├── schema_manager.rs   # Schema load/validate/cache
│   ├── schema_ingestion.rs # JSON schema files → internal schema
│   ├── data_ingestion.rs   # JSONL import pipeline
│   └── embedding_queue.rs  # Async embedding background queue
├── examples/cli_demo.rs    # CLI demo (the only runnable entry point)
├── defaults/
│   ├── data/memory.json    # Foundation universe JSONL dataset (~220 nodes, ~312 edges)
│   └── schemas/            # 13 TTRPG JSON schema files
├── ARCHITECTURE.md          # Detailed architecture, bugs, and design decisions
├── migration.md            # Phased migration plan (Lemonade + SQLite + UI)
├── Cargo.toml
├── env.sh                  # CRITICAL: source before building
└── dev.sh                  # Dev runner script
```

## Key Documentation

| Document | Purpose |
|---|---|
| [ARCHITECTURE.md](ARCHITECTURE.md) | Current architecture, known bugs, design decisions |
| [migration.md](migration.md) | Phased migration plan to Lemonade Server + SQLite + web UI |
| [.cursor/rules/](/.cursor/rules/) | AI assistant context rules (7 files) |

## Sample Data

The included Foundation universe dataset (`defaults/data/memory.json`) contains ~220 nodes and ~312 edges modeling Isaac Asimov's Foundation series — characters, locations, factions, quests, and artifacts with rich metadata.

## License

MIT License. Your worlds belong to you.