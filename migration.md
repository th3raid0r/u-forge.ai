# u-forge.ai — Migration State & Roadmap

> Standardised on [Lemonade Server](https://github.com/lemonade-sdk/lemonade) for all AI
> and SQLite for unified storage. Replaces the former self-managed llama.cpp fleet.

---

## What Is Complete

Everything below is **done and tested**. Refer to the source files for implementation
details — they are thoroughly commented.

### Storage & Search
- SQLite via `rusqlite` (bundled) replaces RocksDB — zero gcc-13 / system lib requirement
- FTS5 full-text search live (`search_chunks_fts`); `src/vector_search.rs` deleted
- `sqlite-vec` ANN vector search via `vec0` virtual table — `chunks_vec` (768-dim, cosine distance)
- `KnowledgeGraphStorage::upsert_chunk_embedding` / `search_chunks_semantic` — rowid-mapped vec index, DELETE+INSERT upsert (vec0 constraint)
- `KnowledgeGraph::search_chunks_semantic(query_embedding, limit)` facade — returns `(ChunkId, ObjectId, String, f32)` tuples ordered by ascending cosine distance
- `KnowledgeGraph::add_text_chunk` now splits content at word boundaries into ≤`MAX_CHUNK_TOKENS` (350) pieces before storage — guards against the llamacpp 512-token batch limit
- `EMBEDDING_DIMENSIONS = 768`, `MAX_CHUNK_TOKENS = 350`, `ENABLE_HIGH_QUALITY_EMBEDDING = false` constants in `storage.rs`
- `chunks_vec_ad` trigger keeps the vec index clean on cascade delete
- `ON DELETE CASCADE`, indexed FKs, `SELECT COUNT(*)` stats — all O(1) or DB-indexed
- JSONL two-pass import with deduplication and cross-session edge resolution

### Embedding & Transcription
- `LemonadeProvider` (HTTP) replaces FastEmbed/ORT/ONNX — fully async, no `Mutex` blocking
- `EmbeddingManager::try_new_auto` probes `localhost:8000`, then `LEMONADE_URL`
- `TranscriptionProvider` trait + `LemonadeTranscriptionProvider` in `src/transcription.rs`
- `TranscriptionManager::try_new_auto` is synchronous — no probe at construction

### Hardware Abstraction
- `src/hardware/` — `DeviceCapability`, `HardwareBackend`, `DeviceWorker` trait
- `NpuDevice` — embedding + STT + LLM via FLM on AMD NPU (no GPU contention)
- `GpuDevice` — STT + LLM on AMD GPU (ROCm), serialised via `GpuResourceManager`
- `CpuDevice` — TTS via Kokoro on host CPU

### Inference Queue
- `InferenceQueue` + `InferenceQueueBuilder` in `src/inference_queue.rs`
- Five capability channels: `embed`, `transcribe`, `synthesize`, `generate`, `rerank`
- MPMC primitive: `parking_lot::Mutex<VecDeque<T>> + tokio::sync::Notify`
- `generate_queue` fully wired — NPU FLM LLMs and GPU llamacpp models compete
- `InferenceQueueBuilder::with_embedding_provider(Arc<dyn EmbeddingProvider>, name)` — register standalone llamacpp/ROCm/CPU embedding workers without a full device struct
- Per-job retry logic in `run_embed_worker` — up to 3 attempts with 100 ms/200 ms exponential backoff before forwarding error to caller

### Extended Lemonade Integration (`src/lemonade.rs`)
- `LemonadeModelRegistry` — model discovery + `ModelRole` classification (FLM checked first)
- `GpuResourceManager` — RAII `SttGuard` / `LlmGuard` for GPU serialisation
- `LemonadeTtsProvider` (Kokoro, CPU), `LemonadeSttProvider` (Whisper, GPU)
- `LemonadeChatProvider` — GPU llamacpp + NPU FLM variants; `gpu: Option<Arc<GpuResourceManager>>`
- `LemonadeStack` — one-call convenience builder
- `LemonadeRerankProvider` — `POST /api/v1/reranking`, `bge-reranker-v2-m3-GGUF`; sorted by descending score
- `SystemInfo::fetch()` + `LemonadeCapabilities` — hardware capability derivation from `/system-info`
- `LemonadeModelRegistry::all_cpu_embedding_models()` — returns all `CpuEmbedding` models in preferred order (`nomic-embed-text-v2-moe-GGUF` first, then v1, then others), excluding `Qwen3-Embedding-8B-GGUF` unless `ENABLE_HIGH_QUALITY_EMBEDDING = true`

### CLI Demo (`examples/cli_demo.rs`)
- Hardware capability detection, device listing, model registry display
- FTS5 search over Foundation universe dataset (~220 nodes, ~312 edges)
- Rerank demo: FTS5 candidates → `LemonadeRerankProvider` → re-ordered results
- Semantic search demo: all chunks embedded via 3-worker `InferenceQueue` (NPU + 2× llamacpp `nomic-embed-text-v2-moe-GGUF`); ~2264 chunks at ~240 chunks/s
- `resolve_lemonade_url()` auto-discovery — demo works with no env vars set; localhost probed first, `LEMONADE_URL` is an override only

### Removed (do not reference)
`fastembed`, `ort`, `ort-sys`, `hnsw_rs`, `fst`, `memmap2`, `rocksdb`, `petgraph`,
`bincode`, `rayon`, `src/vector_search.rs`, `FastEmbedProvider`, `embedding_cache_dir`
param, `get_embedding_provider()` on `KnowledgeGraph`.

---

## Running the Project

```bash
cargo build                        # ~30 s first time (bundled SQLite)
cargo test -- --test-threads=1     # all tests; always single-threaded
cargo run --example cli_demo       # Foundation universe demo — no env vars required
```

**Always pass `--test-threads=1`.** Integration tests share GPU/NPU hardware on a
live Lemonade Server; parallel execution causes intermittent contention failures.
`dev.sh` sets this flag unconditionally.

`LEMONADE_URL` is **not required** — `cli_demo` auto-discovers Lemonade Server at
`localhost:8000` via `resolve_lemonade_url()`. Set `LEMONADE_URL` only if your
server runs on a non-standard host or port.

```bash
# With Lemonade Server (non-default URL example)
lemonade-server pull nomic-embed-text-v2-moe-GGUF
lemonade-server pull bge-reranker-v2-m3-GGUF
lemonade-server serve
cargo run --example cli_demo

# Override URL only when needed
export LEMONADE_URL="http://192.168.1.10:8000/api/v1"
cargo run --example cli_demo
```

---

## Remaining Work

### Phase 3: axum HTTP/WebSocket Server

**Goal:** Wrap `KnowledgeGraph` + `InferenceQueue` behind HTTP and WebSocket endpoints
so a web UI can connect.

**New dependencies:**
```toml
axum = "0.8"
tower-http = { version = "0.6", features = ["cors", "fs"] }
```

**`AppState`:**
```rust
pub struct AppState {
    pub graph:  Arc<KnowledgeGraph>,
    pub queue:  Arc<InferenceQueue>,
    pub config: Arc<AppConfig>,
}
```

`KnowledgeGraph` is already `Send + Sync` and `Arc`-wrapped. No changes needed to
the core library — Phase 3 is a new binary (or feature-gated crate entry point) only.

**REST routes:**
```
GET    /api/objects              list objects (paginated)
GET    /api/objects/:id          get single object
POST   /api/objects              create (validated)
PUT    /api/objects/:id          update
DELETE /api/objects/:id          delete
GET    /api/objects/:id/edges    edges for object
GET    /api/objects/:id/subgraph BFS subgraph
POST   /api/search               hybrid search
GET    /api/schemas              list schemas
POST   /api/ingest               JSONL import
GET    /api/stats                graph statistics
GET    /api/health               health (includes Lemonade status)
```

**WebSocket `/ws`:** push notifications for object mutations, streaming search
results, import progress.

---

### Phase 4: Integrated Search Pipeline

**Goal:** Wire the existing `LemonadeRerankProvider` and `InferenceQueue::rerank()`
into `KnowledgeGraph` search methods. The provider is already implemented and
demonstrated in `cli_demo`.

**What remains:**

1. Add `search_hybrid(query, limit)` to `KnowledgeGraph` that:
   - Embeds `query` via `InferenceQueue::embed()`
   - Runs `search_chunks_semantic`
   - Runs `search_chunks_fts` in parallel
   - Merges results with configurable alpha weighting
   - Reranks merged top-K via `InferenceQueue::rerank()`
   - Returns final ranked `Vec<SearchResult>`
2. `KnowledgeGraph` needs optional `Arc<InferenceQueue>` — pass via a builder or
   separate `configure_inference(queue)` method (keeps `KnowledgeGraph::new` simple)
3. Add streaming LLM responses: `InferenceQueue::generate_stream()` returning a
   `tokio::sync::mpsc::Receiver<String>` of token chunks

---

### Phase 5: Cargo Workspace Split

**Goal:** Separate concerns into independent crates for maintainability and
future packaging.

```
u-forge.ai/
├── crates/
│   ├── core/           # Domain types, KnowledgeGraph, storage, schemas
│   ├── lemonade/       # Lemonade Server client (embeddings, reranking, chat, TTS, STT)
│   ├── hardware/       # Device abstractions + InferenceQueue
│   └── server/         # axum HTTP/WebSocket server (Phase 3)
├── web-ui/             # Frontend (React or Svelte)
└── Cargo.toml          # Workspace root
```

Split only after Phase 3 is stable. The current single-crate layout is intentional
for this stage of development — premature splitting adds friction without benefit.