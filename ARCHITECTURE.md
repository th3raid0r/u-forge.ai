# u-forge.ai — Architecture Reference

## Workspace Layout

The project is a Cargo workspace. All source lives under `crates/`:

| Crate | Kind | Status | Purpose |
|-------|------|--------|---------|
| `u-forge-core` | lib | Complete | All current source — storage, AI, hardware, queue, search, schema, ingest |
| `u-forge-graph-view` | lib | Skeleton | Graph view model + layout (see `feature_UI.md`) |
| `u-forge-ui-traits` | lib | Skeleton | Framework-agnostic rendering contracts (see `feature_UI.md`) |
| `u-forge-ui-gpui` | bin | Skeleton | GPUI native app (see `feature_UI.md`) |
| `u-forge-ui-egui` | bin | Skeleton | egui fallback app (see `feature_UI.md`) |
| `u-forge-ts-runtime` | lib | Skeleton | Embedded deno_core TypeScript sandbox (see `feature_TS-Agent-Sandbox.md`) |

`defaults/` (schemas + sample data) stays at the workspace root. Both example entry points and `examples/common/mod.rs` resolve it via `env!("CARGO_MANIFEST_DIR") + "/../../defaults/"`.

## Module Map (u-forge-core)

All paths below are relative to `crates/u-forge-core/`.

| File | Role | Key Types |
|---|---|---|
| `src/lib.rs` | KnowledgeGraph facade + re-exports | `KnowledgeGraph` |
| `src/builder.rs` | Fluent object construction | `ObjectBuilder` |
| `src/text.rs` | Word-boundary text splitting | `split_text` |
| `src/types.rs` | All domain types | `ObjectMetadata`, `Edge`, `EdgeType`, `TextChunk`, `QueryResult` |
| `src/graph/` | SQLite persistence (6 files) | `KnowledgeGraphStorage`, `GraphStats` |
| `src/graph/edges.rs` | Edge CRUD + `get_all_edges()` | `KnowledgeGraphStorage` (impl) |
| `src/graph/nodes.rs` | Node CRUD + `get_nodes_paginated()` | `KnowledgeGraphStorage` (impl) |
| `src/search/mod.rs` | Hybrid search pipeline (FTS5 + ANN + rerank) | `search_hybrid`, `HybridSearchConfig`, `NodeSearchResult`, `SearchSources` |
| `src/search/sanitize.rs` | FTS5 query sanitisation | `fts5_sanitize` |
| `src/ai/embeddings.rs` | Embedding trait + re-exports | `EmbeddingProvider` (trait), `EmbeddingModelInfo`, `EmbeddingProviderType`; re-exports `LemonadeProvider` |
| `src/ai/transcription.rs` | Transcription trait + MIME helper + re-exports | `TranscriptionProvider` (trait), `mime_for_filename`; re-exports `LemonadeTranscriptionProvider` |
| `src/config.rs` | Application configuration (TOML loading, weights, model prefs, chat settings) | `AppConfig`, `EmbeddingDeviceConfig`, `ModelConfig`, `ModelLoadParams`, `ChatConfig`, `ChatDeviceConfig`, `ChatDevice` |
| `src/queue/dispatch.rs` | Unified MPMC capability-based dispatch queue | `InferenceQueue`, `QueueStats` |
| `src/queue/builder.rs` | Queue builder + device wiring | `InferenceQueueBuilder` |
| `src/queue/jobs.rs` | Internal job types + WorkQueue primitive | `EmbedJob`, `WorkQueue<T>` |
| `src/queue/weighted.rs` | Weighted embedding job dispatcher (high-weight idle first) | `WeightedEmbedDispatcher`, `WeightedWorkerSlot` |
| `src/queue/workers.rs` | Background worker loops | `run_embed_worker`, `run_transcribe_worker`, etc. |
| `src/lemonade/catalog.rs` | Lemonade Server catalog discovery | `LemonadeServerCatalog`, `CatalogModel`, `InstalledBackend`, `LoadedModel` |
| `src/lemonade/selector.rs` | Catalog-driven model selection; per-device-slot dedup | `ModelSelector`, `SelectedModel`, `QualityTier` |
| `src/lemonade/provider_factory.rs` | Provider construction from `SelectedModel` | `ProviderFactory`, `Capability`, `BuiltProvider`, `ProviderSlot` |
| `src/lemonade/duplicate_guard.rs` | Detects duplicate llamacpp model names across backends | `DuplicateGuard` |
| `src/lemonade/embedding.rs` | Embedding provider impl | `LemonadeProvider` |
| `src/lemonade/transcription.rs` | Transcription provider impl (no GPU lock) | `LemonadeTranscriptionProvider` |
| `src/lemonade/gpu_manager.rs` | GPU resource serialisation | `GpuResourceManager`, `GpuWorkload`, `LlmGuard`, `SttGuard` |
| `src/lemonade/chat.rs` | LLM chat provider | `LemonadeChatProvider`, `ChatRequest`, `ChatMessage`, `ChatCompletionResponse` |
| `src/lemonade/stt.rs` | GPU-managed STT provider | `LemonadeSttProvider`, `TranscriptionResult` |
| `src/lemonade/tts.rs` | TTS provider | `LemonadeTtsProvider`, `KokoroVoice` |
| `src/lemonade/rerank.rs` | Cross-encoder reranking | `LemonadeRerankProvider`, `RerankDocument` |
| `src/lemonade/system_info.rs` | System hardware info | `SystemInfo`, `SystemDeviceInfo`, `RecipeBackendInfo` |
| `src/lemonade/load.rs` | Model load/unload via Lemonade `POST /api/v1/load` | `load_model()`, `ModelLoadOptions` |
| `src/lemonade/health.rs` | Server health endpoint | `LemonadeHealth`, `LoadedModelEntry` |
| `src/schema/definition.rs` | Schema definition types | `SchemaDefinition`, `ObjectTypeSchema`, `PropertySchema`, `EdgeTypeSchema` |
| `src/schema/manager.rs` | Schema load/validate/cache | `SchemaManager`, `SchemaStats` |
| `src/schema/ingestion.rs` | JSON schema file → internal | `SchemaIngestion` |
| `src/ingest/data.rs` | JSONL import pipeline | `DataIngestion`, `JsonEntry`, `IngestionStats` |
| `src/ingest/pipeline.rs` | Schema + data loading + FTS5 indexing | `setup_and_index()`, `SetupResult` |
| `src/ingest/embedding.rs` | Batch chunk embedding (standard & HQ) | `embed_all_chunks()`, `build_hq_embed_queue()`, `EmbeddingTarget`, `EmbeddingResult` |
| `examples/common/mod.rs` | Demo-specific helpers (config, CLI args) | `DatabaseConfig`, `DemoArgs`, `resolve_demo_args()`, `load_toml_config()` |
| `examples/cli_demo.rs` | Demo: hardware caps, FTS5, semantic, rerank, hybrid search (includes `common` via `#[path]`) | — |
| `examples/cli_chat.rs` | Interactive RAG chat REPL (includes `common` via `#[path]`) | — |

