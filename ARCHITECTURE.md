# u-forge.ai — Architecture Reference

High-level architecture, data model, storage schema, inference design, and design decisions. For module maps and file indexes, see `.rulesdir/project-structure.mdc`.

---

## Workspace Layout

| Crate | Kind | Status | Purpose |
|-------|------|--------|---------|
| `u-forge-core` | lib | Complete | Storage, AI traits, Lemonade integration, queue, search, schema, ingest |
| `u-forge-graph-view` | lib | Complete | Graph view model + force-directed layout + R-tree spatial index |
| `u-forge-ui-traits` | lib | Complete | Framework-agnostic rendering contracts (`DrawCommands`, `Viewport`, `generate_draw_commands`) |
| `u-forge-ui-gpui` | lib + bin | Alpha | GPUI native desktop app — graph canvas, node editor, search, chat |
| `u-forge-agent` | lib | Complete | Rig-based LLM agent with five graph tools and streaming event loop |
| `u-forge-ts-runtime` | lib | Skeleton | Embedded deno_core TypeScript sandbox — not started |

`defaults/` (schemas + sample data) lives at the workspace root.

---

## Core Data Model

### KnowledgeGraph

The public facade, composed from two `Arc`-wrapped subsystems:

```rust
pub struct KnowledgeGraph {
    storage: Arc<KnowledgeGraphStorage>,
    schema_manager: Arc<SchemaManager>,
}
```

`KnowledgeGraph` has **no** embedding fields, no `InferenceQueue`, and no server dependency. Storage and schema operations are fully synchronous; AI capabilities are opt-in and constructed separately. This decoupling means the graph works in tests without any running server.

**Constructor:** `KnowledgeGraph::new(db_path: &Path)` — one argument. Creates `<db_path>/knowledge.db` automatically.

**Bulk access methods** (added for UI performance):
- `get_all_edges()` — single `SELECT * FROM edges`; use instead of repeated `get_relationships()` when building a snapshot.
- `get_nodes_paginated(offset, limit)` — `ORDER BY name LIMIT ? OFFSET ?` for incremental snapshots.

### Domain Types

- `ObjectMetadata` — `object_type: String` + `properties: serde_json::Value`. Dynamic schema; no compile-time enforcement.
- `EdgeType` — transparent newtype `struct EdgeType(pub String)`. Construct with `::new(s)`; read with `.as_str()`. No enum variants — relationship labels are open-ended strings.
- `ObjectId`, `ChunkId` — newtype structs wrapping `Uuid` (`#[serde(transparent)]`). The compiler rejects passing a `ChunkId` where an `ObjectId` is expected. Construct with `::new_v4()`; parse with `::parse_str(s)`.
- `TextChunk` — content + token count (`len.div_ceil(3)` ≈ 3 chars/token, conservative for dense prose). Types: `Description`, `SessionNote`, `AiGenerated`, `UserNote`, `Imported`.

---

## Storage (SQLite)

Single SQLite database file via `rusqlite` with the `bundled` feature — no system SQLite required. `parking_lot::Mutex` wraps the connection; no async locking.

### Tables

**`nodes`**
```
id TEXT PRIMARY KEY, object_type TEXT NOT NULL, schema_name TEXT,
name TEXT NOT NULL, properties TEXT NOT NULL DEFAULT '{}',
created_at TEXT NOT NULL, updated_at TEXT NOT NULL
```
`properties` is a JSON object storing all schema fields including `"description"` and `"tags"`. No separate columns. Atomic single-property updates use SQLite's `json_set()` via `set_node_property()`.

**`edges`**
```
source_id TEXT REFERENCES nodes(id) ON DELETE CASCADE,
target_id TEXT REFERENCES nodes(id) ON DELETE CASCADE,
edge_type TEXT NOT NULL, weight REAL DEFAULT 1.0, metadata TEXT DEFAULT '{}',
created_at TEXT NOT NULL,
UNIQUE(source_id, target_id, edge_type)
```

