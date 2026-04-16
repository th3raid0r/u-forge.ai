# u-forge.ai (Universe Forge)

> **Your worlds, your data, your way.** A local-first TTRPG worldbuilding tool powered by AI.

![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-lightgrey.svg)
![Status](https://img.shields.io/badge/status-Alpha-yellow.svg)

## What is u-forge.ai?

A **local-first TTRPG worldbuilding tool** that gives game masters a private, AI-assisted knowledge graph for managing worlds — characters, locations, factions, quests, and their relationships — with full-text and semantic search.

**Alpha UI now available.** The native desktop application is built on [GPUI](https://gpui.rs/) (the GPU-accelerated UI framework from the Zed editor). It provides a graph canvas with pan/zoom, a schema-driven node editor, FTS5/semantic/hybrid search, and a streaming LLM chat panel — all running locally with no cloud dependency.

```bash
# Launch the UI
cargo run -p u-forge-ui-gpui
```

---

## Current State

| Area | Status |
|---|---|
| **Native desktop UI (GPUI)** — graph canvas, node editor, search, chat | **Alpha** |
| SQLite knowledge graph (nodes, edges, chunks, schemas) | Working |
| Full-text search via SQLite FTS5 | Working |
| Semantic (vector) search via sqlite-vec ANN (`chunks_vec` vec0 table) | Working |
| Hybrid search — FTS5 + ANN + RRF merge + optional rerank | Working |
| Flexible JSON schema system with validation (13 TTRPG types) | Working |
| JSONL data ingestion — two-pass node + edge import, deduplication | Working |
| Catalog-driven model selection (`LemonadeServerCatalog`, `ModelSelector`) | Working |
| Unified inference queue (`InferenceQueue`) — embed, transcribe, TTS, LLM, rerank | Working |
| Streaming LLM chat with thinking/reasoning separation | Working |
| `cli_demo` — hybrid search + rerank pipeline demo | Working |
| `cli_chat` — interactive RAG chat REPL (hybrid search + LLM) | Working |
| TypeScript agentic sandbox (`deno_core` V8 embedding) | Planned |
| Agentic workflows (AI-driven graph operations) | Planned |
| HTTP / WebSocket server + web UI | Distant |

---

## Technical Stack

| Layer | Technology |
|---|---|
| Language | Rust (multi-crate Cargo workspace) |
| Desktop UI | [GPUI 0.2.2](https://gpui.rs/) (Zed editor's GPU-accelerated framework) |
| Storage | SQLite via `rusqlite` (bundled — zero system deps) |
| Full-text search | SQLite FTS5 |
| Vector search | sqlite-vec ANN (`vec0` virtual table, cosine distance) |
| Graph layout | Force-directed (grid-cell bucketed O(N) repulsion) + R-tree spatial index |
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

# Launch the native UI
cargo run -p u-forge-ui-gpui
```

No `source env.sh`. No model downloads required to build and test.

### The Desktop UI

The GPUI app provides:

- **Graph canvas** — pan/zoom visualization of the knowledge graph with LOD rendering, type-colored nodes, edge rendering, and a legend. Drag nodes to rearrange; positions are persisted to SQLite.
- **Schema-driven node editor** — browser-style tabs with form fields generated from JSON schemas. Supports text, number, boolean, enum, and array fields. Dirty-state tracking with orange tab indicators; Ctrl+S saves all.
- **Tree panel** — collapsible sidebar listing all nodes grouped by type, with selection synced to the canvas and editor.
- **Search panel** — FTS5 (always available), semantic, and hybrid search modes. Semantic/hybrid require Lemonade Server.
- **Chat panel** — streaming LLM chat with model selector, enter-to-submit toggle, and thinking/reasoning token separation. Requires Lemonade Server with an LLM model.
- **Resizable panels** — all panel boundaries are draggable; double-click to reset.

### With Lemonade Server (embeddings + reranking + LLM + chat)

```bash
# Install and start Lemonade Server
sudo snap install lemonade-server        # Linux

# Pull models you want to use
lemonade-server pull embed-gemma-300m-FLM      # embeddings (NPU, 0.62 GB)
lemonade-server pull bge-reranker-v2-m3-GGUF   # reranking (GPU/CPU)
lemonade-server pull GLM-4.7-Flash-GGUF        # LLM for chat (GPU)

lemonade-server serve                    # leave running
```

#### Adding the CPU/GPU embedding model (required for hybrid search)

The NPU embedding model (`embed-gemma-300m-FLM`) only runs on systems with an
AMD NPU.  For CPU/GPU embedding — needed for hybrid search on all other
hardware — you must manually add `embeddinggemma-300M-GGUF` via the
**Lemonade UI**:

1. Open the Lemonade Server UI (default: `http://localhost:13305`)
2. Navigate to **Models** and click **Add Custom Model**
3. Enter the HuggingFace checkpoint: `ggml-org/embeddinggemma-300M-GGUF:Q8_0`
4. Select the **llamacpp** recipe and add the **embeddings** label
5. Save — the model will appear as `user.ggml-org/embeddinggemma-300M-GGUF`

This is the same model family as the NPU variant, so NPU and CPU/GPU workers
produce vectors in the same embedding space.  Using a different embedding model
(e.g. nomic) alongside the NPU gemma model will produce incorrect search
results.

```bash
# u-forge.ai auto-discovers Lemonade on localhost:13305
# Set LEMONADE_URL only to override (e.g. non-standard port):
export LEMONADE_URL="http://localhost:13305/api/v1"

cargo run -p u-forge-ui-gpui
```

The UI auto-discovers Lemonade Server on startup. When connected, semantic/hybrid search and the chat panel become available. When not connected, the app continues with FTS5-only search and the chat panel shows a connection message.

### CLI demos

```bash
# CLI demo — hardware caps, FTS5, semantic, reranking
cargo run --manifest-path crates/u-forge-core/Cargo.toml --example cli_demo

# Interactive RAG chat REPL
cargo run --manifest-path crates/u-forge-core/Cargo.toml --example cli_chat
```

**`cli_chat` REPL commands:**

| Command | Effect |
|---|---|
| `/quit` | Exit |
| `/clear` | Reset conversation history |
| `/context` | Toggle display of retrieved knowledge graph nodes |

**Graceful degradation:**

| Scenario | Behaviour |
|---|---|
| No Lemonade Server | UI works with FTS5-only search; chat panel shows connection message |
| Lemonade running but no LLM model | Search works; chat unavailable |
| LLM available but no embedding model | Chat works with FTS5-only search |
| Full stack (embedding + LLM + reranker) | Hybrid search + rerank + streaming LLM chat |

### Environment Variables

| Variable | Default | Purpose |
|---|---|---|
| `LEMONADE_URL` | *(auto-detected)* | Override Lemonade Server base URL |
| `UFORGE_SCHEMA_DIR` | `./defaults/schemas` | Directory of `.schema.json` files |
| `UFORGE_DATA_FILE` | `./defaults/data/memory.json` | JSONL data file for import |
| `RUST_LOG` | `error` | Log verbosity (`error`/`warn`/`info`/`debug`/`trace`) |

### Device Configuration

Device weights and enable/disable state are controlled by an optional TOML file at:
- `./u-forge.toml` (current directory)
- `$XDG_CONFIG_HOME/u-forge/config.toml` (or `~/.config/u-forge/config.toml`)

If no file exists, defaults are used (all devices enabled: NPU=100, GPU=50, CPU=10).

Example `u-forge.toml`:
```toml
[embedding]
npu_enabled = true
high_quality_embedding = true
gpu_enabled = true
gpu_weight = 40
cpu_enabled = false
cpu_weight = 10

[chat]
preferred_device = "gpu"
system_prompt = "You are a knowledgeable assistant..."

[chat.gpu]
model = "Gemma-4-26B-A4B-it-GGUF"
max_tokens = 262144
```

---

## Project Layout

```
u-forge.ai/
├── crates/
│   ├── u-forge-core/       # All core logic (lib)
│   │   ├── src/
│   │   │   ├── lib.rs              # KnowledgeGraph facade + re-exports
│   │   │   ├── builder.rs          # ObjectBuilder fluent API
│   │   │   ├── text.rs             # split_text() word-boundary chunking
│   │   │   ├── types.rs            # Domain types
│   │   │   ├── graph/              # SQLite persistence + FTS5 + ANN
│   │   │   ├── ai/                 # EmbeddingProvider + TranscriptionProvider traits + re-exports
│   │   │   ├── queue/              # Unified MPMC inference dispatch (embed/transcribe/TTS/LLM/rerank)
│   │   │   ├── lemonade/           # Catalog, ModelSelector, ProviderFactory, all Lemonade providers
│   │   │   ├── schema/             # Schema definition types, load/validate/cache, JSON ingestion
│   │   │   ├── ingest/             # JSONL two-pass import pipeline
│   │   │   ├── rag.rs              # RAG context formatting + message assembly
│   │   │   └── search/             # Hybrid FTS5 + ANN + rerank search pipeline
│   │   └── examples/
│   │       ├── common/             # Shared helpers (config, args, KG setup, embedding)
│   │       ├── cli_demo.rs         # Demo: hardware caps, FTS5, reranking
│   │       └── cli_chat.rs         # Interactive RAG chat REPL
│   ├── u-forge-graph-view/ # Graph view model + layout engine
│   ├── u-forge-ui-traits/  # Framework-agnostic rendering contracts
│   ├── u-forge-ui-gpui/    # GPUI native desktop app (Alpha)
│   └── u-forge-ts-runtime/ # Embedded deno_core TypeScript sandbox (skeleton — see feature_TS-Agent-Sandbox.md)
├── defaults/
│   ├── data/memory.json    # Foundation universe JSONL (~220 nodes, ~312 edges)
│   └── schemas/            # 13 TTRPG JSON schema files
├── .rulesdir/              # AI assistant context rules
├── ARCHITECTURE.md         # Architecture reference and design decisions
├── Cargo.toml              # Workspace root
└── env.sh                  # Optional: sets UFORGE_* path variables
```

Source files are thoroughly commented — refer to them directly for implementation
details rather than this document.

---

## Key Documentation

| Document | Purpose |
|---|---|
| [ARCHITECTURE.md](ARCHITECTURE.md) | Module map, SQLite schema, hardware architecture, inference queue, design decisions |
| [feature_UI.md](feature_UI.md) | Native GPUI desktop UI — design, implementation, module structure |
| [feature_TS-Agent-Sandbox.md](feature_TS-Agent-Sandbox.md) | TypeScript agentic sandbox design |
| [.rulesdir/](.rulesdir/) | AI assistant context rules (7 `.mdc` files) |

---

## Roadmap

1. **Native desktop UI (GPUI)** — Alpha complete. Graph canvas, node editor, search, chat.
2. **TypeScript agentic sandbox** — Embed a sandboxed V8 runtime via `deno_core` for AI-driven graph operations.
3. **Agentic workflows** — AI agents that can query, create, and modify knowledge graph nodes via TypeScript programs.
4. **Polish and stabilization** — UI refinement, performance tuning, error handling improvements.
5. **HTTP server + web UI** — Distant. Requires core feature set to stabilize first.

---

## Sample Data

`defaults/data/memory.json` models Isaac Asimov's **Foundation** universe (~220 nodes,
~312 edges). Used by the UI and CLI demos for end-to-end testing of the full pipeline:
schema load, data import, FTS5 indexing, search, rerank.

---

## License

MIT License. Your worlds belong to you.