---

## Core Data Model

### KnowledgeGraph (lib.rs)

The public facade. Composes two subsystems via `Arc`:

```rust
pub struct KnowledgeGraph {
    storage: Arc<KnowledgeGraphStorage>,
    schema_manager: Arc<SchemaManager>,
}
```

`embedding_manager` has been removed from the struct. Embedding operations are
performed externally through an `InferenceQueue` built separately; they are not
part of the `KnowledgeGraph` lifecycle.

**Constructor:** `KnowledgeGraph::new(db_path: &Path)` — one argument only. The
former `embedding_cache_dir` parameter no longer exists.

**`get_embedding_provider()` has been removed.**

API surface: CRUD for objects (`add_object`, `get_object`, `update_object`,
`delete_node`), edges (`connect_objects`, `get_edges`), text chunks
(`add_chunk_to_object`, `get_chunks_for_node`), `find_by_name`, `find_by_name_only`,
`query_subgraph` (BFS), `search_chunks_fts`, schema validation (async methods),
`get_stats`.

**Bulk access methods:**
- `get_all_edges() -> Result<Vec<Edge>>` — single `SELECT * FROM edges` query; prefer over repeated `get_relationships()` calls when building a full graph snapshot.
- `get_nodes_paginated(offset: usize, limit: usize) -> Result<Vec<ObjectMetadata>>` — `ORDER BY name LIMIT ? OFFSET ?`; for incremental full-graph snapshots.

**Other notable methods:**
- `search_chunks_fts(query: &str, limit: usize) -> Result<Vec<(ChunkId, ObjectId, String)>>` — wraps SQLite FTS5 full-text search over the `chunks_fts` virtual table.
- `find_by_name_only(name: &str) -> Result<Option<ObjectId>>` — name lookup independent of object type; used by `DataIngestion` to resolve cross-session edge references.
- `get_stats() -> Result<GraphStats>` — returns `GraphStats { node_count, edge_count, chunk_count, total_tokens, embedded_count, embedded_hq_count }`. Queries are O(1) via indexed counts. `embedded_count` tracks chunks with 768-dim vectors, `embedded_hq_count` tracks chunks with 4096-dim vectors. Used to detect whether embedding work needs to be done (e.g., in `cli_demo`).
- `clear_all() -> Result<()>` — delete all nodes (cascades to edges/chunks), schemas, and vector indexes. Leaves the database schema intact. Useful for resetting between demo runs.
- `search_chunks_ann(query_embedding: &[f32], limit: usize) -> Result<Vec<(ChunkId, ObjectId, String, f32)>>` — ANN search on `chunks_vec` (or `chunks_vec_hq` if calling `search_chunks_ann_hq`). Returns results ordered by ascending cosine distance. Only chunks indexed via `upsert_chunk_embedding()` are candidates.

`ObjectBuilder` provides a fluent API unchanged from before:
`ObjectBuilder::character("Name")`, `.with_description()`, `.with_property()`,
`.build()`, `.add_to_graph(&kg)`.

---

## Storage (storage.rs — SQLite)

RocksDB and its five column families have been replaced with a single SQLite
database file (`rusqlite` with the `bundled` feature — no system SQLite required).

### Tables

**`nodes`**
```
id          TEXT PRIMARY KEY,
object_type TEXT NOT NULL,
schema_name TEXT,
name        TEXT NOT NULL,
description TEXT,
tags        TEXT NOT NULL DEFAULT '[]',   -- JSON array
properties  TEXT NOT NULL DEFAULT '{}',   -- JSON object
created_at  TEXT NOT NULL,
updated_at  TEXT NOT NULL
```

**`edges`**
```
source_id   TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
target_id   TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
edge_type   TEXT NOT NULL,
weight      REAL NOT NULL DEFAULT 1.0,
metadata    TEXT NOT NULL DEFAULT '{}',   -- JSON object
created_at  TEXT NOT NULL,
UNIQUE(source_id, target_id, edge_type)
```

**`chunks`**
```
id          TEXT PRIMARY KEY,
object_id   TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
chunk_type  TEXT NOT NULL,
content     TEXT NOT NULL,
token_count INTEGER NOT NULL DEFAULT 0,
created_at  TEXT NOT NULL
```

**`schemas`**
```
name        TEXT PRIMARY KEY,
definition  TEXT NOT NULL    -- JSON
```

