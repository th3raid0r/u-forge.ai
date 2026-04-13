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
| Semantic (vector) search via sqlite-vec ANN (`chunks_vec` vec0 table) | ✅ Working |
| Hybrid search — FTS5 + ANN + RRF merge + optional rerank (`src/search/`) | ✅ Working |
| Flexible JSON schema system with validation (13 TTRPG types) | ✅ Working |
| JSONL data ingestion — two-pass node + edge import, deduplication | ✅ Working |
| `ObjectBuilder` fluent API | ✅ Working |
| Catalog-driven model selection (`LemonadeServerCatalog`, `ModelSelector`) | ✅ Working |
| Lemonade Server embedding provider (`LemonadeProvider`) | ✅ Working |
| Lemonade Server transcription provider (`LemonadeTranscriptionProvider`) | ✅ Working |
| Unified inference queue (`InferenceQueue`) — embed, transcribe, TTS, LLM, rerank | ✅ Working |
| Reranking via Lemonade Server (`LemonadeRerankProvider`) | ✅ Working |
| `cli_demo` — hybrid search + rerank pipeline demo with Foundation universe data | ✅ Working |
| `cli_chat` — interactive RAG chat REPL (hybrid search + LLM) | ✅ Working |
| axum HTTP / WebSocket server | 🔜 Planned |
| Streaming LLM responses | 🔜 Planned |
| Web UI | 🔜 Planned |

---

## Technical Stack

| Layer | Technology |
|---|---|
| Language | Rust (multi-crate Cargo workspace) |
| Storage | SQLite via `rusqlite` (bundled — zero system deps) |
| Full-text search | SQLite FTS5 |
| Vector search | sqlite-vec ANN (`vec0` virtual table, cosine distance) |
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
cargo run --manifest-path crates/u-forge-core/Cargo.toml --example cli_demo
```

No `source env.sh`. No model downloads required to build and test.

### With Lemonade Server (embeddings + reranking + LLM + transcription)

```bash
# Install and start Lemonade Server
sudo snap install lemonade-server        # Linux

# Pull models you want to use
lemonade-server pull embed-gemma-300m-FLM      # embeddings (NPU, 0.62 GB)
lemonade-server pull bge-reranker-v2-m3-GGUF   # reranking (GPU/CPU)
lemonade-server pull whisper-v3-turbo-FLM      # transcription (NPU, 1.55 GB)

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

cargo run --manifest-path crates/u-forge-core/Cargo.toml --example cli_demo
```

The CLI demo will detect hardware capabilities, list available models, run FTS5
search over the Foundation universe dataset, and — when a reranker model is
available — demonstrate the FTS5 → rerank pipeline.

### Interactive RAG chat (`cli_chat`)

`cli_chat` is an interactive REPL that grounds every response in the knowledge
graph.  It requires an LLM model in addition to the embedding and reranker.

```bash
# Pull an LLM (if you haven't already)
lemonade-server pull GLM-4.7-Flash-GGUF

# Start the chat demo
cargo run --manifest-path crates/u-forge-core/Cargo.toml --example cli_chat
```

**REPL commands:**

| Command | Effect |
|---|---|
| `/quit` | Exit |
| `/clear` | Reset conversation history |
| `/context` | Toggle display of retrieved knowledge graph nodes |

**Graceful degradation:**

| Scenario | Behaviour |
|---|---|
| No Lemonade Server | Prints setup instructions and exits |
| Lemonade running but no LLM model | Lists available models and exits |
| LLM available but no embedding model | Chat works with FTS5-only search (noted on startup) |
| Full stack (embedding + LLM + reranker) | Hybrid search → rerank → LLM response |

**Config** — add a `[chat]` section to `defaults/demo_config.toml` to override defaults:

```toml
[chat]
system_prompt = "You are a knowledgeable assistant..."
max_history_turns = 10   # turn-pairs retained in context
max_tokens = 1024
temperature = 0.7
alpha = 0.5              # 0.0 = FTS-only, 1.0 = semantic-only
search_limit = 3         # knowledge graph nodes to retrieve per turn
```

### Environment Variables

| Variable | Default | Purpose |
|---|---|---|
| `LEMONADE_URL` | *(auto-detected)* | Override Lemonade Server base URL |
| `UFORGE_SCHEMA_DIR` | `./defaults/schemas` | Directory of `.schema.json` files |
| `UFORGE_DATA_FILE` | `./defaults/data/memory.json` | JSONL data file for import |
| `RUST_LOG` | `error` | Log verbosity (`error`/`warn`/`info`/`debug`/`trace`) |

### Device Configuration

Device weights and enable/disable state are controlled by an optional TOML file at:
- `$XDG_CONFIG_HOME/u-forge-devices.toml` (or `~/.config/u-forge-devices.toml`)
- `./u-forge-devices.toml` (current directory)

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

[models.context_limits]
"embed-gemma-300m-FLM"     = 2048
"user.ggml-org/embeddinggemma-300M-GGUF"   = 2048
"nomic-embed-text-v1-GGUF"  = 2048
"Qwen3-Embedding-8B-GGUF"  = 32768
```

Example `demo_config.toml`:
```toml
# It's separate from u-forge.toml which is used by the application.

[database]
# path = "./demo_data/kg"   # override the default DB location (default: <workspace>/demo_data/kg)
# clear = true              # set to true to wipe the DB before loading (default: false)

[fts]
[[fts.queries]]
query = "mayor"
limit = 3

[semantic]
[[semantic.queries]]
query = "Who is the leader of the Foundation?"
limit = 5

[rerank]
[[rerank.queries]]
query = "Who is the leader of the Foundation?"
semantic_limit = 6

[hybrid]
queries = ["Mayor"]
alpha_sweep_query = "Mayor"
alpha_sweep_values = [0.0, 0.5, 1.0]

[hybrid.config]
alpha = 0.5
fts_limit = 10
semantic_limit = 10
rerank = true
limit = 3

```

When multiple embedding workers are available, the highest-weight idle worker is selected. This configuration only affects the inference queue's device selection strategy — it does not require code changes.

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
│   ├── u-forge-graph-view/ # Graph view model + layout (skeleton — see feature_UI.md)
│   ├── u-forge-ui-traits/  # Framework-agnostic rendering contracts (skeleton — see feature_UI.md)
│   ├── u-forge-ui-gpui/    # GPUI native app (skeleton — see feature_UI.md)
│   ├── u-forge-ui-egui/    # egui fallback app (skeleton — see feature_UI.md)
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
| [.rulesdir/](.rulesdir/) | AI assistant context rules (7 `.mdc` files) |

---

## Sample Data

`defaults/data/memory.json` models Isaac Asimov's **Foundation** universe (~220 nodes,
~312 edges). Used by `cli_demo` for end-to-end testing of the full pipeline: schema
load → data import → FTS5 indexing → search → rerank.

---

## License

MIT License. Your worlds belong to you.