**`chunks`**
```
id TEXT PRIMARY KEY, object_id TEXT REFERENCES nodes(id) ON DELETE CASCADE,
chunk_type TEXT NOT NULL, content TEXT NOT NULL, token_count INTEGER DEFAULT 0,
created_at TEXT NOT NULL
```

**`schemas`** — `name TEXT PRIMARY KEY, definition TEXT NOT NULL` (JSON)

**`chunks_fts`** — FTS5 virtual table mirroring `chunks(content)`. Auto-populated and auto-updated via `AFTER INSERT/UPDATE/DELETE` triggers on `chunks`. Never manually insert.

**`chunks_vec`** — sqlite-vec `vec0` table, 768-dim cosine distance.
```
rowid INTEGER (maps to chunks.rowid), embedding float[768] distance_metric=cosine
```
Populated via `upsert_chunk_embedding()`. Not every chunk has an entry immediately. Cleaned by `chunks_vec_ad` trigger on `AFTER DELETE ON chunks`.

**`chunks_vec_hq`** — sqlite-vec `vec0` table, 4096-dim cosine distance.
```
rowid INTEGER (maps to chunks.rowid), embedding float[4096] distance_metric=cosine
```
Optional — populated only when a high-quality embedding model (e.g. `Qwen3-Embedding-8B-GGUF`) is available and `embedding.high_quality_embedding: true` in config.

**`node_positions`** — canvas layout positions.
```
node_id TEXT PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE, x REAL, y REAL,
layout_version INTEGER DEFAULT 1
```
Written by `save_layout()` after drag. Read by `build_snapshot()` to restore user-arranged positions.

**`schema_metadata`** — open-time validation key/value store.
```
key TEXT PRIMARY KEY, value TEXT NOT NULL
```
Holds `chunks_vec_dims` and `chunks_vec_hq_dims`. `check_or_init_embedding_dims` runs inside `KnowledgeGraphStorage::new` — first open writes the compiled-in constants; subsequent opens compare stored vs. compiled values and return `EmbeddingDimensionMismatch` (a `thiserror` struct re-exported from `u_forge_core`) if they differ. No auto-migration: the user must re-index or pin the old model.

### Storage Design Notes

- `ON DELETE CASCADE` on both `edges` and `chunks` — node deletion is a single `DELETE FROM nodes WHERE id = ?`; the database removes all dependent rows automatically. O(log N), not O(N).
- Edge uniqueness `UNIQUE(source_id, target_id, edge_type)` replaces old manual adjacency-list deduplication.
- `ON CONFLICT DO UPDATE` on `chunks` — preserves the implicit SQLite `rowid` that `chunks_fts` references. Do not use `INSERT OR REPLACE` on chunks.
- `INSERT OR REPLACE` on `nodes` is safe — no cascading rowid dependencies to preserve.
- Chunk size: `add_text_chunk` splits at word boundaries into ≤350-token pieces (`MAX_CHUNK_TOKENS`). Uses `len.div_ceil(3)` heuristic (≈ 1,050 chars per chunk). Guards against the llamacpp 512-token batch limit.
- All complex fields (tags, properties, metadata) stored as JSON text. UUIDs as hyphenated `TEXT`. Datetimes as RFC 3339 `TEXT`.
- FKs enabled at connection time: `PRAGMA foreign_keys = ON`.

---

## Hardware & Inference Architecture

### Catalog-Driven Selection Flow

```
LemonadeServerCatalog::discover(url)
  ├─ GET /api/v1/models   (all catalog entries + download status)
  ├─ GET /system-info     (installed recipe backends)
  └─ GET /api/v1/health   (currently loaded models)

ModelSelector::new(catalog, config)
  ├─ select_embedding_models()  → ≤1 per (device_slot, QualityTier)
  ├─ select_llm_models()        → ≤1 per device_slot
  ├─ select_stt_models()        → ≤1 per device_slot
  ├─ select_tts()               → best downloaded TTS model
  ├─ select_reranker()          → best downloaded reranker
  └─ model_by_id(id, tier)     → exact lookup (bypasses preference lists)

ProviderFactory::build(sel, capability, url, queue_depth, gpu_mgr, already_loaded)
  → BuiltProvider { slot, capability, weight }

InferenceQueueBuilder::new()
  .with_providers(built_providers)
  .build()  → InferenceQueue
```

