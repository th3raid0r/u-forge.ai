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

`defaults/` (schemas + sample data) stays at the workspace root. `cli_demo.rs` resolves it via `env!("CARGO_MANIFEST_DIR") + "/../../defaults/"`.

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
| `src/ai/embeddings.rs` | Lemonade HTTP embedding providers | `EmbeddingProvider` (trait), `LemonadeProvider`, `EmbeddingManager`, `EmbeddingModelInfo` |
| `src/ai/transcription.rs` | Audio-to-text providers | `TranscriptionProvider` (trait), `LemonadeTranscriptionProvider`, `TranscriptionManager`, `mime_for_filename` |
| `src/hardware/mod.rs` | Device abstraction layer | `DeviceCapability`, `HardwareBackend`, `DeviceWorker` (trait) |
| `src/hardware/npu.rs` | AMD NPU device (embedding + STT + LLM) | `NpuDevice` |
| `src/hardware/gpu.rs` | AMD GPU ROCm device (STT + LLM) | `GpuDevice` |
| `src/hardware/cpu.rs` | CPU device (TTS) | `CpuDevice` |
| `src/queue/dispatch.rs` | Unified MPMC capability-based dispatch queue | `InferenceQueue`, `QueueStats` |
| `src/queue/builder.rs` | Queue builder + device wiring | `InferenceQueueBuilder` |
| `src/queue/jobs.rs` | Internal job types + WorkQueue primitive | `EmbedJob`, `WorkQueue<T>` |
| `src/queue/workers.rs` | Background worker loops | `run_embed_worker`, `run_transcribe_worker`, etc. |
| `src/lemonade/` | Extended Lemonade integration (10 files) | `LemonadeModelRegistry`, `GpuResourceManager`, `LemonadeTtsProvider`, `LemonadeSttProvider`, `LemonadeChatProvider`, `LemonadeRerankProvider`, `SystemInfo`, `LemonadeStack` |
| `src/schema/definition.rs` | Schema definition types | `SchemaDefinition`, `ObjectTypeSchema`, `PropertySchema`, `EdgeTypeSchema` |
| `src/schema/manager.rs` | Schema load/validate/cache | `SchemaManager`, `SchemaStats` |
| `src/schema/ingestion.rs` | JSON schema file → internal | `SchemaIngestion` |
| `src/ingest/data.rs` | JSONL import pipeline | `DataIngestion`, `JsonEntry`, `IngestionStats` |
| `examples/cli_demo.rs` | Only runnable entry point (resolves `defaults/` via `CARGO_MANIFEST_DIR`) | — |

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
performed externally through `EmbeddingManager` and `EmbeddingQueue` as needed;
they are not part of the `KnowledgeGraph` lifecycle.

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

**`chunks_vec`** — sqlite-vec `vec0` virtual table for ANN similarity search.
```
rowid   INTEGER  (maps to chunks.rowid — same identity as chunks_fts)
embedding float[768] distance_metric=cosine
```
Populated explicitly via `upsert_chunk_embedding()`; not every chunk has an entry
immediately after creation. Kept clean by a `chunks_vec_ad` trigger that fires
`AFTER DELETE ON chunks`.

### Indexes

| Index | Table | Columns | Purpose |
|---|---|---|---|
| `idx_nodes_type` | nodes | object_type | Filter by type |
| `idx_nodes_name` | nodes | object_type, name | find_by_name (type-scoped) |
| `idx_nodes_name_only` | nodes | name | find_by_name_only (cross-type) |
| `idx_edges_source` | edges | source_id | Outgoing edge lookup |
| `idx_edges_target` | edges | target_id | Incoming edge lookup, cascade perf |
| `idx_chunks_object` | chunks | object_id | get_chunks_for_node |
| `chunks_vec_ad` trigger | chunks | rowid | Cascade-deletes vec entries when a chunk is deleted |

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
- **Vector index live** — `chunks_vec` is a sqlite-vec `vec0` virtual table (768-dim cosine). `upsert_chunk_embedding()` maps chunk rowids into the index. `search_chunks_semantic()` returns results ordered by ascending cosine distance.
- **Chunk size enforcement** — `add_text_chunk` splits content at word boundaries into ≤350-token pieces (`MAX_CHUNK_TOKENS`) before storage. Guards against the llamacpp 512-token batch limit. Uses the same `len/4` heuristic as `estimate_token_count`.

