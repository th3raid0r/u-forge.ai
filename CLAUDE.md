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
  lib.rs                  # KnowledgeGraph facade, ObjectBuilder, re-exports
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

  # ── Still to be reorganized (PR 3) ─────────────────────────────────────────

  inference_queue.rs      # ~1700 lines — target: queue/ directory
  search.rs               # ~1322 lines — target: search/ directory
  schema.rs               # → schema/definition.rs
  schema_manager.rs       # → schema/manager.rs
  schema_ingestion.rs     # → schema/ingestion.rs
  embeddings.rs           # → ai/embeddings.rs
  transcription.rs        # → ai/transcription.rs
  data_ingestion.rs       # → ingest/data.rs
```

---

## Refactor progress

| PR | Status | Description |
|----|--------|-------------|
| PR 1 | ✅ Done | Split `lemonade.rs` (2134 lines) → `lemonade/` (10 files) |
| PR 2 | ✅ Done | Split `storage.rs` (1630 lines) → `graph/` (6 files) |
| PR 3 | ⏳ TODO | Reorganize: `inference_queue.rs` → `queue/`, `search.rs` → `search/`, `schema*.rs` → `schema/`, `embeddings.rs` + `transcription.rs` → `ai/`, `data_ingestion.rs` → `ingest/` |
| PR 4 | ⏳ TODO | Docs cleanup: `env.sh` (remove RocksDB references), update `ARCHITECTURE.md` module map, update `.rulesdir/*.mdc` paths |
| PR 5 | ⏳ TODO | Slim `lib.rs` to facade-only |

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
| **Code Refactor** | 🔄 In progress (PR 3 remaining) |
| Phase 3: axum HTTP Server | ⏳ Next after refactor |
| Phase 4b: LLM token streaming | ⏳ Planned |
| Phase 5: Cargo workspace split | ⏳ Planned |
