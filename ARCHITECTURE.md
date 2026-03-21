# u-forge.ai — Architecture Reference

> This document describes the **current** state of the codebase as of the latest review.
> For the planned migration, see [migration.md](migration.md).

## Module Map

| File | Lines | Role | Key Types |
|---|---|---|---|
| `src/lib.rs` | ~300 | Facade + ObjectBuilder + tests | `KnowledgeGraph`, `ObjectBuilder` |
| `src/types.rs` | ~260 | All domain types | `ObjectMetadata`, `Edge`, `EdgeType`, `TextChunk`, `QueryResult` |
| `src/storage.rs` | ~670 | RocksDB persistence (5 CFs) | `KnowledgeGraphStorage`, `AdjacencyList`, `GraphStats` |
| `src/embeddings.rs` | ~240 | FastEmbed wrapper | `EmbeddingProvider` (trait), `FastEmbedProvider`, `EmbeddingManager` |
| `src/vector_search.rs` | ~720 | HNSW + FST hybrid search | `VectorSearchEngine`, `HnswVectorStore`, `HybridSearchResult` |
| `src/schema.rs` | ~590 | Schema definition types | `SchemaDefinition`, `ObjectTypeSchema`, `PropertySchema`, `EdgeTypeSchema` |
| `src/schema_manager.rs` | ~560 | Schema load/validate/cache | `SchemaManager`, `SchemaStats` |
| `src/schema_ingestion.rs` | ~530 | JSON schema file → internal | `SchemaIngestion` |
| `src/data_ingestion.rs` | ~280 | JSONL import pipeline | `DataIngestion`, `JsonEntry`, `IngestionStats` |
| `src/embedding_queue.rs` | ~650 | Async background queue | `EmbeddingQueue`, `EmbeddingQueueBuilder` |
| `examples/cli_demo.rs` | — | Only runnable entry point | — |

**Total: ~4,800 lines of Rust across 11 source files.**

## Core Data Model

### KnowledgeGraph (lib.rs)

The public facade. Composes three subsystems via `Arc`:

```rust
pub struct KnowledgeGraph {
    storage: Arc<KnowledgeGraphStorage>,
    embedding_manager: Arc<EmbeddingManager>,
    schema_manager: Arc<SchemaManager>,
}
```

**Important:** `VectorSearchEngine` is NOT a member of `KnowledgeGraph`. It is constructed separately in the CLI demo and its lifecycle is entirely manual. This is a known architectural gap that the migration addresses.

API surface: CRUD for objects, edges (connect_objects), text chunks, find_by_name, query_subgraph (BFS), schema validation (async methods), get_stats.

`ObjectBuilder` provides a fluent API: `ObjectBuilder::character("Name")`, `.with_description()`, `.with_property()`, `.build()`, `.add_to_graph(&kg)`.

### Storage (storage.rs — RocksDB)

Five column families:

| CF | Key | Value | Serialization |
|---|---|---|---|
| `CF_NODES` | UUID bytes | `ObjectMetadata` | bincode |
| `CF_EDGES` | UUID bytes | `AdjacencyList { outgoing: Vec<Edge>, incoming: Vec<Edge> }` | bincode |
| `CF_CHUNKS` | UUID bytes | `TextChunk` | bincode |
| `CF_NAMES` | `"{type}:{name}"` bytes | UUID bytes | raw |
| `CF_SCHEMAS` | name bytes | `SchemaDefinition` | serde_json |

**Critical design note:** Each edge exists TWICE in storage (once in source's outgoing list, once in target's incoming list). Edge upsert reads both adjacency lists, mutates, writes back. This is the root cause of the O(N) node deletion bug.

### Domain Types (types.rs)

- `ObjectMetadata`: string `object_type` + `serde_json::Value` properties. Dynamic schema, no compile-time type enforcement.
- `EdgeType`: primarily `Custom(String)`. Four legacy variants (`Contains`, `OwnedBy`, `LocatedIn`, `MemberOf`) are `#[deprecated]` — exist for backward compat only.
- `TextChunk`: content + estimated token count (`text.len() / 4`). Types: Description, SessionNote, AiGenerated, UserNote, Imported.
- `QueryResult`: aggregates objects + edges + chunks with a token budget system.