**`already_loaded`:** pass `catalog.loaded` IDs to skip the `/api/v1/load` round-trip for models already in RAM. `do_init_lemonade()` fires all provider builds concurrently via `futures::future::join_all` after extracting this list — eliminating sequential load waits on warm servers.

**Device slots** — deduplication key in `ModelSelector`:
- `flm` recipe → `"npu"`
- `llamacpp` + `rocm`/`vulkan`/`metal` → `"gpu"`
- `llamacpp` + cpu → `"cpu"`
- other recipes (e.g. `whispercpp`, `kokoro`) → recipe name

**Hardware capability mapping:**

| Hardware | Recipes | Capabilities |
|---|---|---|
| AMD NPU (XDNA2) | FLM | Embedding, Transcription, TextGeneration |
| AMD iGPU (ROCm/Vulkan) | llamacpp:rocm, llamacpp:vulkan, whispercpp:vulkan | TextGeneration, Transcription, Reranking, Embedding (via GGUF) |
| CPU | kokoro, llamacpp (cpu) | TextToSpeech, Embedding (via GGUF) |

### InferenceQueue Design

MPMC work queue built from `parking_lot::Mutex<VecDeque<T>> + tokio::sync::Notify` per capability channel — no extra crate dependencies. Five channels: `WeightedEmbedDispatcher`, `transcribe_queue`, `generate_queue`, `synthesize_queue`, `rerank_queue`.

**Weighted embedding dispatch** (`src/queue/weighted.rs`):
- Each worker tracks an EWMA (α=0.5) of job duration in microseconds.
- Routing cost per worker: `(pending_jobs + 1) × ewma_duration_us`.
- Dispatcher picks the worker with lowest predicted completion time. Static weight (NPU=100, GPU=50, CPU=10) breaks ties only.
- EWMA converges after the first job; fast devices dominate routing naturally.

**Work stealing** — when a worker empties its queue, it calls `steal_from_busiest()` to grab one job from the most-loaded worker. `global_notify` on every `submit()` wakes idle workers. A GPU that is 10× faster than NPU drains the NPU backlog without extra synchronisation.

**Race-free wakeup:** workers register `Notify::notified()` *before* checking the deque, preventing lost-wakeup when a push arrives between check and sleep.

### GPU Sharing Policy (`GpuResourceManager`)

`LemonadeSttProvider` and `LemonadeChatProvider` share a single `Arc<GpuResourceManager>`. Enforced via RAII guards that release the GPU on drop.

| Request | GPU state | Outcome |
|---|---|---|
| STT | Idle | Acquired — `Ok(SttGuard)` |
| STT | LlmActive | **Error immediately** — STT is latency-sensitive, never queued |
| STT | SttActive | Error — already in use |
| LLM | Idle | Acquired — `LlmGuard` |
| LLM | SttActive | **Suspends** (async) until STT releases |
| LLM | LlmActive | Suspends — LLM requests are serialised |

Implementation: `parking_lot::Mutex<GpuWorkload>` (never held across `.await`) + `tokio::sync::Notify` to wake queued LLM tasks.

---

## Search Pipeline

### Hybrid Search (`src/search/`)

`search_hybrid(graph, queue, query, config)` — five-stage pipeline:

1. **FTS5** — `graph.search_chunks_fts(fts5_sanitize(query), fts_limit)`. Skipped when `alpha == 1.0`.
2. **Embed** — `queue.embed(query)`. Skipped when `alpha == 0.0` or no embedding worker.
3. **Semantic ANN** — `search_chunks_ann` (768-dim) or `search_chunks_ann_hq` (4096-dim when available). Skipped if step 2 was skipped or failed.
4. **RRF merge** — Reciprocal Rank Fusion (`score = weight / (k + rank)`, k=60). Deduplicates by `chunk_id`, sums contributions from both paths, caps at `config.limit`. Chunks found by both paths naturally outscore single-path results.
5. **Rerank** — `queue.rerank(query, docs, top_n)` if `config.rerank` and a reranker is registered. Replaces RRF scores with cross-encoder scores.

Graceful degradation at every stage: missing worker → skip that stage with `info!`; runtime failure → skip that stage with `warn!`. Never returns an error due to a missing AI capability.

`fts5_sanitize` strips characters illegal in FTS5 query syntax before `MATCH`; the original query is passed verbatim to `embed()` and `rerank()` where punctuation is meaningful. Returns `None` for all-punctuation input (FTS stage cleanly skipped).

The 768-dim and 4096-dim vector spaces are **fixed and incompatible** — do not mix model families. Changing the embedding model without re-indexing is caught at DB open time (`EmbeddingDimensionMismatch`, re-exported from `u_forge_core`) rather than silently corrupting the vector index.

---

## Schema System (`src/schema/`)

`SchemaDefinition` holds named maps of `ObjectTypeSchema` and `EdgeTypeSchema`. `prompt_summary()` generates a compact markdown block (node types with property names/types/required flags + edge types) for system prompt injection.

`SchemaManager` caches schemas in `parking_lot::RwLock<HashMap>`. Validation helpers (`is_valid_object_type`, `all_object_type_names`, etc.) read from the in-memory cache without touching SQLite. `validate_and_coerce_properties` coerces `String("42")` → `Number` and `String("true"/"false")` → `Bool` in-place; returns `Vec<PropertyIssue>` for type mismatches and invalid enum values. Unknown properties are silently accepted.

`KnowledgeGraph::schema_prompt_summary_all()` merges all persisted schemas and returns `prompt_summary()` output — used by `GraphAgent::new` to inject schema context into the system prompt.

`SchemaIngestion` reads `defaults/schemas/*.schema.json`, strips the `add_` prefix (MCP naming convention), and adds 24 common TTRPG edge types automatically.

---

## Data Ingestion (`src/ingest/`)

**Two-pass JSONL import** (`data.rs`): collect all nodes → create objects with name→ID map → resolve edge names → create edges. `create_objects` deduplicates by type+name (existing node ID reused). `resolve_node_id` calls `find_by_name_only` as a storage fallback, allowing edges to reference nodes from prior import sessions.

**Per-node re-chunking** (`embedding.rs`): `rechunk_and_embed(graph, queue, hq_queue, object_id)` — delete old chunks (cascades FTS5 + vector indexes) → flatten via `flatten_for_embedding()` → create new chunks → embed standard (768-dim) → embed HQ (4096-dim) if `hq_queue` provided. Blocks until all embeddings are stored. Write tools and UI save both call this to guarantee immediate searchability after the call returns.

`EmbeddingPlan` is the declarative UI entry point: `EmbeddingPlan::rechunk(ids)` for per-node re-chunk + embed, `EmbeddingPlan::embed_all()` for bulk unembedded sweep. `AppView::run_embedding_plan(plan, cx)` is the single UI call site — owns status formatting, epoch-based poller cancellation, and the background tokio task. The spawned future is attached to an `info_span!("embedding_plan", plan_kind)` before detaching; `do_init_lemonade` uses the same `.instrument(info_span!(...))` pattern.

---

## Chat UI Component Model (`u-forge-ui-gpui`)

### Component hierarchy

```
AppView
  └─ ChatPanel  (Entity<ChatPanel>)
       ├─ messages: Vec<Entity<ChatMessageView>>
       ├─ stream_task: Option<gpui::Task<()>>   — stored (not detached) for cancellation
       ├─ connecting: bool                       — true while do_init_lemonade is in-flight
       └─ list_state: ListState                 — virtualized list; reset() on any structural change
            └─ item builder closure (render-site action bar)
                 └─ Entity<ChatMessageView>
                      └─ body: Option<Entity<TextFieldView>>  — None for ToolCall rows
```

