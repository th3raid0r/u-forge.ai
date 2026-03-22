# u-forge.ai — Claude Code Context

Read `.rules` first for every task. Rule files in `.rulesdir/` have more detail by topic.

**Canonical test command:** `cargo test -- --test-threads=1`

---

## Project in one paragraph

Local-first TTRPG worldbuilding tool with an AI-powered knowledge graph. Written in Rust. Uses SQLite (FTS5 + sqlite-vec ANN) for storage, Lemonade Server for all AI inference (embedding, STT, TTS, LLM, reranking). The core is a `KnowledgeGraph` facade that is intentionally decoupled from AI — it works without a running server. AI capabilities are opt-in via `InferenceQueue`.

---

## Current module structure

```
src/
  lib.rs                  # KnowledgeGraph facade + re-exports (facade-only, ~270 lines)
  builder.rs              # ObjectBuilder fluent API
  text.rs                 # split_text() — word-boundary chunking utility (pub(crate))
  lib_tests.rs            # Integration tests for KnowledgeGraph + ObjectBuilder
  error.rs                # AppError (future axum HTTP boundary)
  types.rs                # Domain types: ObjectMetadata, Edge, TextChunk, etc.
  test_helpers.rs         # lemonade_url() test helper

  graph/                  # ✅ REFACTORED — was storage.rs
    mod.rs
    storage.rs            # Struct, new(), SQL schema, constants, stats, schema CRUD
    nodes.rs              # upsert_node, get_node, find_by_name, delete_node
    edges.rs              # upsert_edge, get_edges, get_neighbors
    chunks.rs             # upsert_chunk, get_chunks_for_node
    fts.rs                # search_chunks_fts, upsert_chunk_embedding, search_chunks_semantic
    traversal.rs          # query_subgraph (BFS)

  lemonade/               # ✅ REFACTORED — was lemonade.rs + lemonade_client.rs
    mod.rs                # Re-exports + resolve_lemonade_url() + resolve_provider_url()
    client.rs             # LemonadeHttpClient (Bearer auth, error handling)
    registry.rs           # LemonadeModelRegistry, LemonadeModelEntry, ModelRole
    gpu_manager.rs        # GpuResourceManager, GpuWorkload, SttGuard, LlmGuard
    tts.rs                # KokoroVoice, LemonadeTtsProvider
    stt.rs                # LemonadeSttProvider, TranscriptionResult
    chat.rs               # LemonadeChatProvider, ChatMessage, ChatRequest, ChatCompletionResponse
    rerank.rs             # LemonadeRerankProvider, RerankDocument
    system_info.rs        # SystemInfo, LemonadeCapabilities, SystemDeviceInfo
    stack.rs              # LemonadeStack (convenience builder)

  hardware/               # Unchanged — device abstraction layer
    mod.rs                # DeviceCapability, HardwareBackend, DeviceWorker trait
    npu.rs                # NpuDevice
    gpu.rs                # GpuDevice (ROCm)
    cpu.rs                # CpuDevice (Kokoro TTS)

  schema/                 # ✅ REFACTORED — was schema.rs + schema_manager.rs + schema_ingestion.rs
    mod.rs
    definition.rs         # SchemaDefinition, ObjectTypeSchema, PropertySchema, ValidationResult, etc.
    manager.rs            # SchemaManager, SchemaStats
    ingestion.rs          # SchemaIngestion (JSON → SchemaDefinition)

  ai/                     # ✅ REFACTORED — was embeddings.rs + transcription.rs
    mod.rs
    embeddings.rs         # EmbeddingProvider trait, LemonadeProvider, EmbeddingManager
    transcription.rs      # TranscriptionProvider trait, LemonadeTranscriptionProvider, TranscriptionManager

  ingest/                 # ✅ REFACTORED — was data_ingestion.rs
    mod.rs
    data.rs               # DataIngestion, IngestionStats, JsonEntry

  queue/                  # ✅ REFACTORED — was inference_queue.rs (1692 lines)
    mod.rs                # Re-exports + module docs
    jobs.rs               # EmbedJob, TranscribeJob, SynthesizeJob, GenerateJob, RerankJob, WorkQueue<T>
    dispatch.rs           # InferenceQueue struct + public API + QueueStats + tests
    builder.rs            # InferenceQueueBuilder struct + impl + Default
    workers.rs            # run_*_worker functions + retry constants

  search/                 # ✅ REFACTORED — was search.rs (1322 lines)
    mod.rs                # HybridSearchConfig, NodeSearchResult, SearchSources, search_hybrid, tests
    sanitize.rs           # fts5_sanitize() + unit tests
```

---

## Refactor progress

| PR | Status | Description |
|----|--------|-------------|
| PR 1 | ✅ Done | Split `lemonade.rs` (2134 lines) → `lemonade/` (10 files) |
| PR 2 | ✅ Done | Split `storage.rs` (1630 lines) → `graph/` (6 files) |
| PR 3 | ✅ Done | Reorganize: `schema*.rs` → `schema/`, `embeddings.rs` + `transcription.rs` → `ai/`, `data_ingestion.rs` → `ingest/`, `inference_queue.rs` → `queue/`, `search.rs` → `search/` |
| PR 4 | ✅ Done | Docs cleanup: `env.sh` (removed RocksDB references), updated `ARCHITECTURE.md` + `README.md` module maps, updated `.rulesdir/*.mdc` paths |
| PR 5 | ✅ Done | Slim `lib.rs` to facade-only (~270 lines): extracted `ObjectBuilder` → `builder.rs`, `split_text` → `text.rs`, tests → `lib_tests.rs` |

---

## Key anti-patterns (summary — see `.rules` for full list)

1. **Env vars are overrides, not requirements.** `cargo run --example cli_demo` must work with zero env vars set. Always try `http://localhost:8000/api/v1` first.
2. **Fetch live state before assuming capabilities.** Use `SystemInfo::fetch()` and `LemonadeModelRegistry::fetch()`.
3. **Docs are indexes, not mirrors.** Don't duplicate what source comments already say.

---

## Phase status

| Phase | Status |
|-------|--------|
| Phase 1: Lemonade Provider | ✅ Complete |
| Phase 2: SQLite Migration | ✅ Complete |
| Hardware re-arch (NPU/GPU/CPU) | ✅ Complete |
| Phase 4: Hybrid Search | ✅ Complete |
| **Code Refactor** | ✅ Complete |
| Phase 3: axum HTTP Server | ⏳ Next after refactor |
| Phase 4b: LLM token streaming | ⏳ Planned |
| Phase 5: Cargo workspace split | ⏳ Planned |