**`chunks_fts`** — FTS5 virtual table mirroring `chunks(content)`.
Auto-populated and auto-updated via `AFTER INSERT`, `AFTER UPDATE`, and
`AFTER DELETE` triggers on the `chunks` table. Queried via standard FTS5
`MATCH` syntax.

**`chunks_vec`** — sqlite-vec `vec0` virtual table for standard ANN similarity search (768-dim).
```
rowid   INTEGER  (maps to chunks.rowid — same identity as chunks_fts)
embedding float[768] distance_metric=cosine
```
Populated explicitly via `upsert_chunk_embedding()`; not every chunk has an entry
immediately after creation. Kept clean by a `chunks_vec_ad` trigger that fires
`AFTER DELETE ON chunks`.

**`chunks_vec_hq`** — sqlite-vec `vec0` virtual table for high-quality ANN search (4096-dim).
```
rowid   INTEGER  (maps to chunks.rowid)
embedding float[4096] distance_metric=cosine
```
Stored alongside (not replacing) `chunks_vec` to allow both standard and high-quality
embeddings to coexist. Populated only when a high-quality embedding model (e.g.
Qwen3-Embedding-8B-GGUF) is available in Lemonade Server and enabled in config
(`embedding.high_quality_embedding: true`). Kept clean by a `chunks_vec_hq_ad` trigger
that fires `AFTER DELETE ON chunks`.

### Indexes

| Index | Table | Columns | Purpose |
|---|---|---|---|
| `idx_nodes_type` | nodes | object_type | Filter by type |
| `idx_nodes_name` | nodes | object_type, name | find_by_name (type-scoped) |
| `idx_nodes_name_only` | nodes | name | find_by_name_only (cross-type) |
| `idx_edges_source` | edges | source_id | Outgoing edge lookup |
| `idx_edges_target` | edges | target_id | Incoming edge lookup, cascade perf |
| `idx_chunks_object` | chunks | object_id | get_chunks_for_node |
| `chunks_vec_ad` trigger | chunks | rowid | Cascade-deletes 768-dim vec entries when a chunk is deleted |
| `chunks_vec_hq_ad` trigger | chunks | rowid | Cascade-deletes 4096-dim HQ vec entries when a chunk is deleted |

### Design Notes

- Foreign key enforcement is enabled at connection time (`PRAGMA foreign_keys = ON`).
- `ON DELETE CASCADE` on both `edges` and `chunks` means node deletion is a single
  `DELETE FROM nodes WHERE id = ?` — the database removes all dependent rows
  automatically via indexed FK scans. O(log N) instead of O(N).
- Edge uniqueness (`UNIQUE(source_id, target_id, edge_type)`) replaces the old
  manual adjacency-list deduplication.
- `tags` and `properties` are stored as JSON text and deserialized via `serde_json`
  at the Rust layer. No JSON1 extension queries are needed for current operations.
- `GET COUNT(*)` and `SUM(token_count)` queries replace the old full-scan
  `get_stats` implementation.
- The old `AdjacencyList` struct (with separate `outgoing`/`incoming` `Vec<Edge>`
  per node) is gone. Edge direction is now implicit in `source_id`/`target_id`.
- `bincode` has been removed; all serialization is JSON via `serde_json`.
- **Vector indexes live** — Both `chunks_vec` (768-dim) and `chunks_vec_hq` (4096-dim) are sqlite-vec `vec0` virtual tables supporting cosine distance. `upsert_chunk_embedding()` and `upsert_chunk_embedding_hq()` populate them respectively. `search_chunks_ann()` returns results ordered by ascending cosine distance. High-quality (HQ) embeddings are optional and only built when Qwen3-Embedding-8B-GGUF or similar is available and enabled.
- **Chunk size enforcement** — `add_text_chunk` splits content at word boundaries into ≤350-token pieces (`MAX_CHUNK_TOKENS`) before storage. Guards against the llamacpp 512-token batch limit. Uses the same `len.div_ceil(3)` heuristic as `estimate_token_count` (≈ 3 chars/token — conservative for dense prose, producing ≈ 1 500 characters per chunk).

---

## Domain Types (types.rs)

Unchanged from the previous architecture. Key types:

- `ObjectMetadata`: `object_type: String` + `serde_json::Value` properties blob.
  Dynamic schema, no compile-time type enforcement.
  `flatten_for_embedding(edge_lines: &[String]) -> String` accepts incident edge strings (formatted as `"{from_name} {edge_type} {to_name}"`) so relationship context is included in the embedding input. Pass `&[]` when no edges are needed.
- `EdgeType`: transparent newtype `struct EdgeType(pub String)`. Construct with `EdgeType::new(s)`; read back with `.as_str()`. No enum variants.
- `TextChunk`: content + estimated token count (`text.len().div_ceil(3)`). Types:
  `Description`, `SessionNote`, `AiGenerated`, `UserNote`, `Imported`.
- `QueryResult`: aggregates objects + edges + chunks with a token budget system.
- `ChunkId`, `ObjectId`: newtype structs wrapping `Uuid` (`#[serde(transparent)]`). Compile-time type safety — the compiler rejects passing a `ChunkId` where an `ObjectId` is expected. Construct with `::new_v4()`; parse from string with `::parse_str(s)`.

---

## Hardware & Inference Architecture

### Overview

AI inference uses a catalog-driven provider selection flow. `LemonadeServerCatalog`
discovers available models and hardware; `ModelSelector` picks the best downloaded
model for each capability and device slot; `ProviderFactory` constructs the live
providers; `InferenceQueueBuilder` wires them into the `InferenceQueue`.