### Embeddings (embeddings.rs)

`EmbeddingProvider` trait (async_trait, Send + Sync):
- `embed(&str) -> Vec<f32>`
- `embed_batch(Vec<String>) -> Vec<Vec<f32>>`
- `dimensions() -> usize`
- `max_tokens() -> usize`
- `provider_type()`, `model_info()`

Only implementation: `FastEmbedProvider` wrapping `Mutex<TextEmbedding>`. Default model: NomicEmbedTextV15 (768 dimensions). `EmbeddingManager` holds a single `Arc<dyn EmbeddingProvider>`.

**This trait is the primary integration point for Lemonade Server.** A `LemonadeProvider` implementing `EmbeddingProvider` via HTTP replaces `FastEmbedProvider` cleanly.

### Vector Search (vector_search.rs)

`VectorSearchEngine` owns:
1. `Arc<RwLock<HnswVectorStore>>` — hnsw_rs `Hnsw<'static, f32, DistL2>` for ANN search
2. `RwLock<Option<Map<Vec<u8>>>>` — in-memory FST for prefix/exact name matching
3. `Arc<RwLock<HashMap<ChunkId, String>>>` — chunk preview strings

Hybrid search runs semantic (HNSW) and exact (FST) in sequence, returns `HybridSearchResult` with both lists unmerged.

### Schema System (schema.rs / schema_manager.rs / schema_ingestion.rs)

- `SchemaDefinition` → named maps of `ObjectTypeSchema` and `EdgeTypeSchema`
- `SchemaManager` caches in `DashMap`, validates properties (type, regex, enums, min/max), persists to CF_SCHEMAS
- `SchemaIngestion` reads `defaults/schemas/*.schema.json` files, strips `add_` prefix from names (MCP naming convention), adds 24 common TTRPG edge types automatically
- `inheritance` field exists in `ObjectTypeSchema` but is never acted on

### Data Ingestion (data_ingestion.rs)

Two-pass JSONL import: collect all nodes → create objects with name→ID map → resolve edge names → create edges. Metadata strings `"key:value"` become properties; plain strings become tags.

### Embedding Queue (embedding_queue.rs)

Well-implemented async background queue with tokio::mpsc, DashMap status tracking, broadcast progress channel. Builder pattern. **Not integrated into KnowledgeGraph or VectorSearchEngine** — exists as isolated utility.

## Known Bugs (Priority Order)

### BUG-1: HNSW persistence is non-functional (HIGH)
`HnswVectorStore::try_load_from_file` always returns `None` (see TODO at vector_search.rs L113). Every startup rebuilds the vector index from scratch by iterating all RocksDB objects.
**Fix:** Replace HNSW with sqlite-vec (migration Phase 2).

### BUG-2: similarity = 1.0 - distance is mathematically wrong (HIGH)
At vector_search.rs L367: `similarity: 1.0 - distance` converts L2 distance to "similarity" but L2 is unbounded — produces negative values for distant vectors.
**Quick fix:** `similarity: 1.0 / (1.0 + distance)` bounds output to (0, 1].
**Proper fix:** Use cosine similarity (migration Phase 2).

### BUG-3: Node deletion is O(N) (HIGH — blocks UI)
`delete_node` in storage.rs scans the entire CF_EDGES column family to remove dangling references from every node's adjacency list.
**Fix:** SQLite migration with indexed `source_id`/`target_id` columns makes this O(log N).

### BUG-4: get_stats() is O(N) (MEDIUM — blocks UI dashboard)
Counts nodes/edges/chunks by iterating entire column families.
**Quick fix:** Add `AtomicUsize` counters, increment on insert, decrement on delete. Initialize once on startup.
**Proper fix:** SQLite `SELECT COUNT(*)` with covering indexes.

### BUG-5: Mutex<TextEmbedding> in async context (MEDIUM)
`FastEmbedProvider::embed()` locks a `std::sync::Mutex` for CPU-bound embedding work inside an async function, blocking the Tokio thread.
**Fix:** Eliminated entirely by switching to Lemonade HTTP provider (fully async).

