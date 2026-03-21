# u-forge.ai (Universe Forge)

> **Your worlds, your data, your way.** A local-first TTRPG worldbuilding tool powered by AI.

![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-lightgrey.svg)
![Status](https://img.shields.io/badge/status-Early%20Prototype-orange.svg)

## What is u-forge.ai?

A **local-first TTRPG worldbuilding tool** that gives game masters a private, AI-assisted knowledge graph for managing worlds — characters, locations, factions, quests, and their relationships — with full-text and semantic search.

**⚠️ Status: Early prototype. No GUI yet; everything runs as a CLI demo.**

---

## Current State

| Area | Status |
|---|---|
| SQLite knowledge graph (nodes, edges, chunks, schemas) | ✅ Working |
| Full-text search via SQLite FTS5 | ✅ Working |
| Flexible JSON schema system with validation (13 TTRPG types) | ✅ Working |
| JSONL data ingestion — two-pass node + edge import, deduplication | ✅ Working |
| `ObjectBuilder` fluent API | ✅ Working |
| Lemonade Server embedding provider (`EmbeddingManager`) | ✅ Working |
| Lemonade Server transcription provider (`TranscriptionManager`) | ✅ Working |
| Hardware device abstraction (NPU / GPU / CPU) | ✅ Working |
| Unified inference queue (`InferenceQueue`) — embed, transcribe, TTS, LLM, rerank | ✅ Working |
| Reranking via Lemonade Server (`LemonadeRerankProvider`) | ✅ Working |
| `cli_demo` — FTS5 search + rerank pipeline demo with Foundation universe data | ✅ Working |
| sqlite-vec ANN vector search | 🔜 Deferred |
| axum HTTP / WebSocket server | 🔜 Planned |
| Reranking wired into KG search pipeline (hybrid FTS5 + vec + rerank) | 🔜 Planned |
| Web UI | 🔜 Planned |

---

## Technical Stack

| Layer | Technology |
|---|---|
| Language | Rust (single crate, lib + CLI example) |
| Storage | SQLite via `rusqlite` (bundled — zero system deps) |
| Full-text search | SQLite FTS5 |
| Vector search | *(deferred — sqlite-vec planned)* |
| Embeddings / LLM / Reranking / TTS / STT | [Lemonade Server](https://github.com/lemonade-sdk/lemonade) HTTP API (optional) |
| Async runtime | Tokio 1.x |
| Serialization | serde\_json |

---

## Quick Start

```bash
# Build (~30 s first time — compiles bundled SQLite)
cargo build

# Run all tests (no server required)
cargo test -- --test-threads=1

# Run the CLI demo with Foundation universe sample data
cargo run --example cli_demo
```

No `source env.sh`. No gcc-13. No model downloads required to build and test.

### With Lemonade Server (embeddings + reranking + LLM + transcription)

```bash
# Install and start Lemonade Server
sudo snap install lemonade-server        # Linux

# Pull models you want to use
lemonade-server pull embed-gemma-300m-FLM      # embeddings (NPU, 0.62 GB)
lemonade-server pull bge-reranker-v2-m3-GGUF   # reranking (GPU/CPU)
lemonade-server pull whisper-v3-turbo-FLM      # transcription (NPU, 1.55 GB)

lemonade-server serve                    # leave running

# u-forge.ai auto-discovers Lemonade on localhost:8000
# Set LEMONADE_URL only to override (e.g. non-standard port):
export LEMONADE_URL="http://localhost:8000/api/v1"

cargo run --example cli_demo
```

The CLI demo will detect hardware capabilities, list available models, run FTS5
search over the Foundation universe dataset, and — when a reranker model is
available — demonstrate the FTS5 → rerank pipeline.

### Environment Variables

| Variable | Default | Purpose |
|---|---|---|
| `LEMONADE_URL` | *(auto-detected)* | Override Lemonade Server base URL |
| `UFORGE_SCHEMA_DIR` | `./defaults/schemas` | Directory of `.schema.json` files |
| `UFORGE_DATA_FILE` | `./defaults/data/memory.json` | JSONL data file for import |
| `RUST_LOG` | `error` | Log verbosity (`error`/`warn`/`info`/`debug`/`trace`) |

---

## Project Layout

```
u-forge.ai/
├── src/
│   ├── lib.rs              # KnowledgeGraph facade + ObjectBuilder
│   ├── types.rs            # Domain types
│   ├── storage.rs          # SQLite persistence + FTS5
│   ├── embeddings.rs       # EmbeddingProvider trait + LemonadeProvider
│   ├── transcription.rs    # TranscriptionProvider trait + LemonadeTranscriptionProvider
│   ├── hardware/           # DeviceCapability, NpuDevice, GpuDevice, CpuDevice
│   ├── inference_queue.rs  # Unified MPMC inference dispatch (embed/transcribe/TTS/LLM/rerank)
│   ├── lemonade.rs         # LemonadeModelRegistry, GpuResourceManager, LemonadeRerankProvider,
│   │                       #   LemonadeChatProvider, LemonadeTtsProvider, LemonadeSttProvider,
│   │                       #   LemonadeStack, SystemInfo, LemonadeCapabilities
│   ├── embedding_queue.rs  # Legacy embedding-only background queue
│   ├── schema.rs           # Schema definition types
│   ├── schema_manager.rs   # Schema load / validate / cache
│   ├── schema_ingestion.rs # JSON schema files → internal representation
│   └── data_ingestion.rs   # JSONL two-pass import pipeline
├── examples/
│   └── cli_demo.rs         # Demo + integration test: hardware caps, FTS5, reranking
├── defaults/
│   ├── data/memory.json    # Foundation universe JSONL (~220 nodes, ~312 edges)
│   └── schemas/            # 13 TTRPG JSON schema files
├── .rulesdir/              # AI assistant context rules
├── ARCHITECTURE.md         # Architecture reference and design decisions
├── Cargo.toml
└── env.sh                  # Optional: sets UFORGE_* path variables
```

Source files are thoroughly commented — refer to them directly for implementation
details rather than this document.

---

## Key Documentation

| Document | Purpose |
|---|---|
| [ARCHITECTURE.md](ARCHITECTURE.md) | Module map, SQLite schema, hardware architecture, inference queue, design decisions |
| [.rulesdir/](.rulesdir/) | AI assistant context rules (7 `.mdc` files) |

---

## Sample Data

`defaults/data/memory.json` models Isaac Asimov's **Foundation** universe (~220 nodes,
~312 edges). Used by `cli_demo` for end-to-end testing of the full pipeline: schema
load → data import → FTS5 indexing → search → rerank.

---

## License

MIT License. Your worlds belong to you.