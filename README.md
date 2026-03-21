# u-forge.ai (Universe Forge)

> **Your worlds, your data, your way.** A local-first TTRPG worldbuilding tool powered by AI.

![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-lightgrey.svg)
![Status](https://img.shields.io/badge/status-Early%20Prototype-orange.svg)

## What is u-forge.ai?

u-forge.ai (Universe Forge) is a **local-first TTRPG worldbuilding tool** that gives game masters a
private, AI-assisted knowledge graph for managing worlds — characters, locations, factions, quests,
and their relationships — with full-text and semantic search.

**⚠️ Status: Early prototype. No GUI yet; everything runs as a CLI demo.**

### Vision

- Build rich, interconnected worlds with characters, locations, factions, and lore
- AI-powered semantic search across your entire world (via Lemonade Server)
- Session capture with automatic transcription *(planned)*
- Interactive graph visualization via web UI *(planned)*
- All data stays local — no cloud dependency

---

## Current Features

| Area | Status |
|---|---|
| SQLite knowledge graph (nodes, edges, chunks, schemas) | ✅ Working |
| Full-text search via SQLite FTS5 | ✅ Working |
| Flexible JSON schema system with validation (13 TTRPG schemas) | ✅ Working |
| JSONL data ingestion — two-pass node + edge import | ✅ Working |
| Import deduplication (BUG-6 fix) | ✅ Working |
| Cross-session edge resolution (BUG-7 fix) | ✅ Working |
| `ObjectBuilder` fluent API | ✅ Working |
| Async embedding background queue | ✅ Working (not yet wired to storage) |
| Lemonade Server embedding provider | ✅ Implemented (requires running server) |
| Semantic vector search (sqlite-vec) | 🔜 Phase 2 (FTS5 available now) |
| axum HTTP / WebSocket server | 🔜 Phase 3 |
| Lemonade reranking | 🔜 Phase 4 |
| Web UI | 🔜 Phase 5+ |

---

## Technical Stack

### Current

| Layer | Technology |
|---|---|
| Language | Rust (single crate, lib + CLI example) |
| Storage | SQLite via `rusqlite` (bundled — zero system deps) |
| Full-text search | SQLite FTS5 (built-in virtual table) |
| Semantic search | *(deferred — sqlite-vec planned)* |
| Embeddings | [Lemonade Server](https://github.com/lemonade-sdk/lemonade) HTTP API (optional) |
| Async runtime | Tokio 1.x |
| Serialization | serde\_json (all persistence) |

### Target (remaining phases)

| Layer | Technology |
|---|---|
| Vector search | sqlite-vec extension |
| HTTP server | axum + tower-http |
| Reranking | Lemonade Server `/api/v1/reranking` |
| LLM generation | Lemonade Server `/api/v1/chat/completions` |
| UI | Web (React or Svelte) |

---

## Development Setup

### Prerequisites

- **Rust** stable toolchain — [rustup.rs](https://rustup.rs)
- A C compiler (`gcc`, `clang`, or MSVC) — only needed for `openssl-sys` (reqwest TLS). Any modern version works; **gcc-13 is no longer required**.
- *(Optional)* [Lemonade Server](https://github.com/lemonade-sdk/lemonade) for semantic embedding

### Quick Start

```bash
# Build (first time: compiles bundled SQLite — ~30 seconds, not 10 minutes)
cargo build

# Run all tests (no server required)
cargo test

# Run the CLI demo with Foundation universe sample data
cargo run --example cli_demo
```

That's it. No `source env.sh`, no gcc-13, no model downloads required to build and test.

### With Lemonade Server (for semantic embeddings)

```bash
# Install and start Lemonade Server
sudo snap install lemonade-server        # Linux
lemonade-server pull nomic-embed-text
lemonade-server serve                    # Leave running in a separate terminal

# Point u-forge.ai at it
export LEMONADE_URL="http://localhost:8000/api/v1"

cargo run --example cli_demo
```

### Environment Variables

| Variable | Default | Purpose |
|---|---|---|
| `LEMONADE_URL` | *(unset)* | Lemonade Server base URL for embeddings |
| `UFORGE_SCHEMA_DIR` | `./defaults/schemas` | Directory of `.schema.json` files |
| `UFORGE_DATA_FILE` | `./defaults/data/memory.json` | JSONL data file for import |
| `RUST_LOG` | `error` | Log verbosity (`error`/`warn`/`info`/`debug`/`trace`) |

---

## Project Layout

```
u-forge.ai/
├── src/
│   ├── lib.rs              # KnowledgeGraph facade + ObjectBuilder
│   ├── types.rs            # Domain types (ObjectMetadata, Edge, TextChunk, …)
│   ├── storage.rs          # SQLite persistence (KnowledgeGraphStorage)
│   ├── embeddings.rs       # EmbeddingProvider trait + LemonadeProvider
│   ├── embedding_queue.rs  # Async background embedding queue
│   ├── schema.rs           # Schema definition types
│   ├── schema_manager.rs   # Schema load / validate / cache
│   ├── schema_ingestion.rs # JSON schema files → internal representation
│   └── data_ingestion.rs   # JSONL two-pass import pipeline
├── examples/
│   └── cli_demo.rs         # CLI demo — the only runnable entry point
├── defaults/
│   ├── data/memory.json    # Foundation universe JSONL dataset (~220 nodes, ~312 edges)
│   └── schemas/            # 13 TTRPG JSON schema files
├── .rulesdir/              # AI assistant context rules (7 files)
├── ARCHITECTURE.md         # Architecture reference, bug history, design decisions
├── migration.md            # Phased migration plan (Phases 1+2 complete)
├── Cargo.toml
└── env.sh                  # Optional: sets UFORGE_* path variables
```

---

## API Overview

### KnowledgeGraph

The main facade. Create one per database directory:

```rust
use u_forge_ai::KnowledgeGraph;

let graph = KnowledgeGraph::new("./data/my_world")?;
```

| Method | Description |
|---|---|
| `add_object(metadata)` | Persist a new node |
| `get_object(id)` | Retrieve by UUID |
| `get_all_objects()` | All nodes |
| `update_object(metadata)` | Overwrite (bumps `updated_at`) |
| `delete_object(id)` | Delete + cascade edges/chunks |
| `connect_objects_str(from, to, edge_type)` | Create a typed edge |
| `get_relationships(id)` | All edges for a node |
| `get_neighbors(id)` | 1-hop neighbour IDs |
| `add_text_chunk(id, content, type)` | Attach searchable text |
| `search_chunks_fts(query, limit)` | FTS5 full-text search |
| `find_by_name(type, name)` | Exact name lookup (scoped to type) |
| `find_by_name_only(name)` | Exact name lookup (all types) |
| `query_subgraph(id, max_hops)` | BFS subgraph traversal |
| `get_stats()` | Node / edge / chunk counts |
| `validate_object(obj)` | Schema validation |
| `add_object_validated(obj)` | Validate then persist |

### ObjectBuilder

Fluent API for constructing objects:

```rust
use u_forge_ai::ObjectBuilder;

let id = ObjectBuilder::character("Hari Seldon".to_string())
    .with_description("Mathematician and founder of psychohistory.".to_string())
    .with_property("affiliation".to_string(), "Galactic Empire".to_string())
    .with_tag("mathematician".to_string())
    .add_to_graph(&graph)?;
```

Convenience constructors: `character`, `location`, `faction`, `item`, `event`, `session`, `custom`.

### Embedding (optional)

```rust
use u_forge_ai::EmbeddingManager;

// Reads LEMONADE_URL from environment, errors if not set
let mgr = EmbeddingManager::try_new_auto(None, None).await?;
let vec = mgr.get_provider().embed("A wise old wizard").await?;
```

---

## Sample Data

`defaults/data/memory.json` contains ~220 nodes and ~312 edges modelling Isaac Asimov's
**Foundation** universe — characters, locations, factions, artifacts, and lore — with rich
metadata. It is used by the CLI demo for end-to-end testing.

```bash
# Run the demo and watch FTS5 search over the Foundation universe
cargo run --example cli_demo
```

---

## Key Documentation

| Document | Purpose |
|---|---|
| [ARCHITECTURE.md](ARCHITECTURE.md) | SQLite schema, module map, resolved bugs, design decisions |
| [migration.md](migration.md) | Phased migration plan (Phases 1 + 2 complete) |
| [.rulesdir/](.rulesdir/) | AI assistant context rules (7 `.mdc` files) |

---

## License

MIT License. Your worlds belong to you.