### BUG-6: No deduplication on import (MEDIUM)
Each import generates new UUIDs. Re-importing the same JSONL creates duplicate objects.
**Fix:** Check `find_by_name` before insert in `DataIngestion::create_objects`.

### BUG-7: In-memory-only edge resolution during import (MEDIUM)
`DataIngestion` builds name→ID map only for the current session. Cannot resolve references to existing nodes from prior imports.
**Fix:** Query storage name index first, fall back to session map.

### BUG-8: get_chunks_for_node is O(N) (LOW)
Scans entire CF_CHUNKS filtering by object_id. Not indexed.
**Fix:** SQLite migration with indexed `object_id` column.

### BUG-9: 'static lifetime on Hnsw (LOW — soundness concern)
`HnswVectorStore` has `hnsw: Hnsw<'static, f32, DistL2>`. The `'static` on a mutable struct member is a lifetime workaround.
**Fix:** Eliminated by removing hnsw_rs.

## Design Decisions — What's Good

- **`EmbeddingProvider` trait** — clean, extensible, perfect integration point for Lemonade Server
- **Schema system** — flexible JSON schemas with validation, regex, enums. External `.schema.json` files allow evolution without code changes
- **`ObjectBuilder` fluent API** — ergonomic for constructing `ObjectMetadata`
- **`EdgeType::Custom(String)`** — correct move over rigid enums
- **`EmbeddingQueue`** — well-architected async queue (needs integration)
- **JSONL + schema.json format** — sensible for local-first tool
- **Tests everywhere** — unit tests in every module, all using temp directories for isolation

## Design Decisions — Questionable

- **RocksDB adjacency list pattern** — storing edges as mutable `Vec<Edge>` per node is expensive. Standard: sorted edge records with composite keys enabling range scans.
- **Schema naming `add_npc` vs `npc`** — `.schema.json` files named after MCP tool actions. SchemaIngestion strips the `add_` prefix, but the files themselves leak an external convention.
- **Async schema validation with no async work** — `SchemaManager` methods are `async` but contain no `.await` points.
- **`petgraph` unused** — pulled in "for future use," adds compile time.
- **`ort-sys` pinned to pre-release** — `=2.0.0-rc.10` is fragile. Eliminated by Lemonade migration.

## Dependencies

| Crate | Version | Role | Migration Status |
|---|---|---|---|
| `rocksdb` | 0.23 | Storage | → SQLite (Phase 2) |
| `fastembed` | 5.0 | Embeddings | → Lemonade HTTP (Phase 1) |
| `hnsw_rs` | 0.3 | Vector ANN | → sqlite-vec (Phase 2) |
| `fst` | 0.4 | Name matching | → SQLite FTS5 (Phase 2) |
| `ort` / `ort-sys` | =2.0.0-rc.10 | ONNX Runtime | → Remove (Phase 1) |
| `petgraph` | 0.8 | Unused | → Remove (Phase 1) |
| `tokio` | 1.45 | Async runtime | Keep |
| `serde` / `serde_json` | 1.0 | Serialization | Keep |
| `bincode` | 1.3 | Binary serialization | Keep (may simplify with SQLite) |
| `reqwest` | 0.12 | HTTP client | → Required (Phase 1, currently optional) |
| `dashmap` | 6.1 | Concurrent maps | Keep |
| `parking_lot` | 0.12 | Fast locks | Keep |
| `memmap2` | 0.9 | Memory-mapped files | → Remove (Phase 2) |
| `rayon` | 1.8 | Parallelism | Keep (minimal current use) |

## Sample Data

`defaults/data/memory.json`: ~220 nodes + ~312 edges modeling Isaac Asimov's Foundation universe. JSONL format. Used by CLI demo for end-to-end testing.

`defaults/schemas/`: 13 `.schema.json` files — npc, player_character, location, faction, quest, artifact, currency, inventory, skills, temporal, setting_reference, system_reference, transportation.

## Build Requirements

**Current:** GCC 13+ (for RocksDB C++ compilation). Must `source env.sh` first.
**After Phase 2:** Just Rust stable. No system C++ compiler needed.