---

## Domain Types (types.rs)

Unchanged from the previous architecture. Key types:

- `ObjectMetadata`: `object_type: String` + `serde_json::Value` properties blob.
  Dynamic schema, no compile-time type enforcement.
- `EdgeType`: primarily `Custom(String)`. The four legacy variants (`Contains`,
  `OwnedBy`, `LocatedIn`, `MemberOf`) remain `#[deprecated]` for backward compat.
- `TextChunk`: content + estimated token count (`text.len() / 4`). Types:
  `Description`, `SessionNote`, `AiGenerated`, `UserNote`, `Imported`.
- `QueryResult`: aggregates objects + edges + chunks with a token budget system.
- `ChunkId`, `ObjectId`: newtype wrappers around `String` (UUID).

---

## Hardware & Inference Architecture

### Overview

AI inference is split across three hardware tiers, each isolated in its own module
under `src/hardware/`. A unified `InferenceQueue` in `src/queue/`
routes jobs to whichever device is both capable and free.

```
caller                InferenceQueue             device workers (Tokio tasks)
──────                ──────────────             ────────────────────────────

embed(text)  ───────► embed_queue   ───────────► NpuDevice  (embed-gemma-300m-FLM)

transcribe() ───────► transcribe_queue ─────────► NpuDevice  (whisper-v3-turbo-FLM)
                                       ─────────► GpuDevice  (Whisper-Large-v3-Turbo)

synthesize() ───────► synthesize_queue ─────────► CpuDevice  (kokoro-v1)
```

Both whisper workers (NPU and GPU) listen on the **same** `transcribe_queue`.
Whichever is free first picks up the job — natural work-stealing with no
coordination overhead.

### DeviceCapability / HardwareBackend / DeviceWorker

`src/hardware/mod.rs` defines the shared vocabulary:

- **`DeviceCapability`** — `Embedding`, `Transcription`, `TextGeneration`,
  `TextToSpeech`, `Reranking`
- **`HardwareBackend`** — `Npu`, `GpuRocm`, `GpuCuda`, `Cpu`, `Remote`
- **`DeviceWorker`** trait — `name()`, `backend()`, `capabilities()`, `supports()`,
  `summary()`

The queue uses only `DeviceCapability` for routing; `HardwareBackend` is
informational (logs, metrics).

### NpuDevice (`src/hardware/npu.rs`)

Wraps a `LemonadeProvider` (embedding) and an optional `LemonadeTranscriptionProvider`
(STT), both routed to FLM models on the AMD NPU via Lemonade Server.

| Default model | Capability |
|---|---|
| `embed-gemma-300m-FLM` | `Embedding` |
| `whisper-v3-turbo-FLM` | `Transcription` |

**No resource contention** — the NPU is dedicated silicon, separate from the GPU.
Multiple embedding and transcription calls may be in flight simultaneously.

Constructors: `NpuDevice::new()` (both), `NpuDevice::embedding_only()`,
`NpuDevice::transcription_only()`.

### GpuDevice (`src/hardware/gpu.rs`)