### Key design rules

**`stream_task` must be stored, never detached.** Dropping the `Task<()>` cancels the outer GPUI spawn, which drops the `mpsc::Receiver`, causing `tx.send().await` inside `prompt_stream` to return `Err` and break the stream loop. `finalize_stream` sets it back to `None`. This is the Stop button's cancellation mechanism.

**Action bar lives in the list item builder, not in `ChatMessageView`.** The ⟳ retry, × delete, and ⎘ copy buttons are rendered by the closure in `chat_panel.rs` that builds each list item — not inside `ChatMessageView::render`. This eliminates per-message `gpui::Subscription` vectors. See `.rulesdir/gpui-patterns.mdc` — "Render-site action bar pattern".

**`TextFieldView` serves two roles.** The same widget is used as the editable chat input (bottom) and as the read-only, selectable body of User/Assistant/Thinking messages. Construct with `TextFieldView::new_read_only(text, color, cx)` for message bodies. ToolCall rows skip the body entity and render plain divs.

**`ConnectRequested` event bridges `ChatPanel` → `AppView`.** `ChatPanel` cannot call `do_init_lemonade` directly (it doesn't hold the full app state). Instead it emits `ConnectRequested`; `AppView` subscribes, calls `do_init_lemonade`, and calls `ChatPanel::set_connecting(false)` on completion or `set_connect_failed(msg)` on failure.

**Send button is four-state, width-pinned.** States: Connect (yellow) / Connecting… (grey) / Send (blue) / Stop (red). Width is pinned to 88 px so the input row doesn't reflow on state change.

---

## Design Decisions — Questionable / Still Open

- **`chat.rs` uses hand-crafted HTTP, not `async-openai`** — Lemonade Server's `enable_thinking: bool` parameter is non-standard; `async-openai`'s typed struct has no way to inject it. The other Lemonade endpoints (embeddings, TTS, STT, reranking) remain genuinely OpenAI-compatible and continue using `async-openai`. If Lemonade ever standardises the thinking parameter, `LemonadeChatProvider` can be ported.
- **`properties` as JSON text** — stored as an opaque string. Filtering inside the blob requires deserializing at the Rust layer, or using `json_set`/`json_extract` for targeted mutations. Acceptable for now; revisit if query patterns demand indexed property access.
- **Schema naming `add_npc` vs `npc`** — `.schema.json` files are named after MCP tool actions. `SchemaIngestion` strips the `add_` prefix, but the file names leak an external convention.
- **`save_schema` is `async` but has no `.await`** — called with `.await` by several callers; making it sync would require updating all of them. Minor but misleading.
- **`embedding_manager` not in `KnowledgeGraph`** — embedding is now a caller concern. Simplifies the core struct but means callers must manage the embedding lifecycle separately from storage.

---

## Dependencies

| Crate | Version | Role |
|---|---|---|
| `rusqlite` | 0.32 | SQLite storage (`bundled` + `vtab` features) |
| `sqlite-vec` | 0.1.7 | ANN vector search via `vec0` virtual table |
| `tokio` | 1.45 | Async runtime |
| `serde` / `serde_json` | 1.0 | Serialization (all layers) |
| `reqwest` | 0.12 | HTTP client — all Lemonade endpoints |
| `parking_lot` | 0.12 | Non-async mutex (storage, queue, GPU manager) |
| `dashmap` | 6.1 | Concurrent maps (SchemaManager) |
| `uuid` | 1.x | ID generation |
| `anyhow` / `thiserror` | 1.x | Error handling |
| `async-trait` | 0.1 | Trait-object async methods |
| `tracing` / `tracing-subscriber` | 0.1 | Structured logging |
| `rig-core` | 0.35.0 | LLM agent framework (`u-forge-agent`) |
| `gpui` | 0.2.2 | GPU-accelerated UI framework (`u-forge-ui-gpui`, crates.io release) |
| `glam` | — | Vector math (`u-forge-graph-view`, `u-forge-ui-traits`) |
| `rstar` | — | R-tree spatial index (`u-forge-graph-view`) |
| `tempfile` | 3.x | Test isolation (dev/test) |

---

## Patched / Vendored Dependencies

### `crates/cosmic-text-patched` — ShapePlan cache backport

**What:** A local copy of `cosmic-text 0.14.2` (the version pulled in by `gpui 0.2.2`) with a single targeted fix backported from `cosmic-text 0.17.1`. Activated via `[patch.crates-io]` in the root `Cargo.toml` — `gpui` picks it up transparently.

**Why it exists:** `cosmic-text 0.14.2` creates a new `rustybuzz::ShapePlan` on every word of every cold-cache text shape call. A `ShapePlan` compiles the font's OpenType layout tables (GSUB/GPOS feature lookup via `hb_ot_map_builder_t::compile` → `find_language_feature`) — an operation costing several milliseconds per call. With `gpui`'s frame-scoped `LineLayoutCache`, any message that scrolls off-screen has its line layouts evicted; on scroll-back every line re-shapes, paying this cost for every word on every line simultaneously. A 4 KB assistant message (~87 lines × ~8 words each) produced a measured ~550 ms freeze confirmed via `samply` flamegraph (89% of samples in `shape_text`, 77% in `find_language_feature`).

**The fix** (`crates/cosmic-text-patched/src/shape.rs`):
- Added `shape_plan_cache: VecDeque<(fontdb::ID, rustybuzz::Direction, rustybuzz::Script, rustybuzz::ShapePlan)>` to `ShapeBuffer`.
- `shape_fallback` checks the cache (keyed on font + direction + script) before calling `ShapePlan::new`. Plans are reused across all lines sharing the same font/direction/script combination. Cache is capped at 6 entries (FIFO eviction).
- No public API changes — the patch is invisible to `gpui`.

**Why `cosmic-text 0.17.1` fixes it:** Version 0.17.1 (used by Zed's internal `gpui` fork) introduced the same `VecDeque` plan cache. `gpui 0.2.2` on crates.io cannot be bumped to use 0.17.x directly because the crate has not been republished with updated deps.

**Maintenance — how to check for an upstream fix:**

1. Check whether a new `gpui` version has been published to crates.io:
   ```sh
   cargo search gpui
   ```
   If `gpui > 0.2.2` appears, inspect its `Cargo.toml` for `cosmic-text` version. If it depends on `cosmic-text >= 0.17`, remove the patch.

2. Alternatively, check the `cosmic-text` version pulled in by `gpui`:
   ```sh
   cargo tree -p gpui --depth 1 | grep cosmic
   ```
   If the resolved version is `>= 0.17`, the upstream fix is present and the patch can be dropped.

**Maintenance — how to remove the patch once upstream is fixed:**

1. Delete `crates/cosmic-text-patched/` from the workspace.
2. Remove the `[patch.crates-io]` block and its comment from `Cargo.toml`.
3. Remove `cosmic-text-patched` from the `members` glob (it auto-excludes when the directory is gone since members uses `crates/*`).
4. Run `cargo build -p u-forge-ui-gpui` to confirm the upstream version compiles and `cargo test --workspace -- --test-threads=1` to verify nothing regressed.
5. Run under `samply` during a chat scroll to confirm the `find_language_feature` hotspot is gone from the flamegraph.

---

## Build Requirements

Standard Rust stable toolchain + a C compiler (`gcc`, `clang`, or MSVC) for bundled SQLite compilation. No system SQLite, no ONNX Runtime, no RocksDB. `source env.sh` is not required.

`cargo run -p u-forge-ui-gpui` works with zero environment variables set — Lemonade Server on `localhost:13305` is auto-discovered; all AI features degrade gracefully when absent.
