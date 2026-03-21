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

### Extended Lemonade Integration (`src/lemonade.rs`)
- `LemonadeModelRegistry` — model discovery + `ModelRole` classification (FLM checked first)
- `GpuResourceManager` — RAII `SttGuard` / `LlmGuard` for GPU serialisation
- `LemonadeTtsProvider` (Kokoro, CPU), `LemonadeSttProvider` (Whisper, GPU)
- `LemonadeChatProvider` — GPU llamacpp + NPU FLM variants; `gpu: Option<Arc<GpuResourceManager>>`
- `LemonadeStack` — one-call convenience builder
- `LemonadeRerankProvider` — `POST /api/v1/reranking`, `bge-reranker-v2-m3-GGUF`; sorted by descending score
- `SystemInfo::fetch()` + `LemonadeCapabilities` — hardware capability derivation from `/system-info`

### CLI Demo (`examples/cli_demo.rs`)
- Hardware capability detection, device listing, model registry display
- FTS5 search over Foundation universe dataset (~220 nodes, ~312 edges)
- Rerank demo: FTS5 candidates → `LemonadeRerankProvider` → re-ordered results
- Degrades gracefully when no Lemonade Server is present

### Removed (do not reference)
`fastembed`, `ort`, `ort-sys`, `hnsw_rs`, `fst`, `memmap2`, `rocksdb`, `petgraph`,
`bincode`, `rayon`, `src/vector_search.rs`, `FastEmbedProvider`, `embedding_cache_dir`
param, `get_embedding_provider()` on `KnowledgeGraph`.

---

## Running the Project

```bash
cargo build                        # ~30 s first time (bundled SQLite)
cargo test -- --test-threads=1     # all tests; always single-threaded
cargo run --example cli_demo       # Foundation universe demo
```

**Always pass `--test-threads=1`.** Integration tests share GPU/NPU hardware on a
live Lemonade Server; parallel execution causes intermittent contention failures.
`dev.sh` sets this flag unconditionally.

```bash
# With Lemonade Server
export LEMONADE_URL="http://localhost:8000/api/v1"
lemonade-server pull embed-gemma-300m-FLM
lemonade-server pull bge-reranker-v2-m3-GGUF
lemonade-server serve
cargo run --example cli_demo
```

---

## Remaining Work

### sqlite-vec (Phase 2 remainder) — highest priority storage item

Add the `vec0` virtual table to `storage.rs`:

1. Add `rusqlite` vtab for sqlite-vec to `Cargo.toml`
2. `CREATE VIRTUAL TABLE chunks_vec USING vec0(embedding FLOAT[N])` in schema init
3. Add `embedding` column (nullable `BLOB`) to `chunks` table
4. Implement `KnowledgeGraphStorage::upsert_chunk_embedding(chunk_id, vec: Vec<f32>)`
5. Implement `KnowledgeGraphStorage::search_chunks_semantic(query_vec, limit) -> Vec<(ChunkId, ObjectId, f32)>`
6. Expose `KnowledgeGraph::search_chunks_semantic` — mirrors `search_chunks_fts` signature
7. Wire `InferenceQueue::embed()` to populate embeddings after chunk insert
8. Dimension constant must match the active embedding model (e.g. 256 for `embed-gemma-300m-FLM`)

---

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
   - Runs `search_chunks_semantic` (needs sqlite-vec — depends on Phase 2 remainder)
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