```
LemonadeServerCatalog::discover(url)
  ├─ GET /api/v1/models   (all models + download status)
  ├─ GET /system-info     (installed recipe backends)
  └─ GET /api/v1/health   (currently loaded models)

ModelSelector::new(catalog, config)
  ├─ select_embedding_models()  → ≤1 per (device_slot, quality_tier)
  ├─ select_llm_models()        → ≤1 per device_slot
  ├─ select_stt_models()        → ≤1 per device_slot
  ├─ select_tts()               → best downloaded TTS model
  ├─ select_reranker()          → best downloaded reranker
  └─ model_by_id(id, tier)     → exact lookup (ignores preference lists)

ProviderFactory::build(sel, capability, url, queue_depth, gpu_mgr)
  → BuiltProvider { slot: ProviderSlot, capability: Capability, weight: u32 }

InferenceQueueBuilder::new()
  .with_providers(built_providers)
  .build()  → InferenceQueue (spawns background Tokio tasks)

InferenceQueue
  ├─ WeightedEmbedDispatcher  (EWMA latency + work-stealing)
  ├─ transcribe_queue         (first-free worker wins)
  ├─ generate_queue           (serialised per GPU resource manager)
  ├─ synthesize_queue
  └─ rerank_queue
```

**Device slots** — deduplication key used by `ModelSelector`:
- `flm` recipe → `"npu"`
- `llamacpp` + `rocm`/`vulkan`/`metal` → `"gpu"`
- `llamacpp` + cpu → `"cpu"`
- other recipes (e.g. `whispercpp`, `kokoro`) → recipe name

**Embedding dispatch:** `WeightedEmbedDispatcher` uses EWMA latency to route to the
fastest idle worker; static weights (NPU=100, GPU=50, CPU=10) break ties. Work
stealing drains backlogs when one worker is faster than another.

**GPU sharing:** `LemonadeSttProvider` and `LemonadeChatProvider` share a
`GpuResourceManager`. STT fails immediately if LLM is active; LLM suspends until
STT finishes. `ProviderFactory` attaches the GPU manager automatically for
`llamacpp:rocm/vulkan/metal` and `whispercpp` recipes.

### InferenceQueue (`src/queue/`)

MPMC work queue built from `parking_lot::Mutex<VecDeque<T>> + tokio::sync::Notify`
per capability type — no additional crate dependencies.

#### Weighted Embedding Dispatch with Throughput Awareness and Work Stealing

Embedding jobs are routed via `WeightedEmbedDispatcher` (`src/queue/weighted.rs`), which adapts to actual device throughput:

**Dispatch algorithm:**
- Each worker tracks an EWMA (exponential weighted moving average, α=0.5) of job duration in microseconds
- Cost of routing a job to a worker: `(pending_jobs + 1) × ewma_duration_us`
- The dispatcher picks the worker with the **lowest predicted completion time**
- Static `weight` (NPU=100, GPU=50, CPU=10) is used only as a tiebreaker when costs are equal
- After the first job completes on each worker, the EWMA converges quickly to actual latency, causing the dispatcher to route new jobs to the faster device

**Work stealing (solves NPU backlog drain problem):**
- When a worker finishes a job and its own queue is empty, it calls `steal_from_busiest()` to grab one job from the most-loaded other worker's queue
- A `global_notify` broadcast on every `submit()` wakes all idle workers, so a GPU worker sleeping while NPU has a backlog immediately wakes and steals
- The steal loop keeps the fast worker busy draining the slow worker's backlog without any additional synchronisation
- Worst-case latency when GPU is faster by 10×: GPU drains the initial NPU backlog in ~1/10 the time NPU alone would take

**Device weights** (configurable via `AppConfig`):
- NPU embedding: 100 (highest static priority for new jobs when costs tie)
- GPU embedding: 50 (medium static priority)
- CPU embedding: 10 (lowest static priority)

#### Configuration

Application-level configuration is controlled by `AppConfig` (`src/config.rs`):

```rust
pub struct AppConfig {
    pub embedding: EmbeddingDeviceConfig,  // device enable/disable, weights, HQ flag
    pub models: ModelConfig,               // load params, preference lists
    pub chat: ChatConfig,                  // preferred device, per-device model overrides
}

pub struct EmbeddingDeviceConfig {
    pub npu_enabled: bool,              // default: true
    pub gpu_enabled: bool,              // default: true
    pub cpu_enabled: bool,              // default: true
    pub high_quality_embedding: bool,   // default: false
    pub npu_weight: u32,                // default: 100
    pub gpu_weight: u32,                // default: 50
    pub cpu_weight: u32,                // default: 10
}

pub struct ModelConfig {
    pub load_params: HashMap<String, ModelLoadParams>,  // ctx_size, batch_size, ubatch_size
    pub high_quality_embedding_models: Vec<String>,     // e.g. ["Qwen3-Embedding-8B-GGUF"]
    pub llamacpp_backend_preference: Vec<String>,       // default: ["rocm", "vulkan", "cpu"]
    pub embedding_model_preferences: Vec<String>,       // ordered preference list
    pub reranker_model_preferences: Vec<String>,
    pub stt_model_preferences: Vec<String>,
    pub llm_model_preferences: Vec<String>,
    pub tts_model_preferences: Vec<String>,
}

pub struct ChatConfig {
    pub preferred_device: ChatDevice,   // Auto | Gpu | Npu | Cpu
    pub gpu: ChatDeviceConfig,          // { model, max_tokens, temperature }
    pub npu: ChatDeviceConfig,
    pub cpu: ChatDeviceConfig,
    pub system_prompt: String,
    pub max_history_turns: usize,       // default: 10
    pub alpha: f32,                     // 0.0 = FTS5-only, 1.0 = semantic-only
    pub search_limit: usize,            // default: 3
    pub hq_semantic_boost: f32,         // RRF weight multiplier for 4096-dim path
}
```