Wraps `LemonadeSttProvider` and/or `LemonadeChatProvider`, both sharing a single
`Arc<GpuResourceManager>` that enforces the GPU scheduling policy (see
[GpuResourceManager](#gpuresourcemanager) below).

| Default model | Capability |
|---|---|
| `Whisper-Large-v3-Turbo` | `Transcription` |
| `GLM-4.7-Flash-GGUF` | `TextGeneration` |

Constructors: `GpuDevice::from_registry()`, `GpuDevice::new()`,
`GpuDevice::stt_only()`, `GpuDevice::llm_only()`.

### CpuDevice (`src/hardware/cpu.rs`)

Wraps `LemonadeTtsProvider` (Kokoro). No GPU or NPU resource contention.

| Default model | Capability |
|---|---|
| `kokoro-v1` | `TextToSpeech` |

Constructors: `CpuDevice::from_registry()`, `CpuDevice::new()`,
`CpuDevice::new_with_voice()`, `CpuDevice::empty()`.

Convenience methods: `speak(text)`, `speak_with_voice(text, voice)`.

### InferenceQueue (`src/queue/`)

MPMC work queue built from `parking_lot::Mutex<VecDeque<T>> + tokio::sync::Notify`
per capability type — no additional crate dependencies.

**Public API:**

```rust
// Build the queue
let queue = InferenceQueueBuilder::new()
    .with_npu_device(npu)   // Embedding + Transcription
    .with_gpu_device(gpu)   // Transcription (GPU STT competes with NPU whisper)
    .with_cpu_device(cpu)   // TextToSpeech
    .build();               // spawns background Tokio tasks

// Submit jobs (all async, block until a worker picks them up)
let vec:   Vec<f32>  = queue.embed("The kingdom fell at dawn.").await?;
let vecs:  Vec<Vec<f32>> = queue.embed_many(texts).await?;
let text:  String    = queue.transcribe(wav_bytes, "session.wav").await?;
let audio: Vec<u8>   = queue.synthesize("Roll for initiative.", None).await?;

// Monitoring
let stats: QueueStats = queue.stats(); // pending_{embeddings,transcriptions,syntheses}
```

`InferenceQueue` is `Clone` (Arc internals) — hand copies to as many callers as
needed.

**Race-free worker wakeup:** each worker loop registers a `Notify::notified()`
future *before* checking the deque, preventing lost-wakeup races when a push
arrives between the check and the sleep.

**Integration test parallelism:** integration tests from multiple modules hit the
same Lemonade Server concurrently, causing intermittent GPU/NPU resource contention
on the server side. Always run the full test suite with `--test-threads=1`.
`dev.sh` sets this unconditionally. Integration tests auto-discover a running server
on `localhost:8000` via `GET /api/v1/health` — no `LEMONADE_URL` env var required.

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
- `base_url` is resolved by `EmbeddingManager::try_new_auto` (see below).
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

### EmbeddingManager

Holds a single `Arc<dyn EmbeddingProvider>`. `try_new_auto()` constructs a
`LemonadeProvider` using the following resolution order:

1. Explicit `lemonade_url` argument (if provided)
2. `LEMONADE_URL` environment variable
3. Localhost probe — `resolve_lemonade_url()` tries `http://localhost:8000` then
   `http://127.0.0.1:8000` via `GET /api/v1/health` (2 s timeout)
4. Hard error — no server could be found

Defaults to model `embed-gemma-300m-FLM` when no model is specified.
No `embedding_cache_dir` parameter.

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
| Used by | `NpuDevice`, `TranscriptionManager`, `InferenceQueue` NPU worker | `GpuDevice`, `LemonadeStack`, `InferenceQueue` GPU worker |

### TranscriptionManager

Mirrors `EmbeddingManager`. Holds a single `Arc<dyn TranscriptionProvider>`.

```rust
// Auto: reads LEMONADE_URL env var; defaults to "whisper-v3-turbo-FLM"
let mgr = TranscriptionManager::try_new_auto(None, None)?;

// Explicit
let mgr = TranscriptionManager::new_lemonade(
    "http://localhost:8000/api/v1",
    "whisper-v3-turbo-FLM",
);

// From an arbitrary provider (useful in tests)
let mgr = TranscriptionManager::from_provider(Arc::new(my_provider));

let text = mgr.get_provider().transcribe(wav_bytes, "session.wav").await?;
```

`try_new_auto` is **synchronous**. Resolution order:
1. `lemonade_url` argument (if `Some`)
2. `LEMONADE_URL` environment variable
3. Hard error — no silent fallback (transcription does not perform a localhost probe
   at construction time; errors surface on the first `transcribe()` call)

### Test Helper: `make_silence_wav`

`transcription::tests::make_silence_wav(duration_secs: f32) -> Vec<u8>` builds a
valid mono 16-bit 16 kHz PCM WAV file in memory with no external dependencies.
Used in transcription tests and copied inline into `inference_queue` tests.

---


## Extended Lemonade Integration (`src/lemonade/`)

### Hardware Assignment

| Component | Hardware | Model |
|---|---|---|
| `LemonadeTtsProvider` | CPU | `kokoro-v1` (kokoro recipe) |
| `LemonadeSttProvider` | GPU | `Whisper-Large-v3-Turbo` (whispercpp recipe) |
| `LemonadeChatProvider` | GPU | `GLM-4.7-Flash-GGUF` (llamacpp recipe) |
| NPU embedding (via `LemonadeProvider`) | NPU | `embed-gemma-300m-FLM` (flm recipe) |

### LemonadeModelRegistry

Fetches `GET /api/v1/models` and classifies each entry into a `ModelRole`:
`NpuEmbedding`, `CpuTts`, `GpuStt`, `GpuLlm`, `Reranker`, `ImageGen`, `Other`.

Classification is rule-based (recipe + labels):
- FLM recipe + `embeddings` label → `NpuEmbedding`
- kokoro recipe, or `tts`/`speech` label → `CpuTts`
- whispercpp recipe, or `transcription` label → `GpuStt`
- llamacpp/flm recipe + `reasoning`/`tool-calling`/`vision` label → `GpuLlm`

Convenience accessors: `npu_embedding_model()`, `tts_model()`, `stt_model()`,
`llm_model()`, `by_role(role)`, `summary()`.

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

### LemonadeStack

One-call builder that fetches the registry and wires all three providers to a
single shared `Arc<GpuResourceManager>`:

```rust
let stack = LemonadeStack::build("http://127.0.0.1:8000/api/v1").await?;
let audio  = stack.tts.synthesize_default("Roll for initiative.").await?;
let answer = stack.chat.ask("Describe a dragon.").await?;
```

---

## Search

### Full-Text Search

**`KnowledgeGraph::search_chunks_fts(query: &str, limit: usize) -> Vec<(ChunkId, ObjectId, String)>`**

Runs an FTS5 `MATCH` query against the `chunks_fts` virtual table. Supports phrase queries, prefix queries, and boolean operators natively.

### Semantic (Vector) Search

**`KnowledgeGraph::search_chunks_semantic(query_embedding: &[f32], limit: usize) -> Vec<(ChunkId, ObjectId, String, f32)>`**

Queries the `chunks_vec` sqlite-vec `vec0` virtual table for the `limit` closest chunks by cosine distance. Returns `(chunk_id, object_id, content, distance)` tuples ordered by ascending distance (0.0 = identical, 2.0 = maximally dissimilar). Only chunks indexed via `upsert_chunk_embedding` are candidates.

The embedding dimension is fixed at `EMBEDDING_DIMENSIONS = 768` (matching `embed-gemma-300m-FLM` and `nomic-embed-text-v2-moe-GGUF`). The constant and the `vec0` DDL must stay in sync; recreate the database if the model changes.

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
3. **Semantic ANN** — `graph.search_chunks_semantic(&vec, config.semantic_limit)`.
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
- **`EdgeType::Custom(String)`** — correct over rigid enums.
- **`EmbeddingQueue`** — well-architected async queue (needs integration, but solid).
- **JSONL + schema.json format** — sensible for local-first tooling.
- **Tests everywhere** — unit tests in every module using `TempDir` for isolation.

## Design Decisions — Questionable / Still Open

- **`embedding_manager` not in `KnowledgeGraph`** — embedding is now a caller
  concern. This simplifies the core struct but means callers must manage the
  embedding lifecycle separately from storage.
- **Schema naming `add_npc` vs `npc`** — `.schema.json` files are still named after
  MCP tool actions. `SchemaIngestion` strips the `add_` prefix, but the file names
  still leak an external convention.
- **Async schema validation with no async work** — `SchemaManager` methods are
  `async` but contain no `.await` points. Minor, but misleading.
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

---

## Remaining Roadmap

| Item | Description |
|---|---|
| axum HTTP/WebSocket server | Wraps `KnowledgeGraph` + `InferenceQueue` behind HTTP and WebSocket endpoints. `KnowledgeGraph` is `Send + Sync` and `Arc`-wrapped; `LemonadeStack`, `InferenceQueue`, and `search_hybrid` slot into an `AppState`. `POST /api/search` will call `search_hybrid` directly. |
| Streaming LLM responses | `InferenceQueue::generate_stream()` returning `tokio::sync::mpsc::Receiver<String>` of token chunks for axum WebSocket delivery. |
| UI feature | Native GPUI graph visualization (`u-forge-graph-view` view model + `u-forge-ui-gpui` app). Skeleton crates exist in `crates/`. See `feature_UI.md`. |
| TS sandbox feature | Embedded V8 TypeScript sandbox (`u-forge-ts-runtime` with `deno_core` ops). Skeleton crate exists in `crates/`. See `feature_TS-Agent-Sandbox.md`. |