`AppConfig::load_default()` loads from:
1. `./u-forge.toml` (current directory)
2. `$XDG_CONFIG_HOME/u-forge/config.toml` (or `~/.config/u-forge/config.toml`)
3. Built-in defaults

Example `u-forge.toml` (see actual file for full example):
```toml
[embedding]
npu_enabled = true
gpu_enabled = true
gpu_weight = 40
cpu_enabled = false

[models.load_params]
"embed-gemma-300m-FLM"    = { ctx_size = 2048 }
"Qwen3-Embedding-8B-GGUF" = { ctx_size = 32768, batch_size = 2048, ubatch_size = 2048 }

[chat]
preferred_device = "gpu"
system_prompt = "You are a knowledgeable assistant..."

[chat.gpu]
model = "Gemma-4-26B-A4B-it-GGUF"
max_tokens = 262144

[chat.npu]
model = "qwen3.5-9b-FLM"
max_tokens = 32768
```

**Public API:**

```rust
// Discover catalog and build queue
let catalog = LemonadeServerCatalog::discover(&url).await?;
let selector = ModelSelector::new(catalog, config.models.clone());
let gpu_mgr  = Arc::new(GpuResourceManager::new());

let mut built = Vec::new();
for sel in selector.select_embedding_models() {
    if let Ok(p) = ProviderFactory::build(&sel, Capability::Embedding, &url, 10, Some(Arc::clone(&gpu_mgr))).await {
        built.push(p);
    }
}
// ... similarly for LLM, STT, TTS, reranker

let queue = InferenceQueueBuilder::new()
    .with_providers(built)
    .build();

// Submit jobs (all async)
let vec:   Vec<f32>  = queue.embed("The kingdom fell at dawn.").await?;
let vecs:  Vec<Vec<f32>> = queue.embed_many(texts).await?;
let text:  String    = queue.transcribe(wav_bytes, "session.wav").await?;
let audio: Vec<u8>   = queue.synthesize("Roll for initiative.", None).await?;

// Monitoring
let stats: QueueStats = queue.stats();
```

`InferenceQueue` is `Clone` (Arc internals) — hand copies to as many callers as
needed.

**Race-free worker wakeup:** each worker loop registers a `Notify::notified()`
future *before* checking the deque, preventing lost-wakeup races when a push
arrives between the check and the sleep. Embedding workers also manage an atomic
`idle` flag for dispatcher visibility.

**Integration test parallelism:** integration tests from multiple modules hit the
same Lemonade Server concurrently, causing intermittent GPU/NPU resource contention
on the server side. Always run the full test suite with `--test-threads=1`.
`dev.sh` sets this unconditionally. Integration tests auto-discover a running server
on `localhost:13305` via `GET /api/v1/health` — no `LEMONADE_URL` env var required.

---

## Embeddings (`src/ai/embeddings.rs`)

### EmbeddingProvider Trait

```rust
pub trait EmbeddingProvider: Send + Sync {
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;
    async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>>;
    fn dimensions(&self) -> Result<usize>;
    fn max_tokens(&self) -> Result<usize>;
    fn provider_type(&self) -> EmbeddingProviderType;
    fn model_info(&self) -> Option<EmbeddingModelInfo>;
}
```

`model_info()` returns `Option<EmbeddingModelInfo>` — our own type defined in
`embeddings.rs`, not `fastembed`'s type.

### LemonadeProvider

The only concrete implementation of `EmbeddingProvider`. Makes HTTP requests to a
running [Lemonade Server](https://github.com/lemonade-sdk/lemonade) instance.

- Fully async — no blocking mutexes on the Tokio thread pool.
- Endpoint: `POST {base_url}/embeddings` with a JSON body.
- `base_url` is the Lemonade Server URL (auto-discovered via `resolve_lemonade_url()`).
- **Default model: `embed-gemma-300m-FLM`** (NPU-accelerated, 300 M parameter
  Gemma embedding model running via the FLM recipe on the NPU).

**Batch fallback for FLM/NPU backends:** The FLM recipe processes one input at a
time and silently returns a single-item `data` array regardless of how many texts
are submitted. `embed_batch` detects the length mismatch (`data.len() != texts.len()`)
and automatically falls back to sequential single-item `embed` calls, so callers
always receive exactly one embedding per input. A `DEBUG`-level log message is
emitted when the fallback is triggered.

`FastEmbedProvider`, `Mutex<TextEmbedding>`, ONNX Runtime, and all ORT dependencies
have been removed entirely.

---

## Transcription (`src/ai/transcription.rs`)

### TranscriptionProvider Trait

```rust
#[async_trait]
pub trait TranscriptionProvider: Send + Sync {
    async fn transcribe(&self, audio_bytes: Vec<u8>, filename: &str) -> Result<String>;
    fn model_name(&self) -> &str;
}
```

`transcribe` returns the server's text trimmed of leading/trailing whitespace.
MIME type is inferred from the filename extension via `mime_for_filename()`:

| Extension | MIME |
|---|---|
| `.mp3` | `audio/mpeg` |
| `.ogg` | `audio/ogg` |
| `.flac` | `audio/flac` |
| `.m4a` | `audio/mp4` |
| anything else | `audio/wav` |

### LemonadeTranscriptionProvider

The concrete implementation:

- Endpoint: `POST {base_url}/audio/transcriptions` as `multipart/form-data`.
- Construction is **synchronous and cheap** — no probe request at construction.
- Server-side `{"error": …}` responses are propagated as `anyhow::Error`.
- Default model: `whisper-v3-turbo-FLM` (NPU FLM whisper).
- Trims trailing slashes from `base_url` on construction.

**Compared to `LemonadeSttProvider` in `lemonade.rs`:**

| | `LemonadeTranscriptionProvider` | `LemonadeSttProvider` |
|---|---|---|
| GPU resource management | None — runs on NPU or caller manages | `GpuResourceManager` RAII guard |
| Construction | Synchronous, no server probe | Requires `Arc<GpuResourceManager>` |
| Used by | `InferenceQueue` NPU worker, direct use | `InferenceQueue` GPU worker |

### Test Helper: `make_silence_wav`

`transcription::tests::make_silence_wav(duration_secs: f32) -> Vec<u8>` builds a
valid mono 16-bit 16 kHz PCM WAV file in memory with no external dependencies.
Used in transcription tests and copied inline into `inference_queue` tests.

---


## Extended Lemonade Integration (`src/lemonade/`)

### Catalog-Driven Provider Selection

The `hardware/` abstraction layer has been replaced by a catalog-driven flow:

1. **`LemonadeServerCatalog::discover(url)`** — fetches `/models`, `/system-info`, and `/health` concurrently. Exposes `models: Vec<CatalogModel>` (all catalog entries), `backends: Vec<InstalledBackend>` (installed recipe backends), and `loaded: Vec<LoadedModel>` (currently in RAM). Capability predicates (`has_npu()`, `has_gpu()`, etc.) are computed on-the-fly from backends.

2. **`ModelSelector::new(catalog, model_config)`** — picks the best downloaded model per capability and device slot. Selection methods:
   - `select_embedding_models()` → `Vec<SelectedModel>`, ≤1 per `(device_slot, QualityTier)`. Models in `high_quality_embedding_models` get `QualityTier::High` and are routed to the 4096-dim HQ index.
   - `select_llm_models()` → `Vec<SelectedModel>`, ≤1 per device slot.
   - `select_stt_models()` → `Vec<SelectedModel>`, ≤1 per device slot.
   - `select_reranker()` → `Option<SelectedModel>`.
   - `select_tts()` → `Option<SelectedModel>`.
   - `model_by_id(id, quality_tier)` → `Option<SelectedModel>` — exact catalog lookup, ignores preference lists. Used by `cli_chat` to honour `[chat.gpu]`/`[chat.npu]` model overrides.

3. **`ProviderFactory::build(sel, capability, url, queue_depth, gpu_mgr)`** — constructs a live provider from a `SelectedModel`. Branches on `capability` and `sel.recipe`/`sel.backend` to build the right type; loads the model via `POST /api/v1/load` with the configured `ModelLoadOptions`; returns `BuiltProvider { slot, capability, weight }`.

4. **`DuplicateGuard::check(selections)`** — validates that no llamacpp model ID appears across multiple backends (Lemonade Server limitation). FLM and other recipes are excluded from the check.

### GpuResourceManager

Enforces the GPU sharing policy between the two GPU workloads. Constructed once
and shared via `Arc<GpuResourceManager>` between `LemonadeSttProvider` and
`LemonadeChatProvider`.

| Request | GPU state | Outcome |
|---|---|---|
| STT | Idle | Acquired — returns `Ok(SttGuard)` |
| STT | LlmActive | **Error immediately** — STT is latency-sensitive, never queued |
| STT | SttActive | Error — already in use |
| LLM | Idle | Acquired — returns `LlmGuard` |
| LLM | SttActive | **Suspends** (async) until STT releases |
| LLM | LlmActive | Suspends — LLM requests are serialised |

**Implementation:** `parking_lot::Mutex<GpuWorkload>` (never held across `.await`)
+ `tokio::sync::Notify` for waking queued LLM tasks.

**RAII guards:** `SttGuard` and `LlmGuard` set `GpuWorkload::Idle` and call
`notify_waiters()` on drop. Callers cannot forget to release the GPU.

### Providers

- **`LemonadeTtsProvider`** — `POST /api/v1/audio/speech`. Returns `Vec<u8>` raw
  audio bytes (typically WAV). Supports named voices (`KokoroVoice` enum) and a
  per-instance default voice. Does **not** touch `GpuResourceManager` — kokoro
  runs entirely on the CPU.

- **`LemonadeSttProvider`** — `POST /api/v1/audio/transcriptions` via multipart
  form upload. Calls `begin_stt()` before the request; returns an error
  immediately if LLM inference is active. Returns `TranscriptionResult { text }`.

- **`LemonadeChatProvider`** — `POST /api/v1/chat/completions` (OpenAI-compatible).
  Calls `begin_llm().await` before the request, queuing if STT or another LLM is
  active. Convenience methods: `ask(prompt)`, `ask_with_system(system, prompt)`,
  `chat(messages)`, `complete(ChatRequest)` (with per-call `max_tokens` and
  `temperature` overrides).

### Model Loading (`src/lemonade/load.rs`)

`load_model(url, model_id, options)` calls `POST /api/v1/load` to ensure a model is
loaded before inference. Called by `ProviderFactory::build` with options from
`ModelConfig::load_options_for(model_id)`.

**`ModelLoadOptions`** controls context window and batch size at load time:

```rust
let opts = ModelLoadOptions {
    ctx_size: Some(2048),
    batch_size: Some(2048),
    ubatch_size: Some(2048),
    ..Default::default()
};
load_model(&url, "embed-gemma-300m-FLM", opts).await?;
```

**FLM vs llamacpp parameter handling:** FLM recipe models reject `llamacpp_backend`
and `llamacpp_args` — `load_model()` detects FLM models by recipe and omits those
fields. Non-FLM models have the backend and optional batch-size args injected
automatically.

---

## Search

### Full-Text Search

**`KnowledgeGraph::search_chunks_fts(query: &str, limit: usize) -> Vec<(ChunkId, ObjectId, String)>`**

Runs an FTS5 `MATCH` query against the `chunks_fts` virtual table. Supports phrase queries, prefix queries, and boolean operators natively.

### Semantic (Vector) Search

**Standard embeddings (768-dim):**
`KnowledgeGraph::search_chunks_ann(query_embedding: &[f32], limit: usize) -> Result<Vec<(ChunkId, ObjectId, String, f32)>>`

**High-quality embeddings (4096-dim):**
`KnowledgeGraph::search_chunks_ann_hq(query_embedding: &[f32], limit: usize) -> Result<Vec<(ChunkId, ObjectId, String, f32)>>`

Both query their respective sqlite-vec `vec0` virtual tables (`chunks_vec` and `chunks_vec_hq`) for the `limit` closest chunks by cosine distance. Returns `(chunk_id, object_id, content, distance)` tuples ordered by ascending distance (0.0 = identical, 2.0 = maximally dissimilar). Only chunks indexed via `upsert_chunk_embedding` or `upsert_chunk_embedding_hq` are candidates.

**768-dim index:**
Fixed at `EMBEDDING_DIMENSIONS = 768` (gemma family: `embed-gemma-300m-FLM` NPU, `embeddinggemma-300M-GGUF` CPU/GPU). **Mixing model families is forbidden** — incompatible spaces produce meaningless distances.

**4096-dim HQ index (optional):**
Fixed at `HIGH_QUALITY_EMBEDDING_DIMENSIONS = 4096` (e.g. Qwen3-Embedding-8B-GGUF). Only populated when the HQ model is available and `embedding.high_quality_embedding: true` in config. Allows finer-grained semantic search alongside the standard index.

### Hybrid Search (`src/search/`)

**`search_hybrid(graph, queue, query, config) -> Result<Vec<HybridSearchResult>>`**

Free async function that combines the two search primitives above with optional
cross-encoder reranking. Lives outside `KnowledgeGraph` to preserve the graph's
purely-synchronous, no-AI-dependency contract.

**Algorithm (5 stages):**

1. **FTS5** — `graph.search_chunks_fts(fts5_sanitize(query), config.fts_limit)`.
   Skipped when `alpha == 1.0`.
2. **Embed** — `queue.embed(query)` for the query vector.
   Skipped when `alpha == 0.0` or no embedding worker registered.
3. **Semantic ANN** — `graph.search_chunks_ann(&vec, config.semantic_limit)` (or `search_chunks_ann_hq` if HQ embeddings are available).
   Skipped when step 2 was skipped or failed.
4. **RRF merge** — Reciprocal Rank Fusion (`score = weight / (k + rank)`, k=60).
   Deduplicates by `chunk_id`, sums contributions from both paths, sorts descending,
   caps at `config.limit`. Chunks found by both paths naturally outscore single-path
   results.
5. **Rerank** — `queue.rerank(query, docs, top_n)` if `config.rerank` and a
   reranking worker is registered. Replaces RRF scores with cross-encoder scores.

**`HybridSearchConfig`** fields: `alpha` (0.0–1.0), `fts_limit`, `semantic_limit`,
`rerank: bool`, `limit`. `Default` provides `alpha=0.5`, pools of 20, rerank on,
top 10 results.

**`HybridSearchResult`** fields: `chunk_id`, `object_id`, `content`, `score`,
`sources: SearchSources`.

**`SearchSources`** tracks provenance: `fts_rank: Option<usize>`,
`semantic_distance: Option<f32>`, `rerank_score: Option<f32>`. The `label()` method
returns a human-readable string: `[FTS]`, `[SEM]`, `[FTS+SEM]`, `[FTS+SEM+RR]`.

**`fts5_sanitize(query) -> Option<String>`** — strips characters that are illegal
in FTS5 query syntax (e.g. `?`, `!`, `'`, `(`, `)`) before the SQLite `MATCH` call.
The original query is passed verbatim to `embed()` and `rerank()` — those go over
HTTP where punctuation is meaningful. Returns `None` for all-punctuation input,
causing the FTS stage to be skipped cleanly.

**Graceful degradation:** no embedding worker → FTS-only with `info!` log; embed
fails at runtime → FTS-only with `warn!`; no reranking worker → RRF scores with
`info!`; reranker fails → RRF scores with `warn!`. Never returns an error due to a
missing AI capability.

All public types are re-exported from `src/lib.rs`. 27 unit tests; all pass with
no Lemonade Server required (`MockEmbeddingProvider` + `TempDir`).

---

## Schema System (`src/schema/`)

- `SchemaDefinition` → named maps of `ObjectTypeSchema` and `EdgeTypeSchema`.
- `SchemaManager` caches schemas in `DashMap`, validates properties (type, regex,
  enums, min/max), persists to the `schemas` SQLite table.
- `SchemaIngestion` reads `defaults/schemas/*.schema.json`, strips the `add_`
  prefix from names (MCP naming convention), and adds 24 common TTRPG edge types
  automatically.
- `inheritance` field exists in `ObjectTypeSchema` but is still never acted on.

---

## Data Ingestion (`src/ingest/data.rs`)

Two-pass JSONL import: collect all nodes → create objects with name→ID map →
resolve edge names → create edges. Metadata strings `"key:value"` become
properties; plain strings become tags.

`create_objects` calls `find_by_name` before inserting — if a node with the same
type and name already exists, the existing ID is reused (no duplicates).
`resolve_node_id` calls `KnowledgeGraph::find_by_name_only` as a storage fallback
before failing, allowing edges to reference nodes from prior import sessions.

---

---

---

## Design Decisions — What's Good

- **`EmbeddingProvider` trait** — clean, async, extensible. `LemonadeProvider` slots
  in perfectly; adding future providers (local GGUF, OpenAI, etc.) requires only a
  new impl block.
- **SQLite with bundled feature** — zero system dependencies for storage. Single
  `.db` file is easy to inspect, back up, and migrate. FTS5 and future sqlite-vec
  are native extensions.
- **`ON DELETE CASCADE`** — referential integrity enforced by the database, not
  application code. Eliminates entire categories of dangling-reference bugs.
- **FTS5 auto-sync triggers** — `chunks_fts` stays consistent without application
  code involvement.
- **Schema system** — flexible JSON schemas with validation. External `.schema.json`
  files allow evolution without code changes.
- **`ObjectBuilder` fluent API** — ergonomic for constructing `ObjectMetadata`.
- **`EdgeType` as transparent newtype** — a plain `struct EdgeType(pub String)` with `::new()` / `.as_str()` is simpler and more correct than a single-variant enum. Relationship labels are open-ended strings in TTRPG worldbuilding; an enum adds friction without safety.
- **`EmbeddingQueue`** — well-architected async queue (needs integration, but solid).
- **JSONL + schema.json format** — sensible for local-first tooling.
- **Tests everywhere** — unit tests in every module using `TempDir` for isolation.

## Design Decisions — Questionable / Still Open

- **`chat.rs` must use hand-crafted HTTP, not `async-openai`** — Lemonade Server's
  chat endpoint deviates from the OpenAI spec at the thinking/reasoning parameter.
  OpenAI uses a `reasoning` object (or `reasoning_effort` string); Lemonade uses a
  flat `enable_thinking: bool` field at the request body root.  `async-openai`'s
  typed `CreateChatCompletionRequest` has no way to inject this field, so the crate
  cannot be used to drive the chat endpoint when thinking must be controlled.
  The other Lemonade endpoints (embeddings, TTS, STT) remain genuinely OpenAI-compatible
  and continue to use `async-openai` via `make_lemonade_openai_client`.
  Files that must change when `chat.rs` is migrated to direct `reqwest` calls:
  `src/lemonade/chat.rs` (replace `Client<OpenAIConfig>` + builder API with a
  `LemonadeHttpClient` + hand-rolled request/response structs, swap
  `ReasoningEffort` for `enable_thinking: Option<bool>`),
  `src/lemonade/mod.rs` (drop the `ChatReasoningEffort` re-export).
  `Cargo.toml` retains the `async-openai` dependency for the remaining three
  endpoints.

- **`embedding_manager` not in `KnowledgeGraph`** — embedding is now a caller
  concern. This simplifies the core struct but means callers must manage the
  embedding lifecycle separately from storage.
- **Schema naming `add_npc` vs `npc`** — `.schema.json` files are still named after
  MCP tool actions. `SchemaIngestion` strips the `add_` prefix, but the file names
  still leak an external convention.
- **`save_schema` is `async` but contains no `.await`** — `list_schemas` and `delete_schema` have been made sync. `save_schema` remains `async` because it is called with `.await` by `load_schema`, `register_object_type`, and `register_edge_type`; making it sync would require updating all those callers. Minor, but misleading.
- **`tags` and `properties` as JSON text** — stored as opaque strings, not as
  SQLite JSON1 columns. Filtering or querying inside these fields requires
  deserializing at the Rust layer. Acceptable for now; revisit if query patterns
  demand it.
- **`inheritance` in `ObjectTypeSchema` is never acted on** — still present as a
  schema field, still ignored at runtime.

---

## Dependencies

| Crate | Version | Role | Status |
|---|---|---|---|
| `rusqlite` | 0.32 | SQLite storage (bundled + vtab) | Primary storage |
| `sqlite-vec` | 0.1.7 | ANN vector search via `vec0` virtual table (bundles C source) | Active |
| `tokio` | 1.45 | Async runtime | Active |
| `serde` / `serde_json` | 1.0 | Serialization (all layers) | Active |
| `reqwest` | 0.12 | HTTP client — embeddings, TTS, STT, chat, registry (`json` + `multipart` features) | Active |
| `dashmap` | 6.1 | Concurrent maps (SchemaManager) | Active |
| `parking_lot` | 0.12 | Fast non-async mutex (GpuResourceManager, WorkQueue) | Active |
| `uuid` | 1.x | ID generation | Active |
| `anyhow` | 1.x | Error handling | Active |
| `async-trait` | 0.1 | Trait-object async methods | Active |
| `tracing` / `tracing-subscriber` | 0.1 | Structured logging | Active |
| `tempfile` | 3.x | Test isolation | Dev/test |

---

## Build Requirements

**Current:** Standard Rust stable toolchain + plain `gcc`/`g++` (for the bundled
SQLite C compilation via `rusqlite`'s build script). No GCC 13 requirement, no
system RocksDB, no ONNX Runtime download, no `source env.sh` required for
compilation.

`env.sh` still exists and sets optional environment variables (e.g. `LEMONADE_URL`
for runtime embedding calls), but sourcing it is not a prerequisite for `cargo
build` or `cargo test`.

---

## Sample Data

`defaults/data/memory.json`: ~220 nodes + ~312 edges modeling Isaac Asimov's
Foundation universe. JSONL format. Used by the CLI demo for end-to-end testing.

`defaults/schemas/`: 13 `.schema.json` files — `add_npc`, `add_player_character`,
`add_location`, `add_faction`, `add_quest`, `add_artifact`, `add_currency`,
`add_inventory`, `add_skills`, `add_temporal`, `add_setting_reference`,
`add_system_reference`, `add_transportation`. Loaded by `SchemaIngestion` at
startup; the `add_` prefix is stripped before storage.

