# Migration Plan: Lemonade Server + SQLite + Web UI

> **Status as of latest session:**
> - ✅ **Phase 1 COMPLETE** — `LemonadeProvider` implemented; FastEmbed/ORT/ort-sys removed; BUG-2, BUG-6, BUG-7 fixed.
> - ✅ **Phase 2 COMPLETE** — SQLite (`rusqlite` bundled) replaces RocksDB; FTS5 full-text search live; BUG-1, BUG-3, BUG-4, BUG-5, BUG-8, BUG-9 resolved. **sqlite-vec (vector ANN) is deferred** — FTS5 keyword search is the current semantic-search substitute until Phase 2 is fully closed out.
> - ✅ **Extended Lemonade Integration COMPLETE** — `src/lemonade.rs` added: `LemonadeModelRegistry` (model discovery + role classification), `GpuResourceManager` (STT/LLM GPU sharing policy with RAII guards), `LemonadeTtsProvider` (kokoro-v1, CPU), `LemonadeSttProvider` (Whisper-Large-v3-Turbo, GPU), `LemonadeChatProvider` (GLM-4.7-Flash-GGUF, GPU), `LemonadeStack` convenience builder. Default embedding model changed to `embed-gemma-300m-FLM` (NPU). `embed_batch` fallback added for FLM backends. 92/92 tests passing.
> - ⏳ **Phase 3–5** — not yet started.

> Replaces the previous plan (self-managed llama.cpp fleet). This plan standardizes on
> [Lemonade Server](https://github.com/lemonade-sdk/lemonade) for all AI capabilities
> and SQLite for unified storage.

## Goals

1. **Replace in-process AI** (FastEmbed, HNSW, ONNX Runtime) with Lemonade Server HTTP API
2. **Replace RocksDB** with SQLite + FTS5 + sqlite-vec — eliminates gcc-13 requirement
3. **Prepare for web UI** — HTTP/WebSocket server, unified AppState
4. **Fix critical bugs** — see ARCHITECTURE.md §Known Bugs

## Prerequisites

### Lemonade Server

Install and run [Lemonade Server](https://github.com/lemonade-sdk/lemonade):

```bash
# Linux (Snap)
sudo snap install lemonade-server

# macOS (beta)
# Download .pkg from https://github.com/lemonade-sdk/lemonade/releases

# Windows
# Download .exe installer from releases page
```

Lemonade provides an **OpenAI-compatible REST API** at `http://localhost:8000/api/v1`:

| Endpoint | Use in u-forge.ai |
|---|---|
| `POST /api/v1/embeddings` | Text → vector embeddings (`LemonadeProvider`) |
| `POST /api/v1/audio/speech` | Text-to-speech synthesis (`LemonadeTtsProvider`) |
| `POST /api/v1/audio/transcriptions` | Speech-to-text transcription (`LemonadeSttProvider`) |
| `POST /api/v1/chat/completions` | LLM generation (`LemonadeChatProvider`) |
| `POST /api/v1/reranking` | Query + docs → relevance scores (Phase 4) |
| `GET /api/v1/models` | Available model listing (`LemonadeModelRegistry`) |
| `GET /api/v1/health` | Server health check |
| `POST /api/v1/pull` | Download new models |

API key is required but ignored — use any string (e.g., `"lemonade"`).

### Models

The following hardware-aware model assignment is used. The `LemonadeModelRegistry`
discovers and classifies all of these automatically via `GET /api/v1/models`.

| Capability | Model | Hardware | Recipe | Endpoint |
|---|---|---|---|---|
| Embeddings | `embed-gemma-300m-FLM` | **NPU** | `flm` | `/api/v1/embeddings` |
| Text-to-speech | `kokoro-v1` | **CPU** | `kokoro` | `/api/v1/audio/speech` |
| Speech-to-text | `Whisper-Large-v3-Turbo` | **GPU** | `whispercpp` | `/api/v1/audio/transcriptions` |
| Chat / LLM | `GLM-4.7-Flash-GGUF` | **GPU** | `llamacpp` | `/api/v1/chat/completions` |
| Reranking (Phase 4) | `bge-reranker-v2-m3-GGUF` | CPU | `llamacpp` | `/api/v1/reranking` |

> **GPU sharing policy:** `Whisper-Large-v3-Turbo` and `GLM-4.7-Flash-GGUF` share
> the same GPU and run one at a time. `GpuResourceManager` enforces this:
> STT invoked during LLM inference is **rejected immediately** (latency-sensitive);
> LLM invoked during STT is **queued** until the STT session completes.

Pull models via CLI or API:
```bash
lemonade-server pull embed-gemma-300m-FLM
lemonade-server pull kokoro-v1
lemonade-server pull Whisper-Large-v3-Turbo
lemonade-server pull GLM-4.7-Flash-GGUF
lemonade-server pull bge-reranker-v2-m3-GGUF   # Phase 4
```

---

## Phase 1: Lemonade Embedding Provider ✅ COMPLETE

**Goal:** Add `LemonadeProvider` as a new `EmbeddingProvider` implementation using HTTP.
**Status:** Complete. `FastEmbedProvider`, `ort`, `ort-sys`, and `fastembed` crates have been removed entirely. `LemonadeProvider` is the only `EmbeddingProvider` implementation.

### 1a. Make reqwest a required dependency ✅

```toml
# Cargo.toml — change reqwest from optional to required
reqwest = { version = "0.12", features = ["json"] }
```

### 1b. Implement LemonadeProvider ✅

Add to `src/embeddings.rs`:

```rust
/// Embedding provider using Lemonade Server's OpenAI-compatible API.
pub struct LemonadeProvider {
    client: reqwest::Client,
    base_url: String,   // e.g. "http://localhost:8000/api/v1"
    model: String,      // e.g. "nomic-embed-text"
    dimensions: usize,  // probed on construction
}

impl LemonadeProvider {
    pub async fn new(base_url: &str, model: &str) -> Result<Self> {
        let client = reqwest::Client::new();
        // Probe dimensions by embedding a single token
        let resp = client.post(format!("{}/embeddings", base_url))
            .header("Authorization", "Bearer lemonade")
            .json(&serde_json::json!({
                "model": model,
                "input": ["dimension probe"]
            }))
            .send().await?
            .json::<serde_json::Value>().await?;
        let dimensions = resp["data"][0]["embedding"]
            .as_array()
            .map(|a| a.len())
            .ok_or_else(|| anyhow::anyhow!("Failed to probe embedding dimensions"))?;
        Ok(Self { client, base_url: base_url.to_string(), model: model.to_string(), dimensions })
    }
}

#[async_trait]
impl EmbeddingProvider for LemonadeProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> { /* POST /embeddings */ }
    async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> { /* POST /embeddings with array input */ }
    fn dimensions(&self) -> Result<usize> { Ok(self.dimensions) }
    fn max_tokens(&self) -> Result<usize> { Ok(8192) } // Lemonade models typically support 8K
    fn provider_type(&self) -> EmbeddingProviderType { /* new variant */ }
    fn model_info(&self) -> Option<ModelInfo<EmbeddingModel>> { None }
}
```

### 1c. Add factory methods to EmbeddingManager ✅

```rust
impl EmbeddingManager {
    /// Connect to Lemonade Server for embeddings
    pub async fn try_new_lemonade(base_url: &str, model: &str) -> Result<Self> { ... }

    /// Try Lemonade first, fall back to local FastEmbed
    pub async fn try_new_auto(lemonade_url: Option<&str>, model: Option<&str>, cache_dir: Option<PathBuf>) -> Result<Self> {
        if let Some(url) = lemonade_url {
            if let Ok(mgr) = Self::try_new_lemonade(url, model.unwrap_or("nomic-embed-text")).await {
                tracing::info!("Connected to Lemonade Server at {}", url);
                return Ok(mgr);
            }
            tracing::warn!("Lemonade Server not available at {}, falling back to local", url);
        }
        Self::try_new_local_default(cache_dir)
    }
}
```

### 1d. Update KnowledgeGraph::new ✅

`KnowledgeGraph::new` is synchronous and takes only `db_path`. The `embedding_manager` field has been removed from the struct — embeddings are opt-in via `EmbeddingManager` separately. `EmbeddingManager::try_new_auto(url, model)` reads `LEMONADE_URL` from the environment.

### 1e. Remove heavy crates ✅

Removed: `fastembed`, `ort`, `ort-sys`, `petgraph`, `hnsw_rs`, `fst`, `memmap2`, `bincode`, `rayon`.
Decision: FastEmbed removed entirely. Lemonade Server is required for embeddings. No local fallback.

### Phase 1 Bug Fixes ✅

- **BUG-2:** Fixed `similarity: 1.0 / (1.0 + distance)` — now moot since HNSW removed in Phase 2.
- **BUG-6:** `DataIngestion::create_objects` calls `find_by_name` before insert; duplicates skipped with a warn log.
- **BUG-7:** New `resolve_node_id` helper checks session map first, then falls back to `storage.find_nodes_by_name_only` (O(log N) via `idx_nodes_name_only` index).

---

## Phase 2: SQLite Migration ✅ COMPLETE (sqlite-vec deferred)

**Goal:** Replace RocksDB + HNSW + FST with SQLite + sqlite-vec + FTS5.
**Status:** Complete for storage and FTS5. `sqlite-vec` (vector ANN search) is deferred — FTS5 keyword search serves as the interim text-matching solution. No production data existed, so this was a clean cut.

### 2a. New dependencies ✅

```toml
# Added:
rusqlite = { version = "0.32", features = ["bundled", "vtab"] }
# sqlite-vec: deferred (no crate added yet)

# Removed:
# rocksdb, hnsw_rs, fst, memmap2, bincode, ort, ort-sys, fastembed, petgraph, rayon
```

### 2b. SQLite schema ✅

Implemented in `src/storage.rs` as `SQL_SCHEMA` const. Actual schema:

```sql
CREATE TABLE IF NOT EXISTS nodes (
    id          TEXT PRIMARY KEY,
    object_type TEXT NOT NULL,
    schema_name TEXT,
    name        TEXT NOT NULL,
    description TEXT,
    tags        TEXT NOT NULL DEFAULT '[]',   -- JSON array
    properties  TEXT NOT NULL DEFAULT '{}',  -- JSON object
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS edges (
    source_id  TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    target_id  TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    edge_type  TEXT NOT NULL,
    weight     REAL NOT NULL DEFAULT 1.0,
    metadata   TEXT NOT NULL DEFAULT '{}',   -- JSON object
    created_at TEXT NOT NULL,
    UNIQUE(source_id, target_id, edge_type)
);

CREATE TABLE IF NOT EXISTS chunks (
    id          TEXT PRIMARY KEY,
    object_id   TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    chunk_type  TEXT NOT NULL,
    content     TEXT NOT NULL,
    token_count INTEGER NOT NULL,
    created_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS schemas (
    name       TEXT PRIMARY KEY,
    definition TEXT NOT NULL  -- JSON
);

-- Full-text search (active)
CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
    content,
    content='chunks',
    content_rowid='rowid'
);
-- Three DML triggers keep chunks_fts in sync with chunks automatically.

-- Indexes
CREATE INDEX IF NOT EXISTS idx_nodes_type      ON nodes(object_type);
CREATE INDEX IF NOT EXISTS idx_nodes_name      ON nodes(object_type, name);
CREATE INDEX IF NOT EXISTS idx_nodes_name_only ON nodes(name);  -- for find_by_name_only
CREATE INDEX IF NOT EXISTS idx_edges_source    ON edges(source_id);
CREATE INDEX IF NOT EXISTS idx_edges_target    ON edges(target_id);
CREATE INDEX IF NOT EXISTS idx_chunks_object   ON chunks(object_id);

-- NOTE: chunk_vectors (sqlite-vec) not yet created — deferred.
```

### 2c. StorageBackend trait — deferred

No trait was needed — this was a clean cut (no production data). `KnowledgeGraphStorage` is now the SQLite implementation directly. RocksDB was removed in the same commit. The `StorageBackend` trait can be introduced in Phase 5 (workspace split) if multiple backends are needed again.

### 2d. Absorb VectorSearchEngine into KnowledgeGraph ✅ (partial)

`VectorSearchEngine`, `HnswVectorStore`, and all HNSW/FST code have been deleted. `src/vector_search.rs` no longer exists.

```rust
// Current KnowledgeGraph struct (src/lib.rs):
pub struct KnowledgeGraph {
    storage: Arc<KnowledgeGraphStorage>,
    schema_manager: Arc<SchemaManager>,
    // embedding_manager removed — opt-in via EmbeddingManager separately
}
```

FTS5 search is exposed via `KnowledgeGraph::search_chunks_fts(query, limit)`.
Vector ANN search via `sqlite-vec` is the remaining deferred item.

### Phase 2 Bug Fixes ✅ (all resolved)

- **BUG-1:** HNSW persistence broken → eliminated (HNSW removed entirely)
- **BUG-3:** O(N) node deletion → `ON DELETE CASCADE` + indexed FK columns
- **BUG-4:** O(N) get_stats → `SELECT COUNT(*)` + `SUM(token_count)` — O(1)
- **BUG-8:** O(N) get_chunks_for_node → `idx_chunks_object` index on `chunks(object_id)`
- **BUG-9:** `'static` lifetime on Hnsw → eliminated (hnsw_rs removed)

---

## Phase 3: HTTP Server for UI ⏳ NOT STARTED

**Goal:** Add an axum HTTP/WebSocket server so a web UI can connect.

### 3a. New dependencies

```toml
axum = "0.8"
tower-http = { version = "0.6", features = ["cors", "fs"] }
```

### 3b. AppState

```rust
pub struct AppState {
    pub graph: Arc<KnowledgeGraph>,
    pub config: Arc<AppConfig>,
}
```

### 3c. REST API routes

```
GET    /api/objects              → list all objects (paginated)
GET    /api/objects/:id          → get single object
POST   /api/objects              → create object (validated)
PUT    /api/objects/:id          → update object
DELETE /api/objects/:id          → delete object
GET    /api/objects/:id/edges    → get edges for object
GET    /api/objects/:id/subgraph → BFS subgraph query
POST   /api/search               → hybrid search (semantic + FTS + exact)
GET    /api/schemas              → list schemas
GET    /api/schemas/:name        → get schema details
POST   /api/ingest               → JSONL import
GET    /api/stats                → graph statistics
GET    /api/health               → health check (includes Lemonade status)
```

### 3d. WebSocket for real-time

```
WS /ws
→ Push notifications for: object created/updated/deleted, search results streaming, import progress
```

### 3e. Configuration

```toml
# config.toml
[server]
host = "127.0.0.1"
port = 3000

[lemonade]
url = "http://localhost:8000/api/v1"
embedding_model = "nomic-embed-text"
rerank_model = "bge-reranker-v2-m3"

[database]
path = "data/knowledge.db"

[search]
default_semantic_limit = 10
default_exact_limit = 5
rerank_enabled = true
```

---

## Phase 4: Enhanced Search ⏳ NOT STARTED

**Goal:** Add reranking and improve search quality using Lemonade Server capabilities.

### 4a. Reranking via Lemonade

```rust
pub struct LemonadeReranker {
    client: reqwest::Client,
    base_url: String,
    model: String,
}

impl LemonadeReranker {
    /// Rerank documents by relevance to query
    pub async fn rerank(&self, query: &str, documents: Vec<String>, top_n: usize) -> Result<Vec<RankedDocument>> {
        // POST /api/v1/reranking
        // Body: { "model": "...", "query": "...", "documents": [...], "top_n": N }
    }
}
```

### 4b. Enhanced search pipeline

1. **Query** → embed via Lemonade
2. **Parallel:** sqlite-vec ANN search + FTS5 lexical search
3. **Merge** results with configurable alpha weighting
4. **Rerank** top-K via Lemonade cross-encoder
5. **Return** final ranked results

---

## Phase 5: Cargo Workspace ⏳ NOT STARTED

**Goal:** Split single crate into workspace for separation of concerns.

```
u-forge.ai/
├── crates/
│   ├── core/           # Domain types, storage trait, schemas
│   ├── lemonade/       # Lemonade Server client (embeddings, reranking, chat)
│   ├── storage-sqlite/ # SQLite StorageBackend implementation
│   └── server/         # axum HTTP/WebSocket server
├── web-ui/             # Frontend (React or Svelte)
├── Cargo.toml          # Workspace root
└── config.toml
```

---

## Execution Sequence

| Step | Phase | Description | Risk | Status |
|---|---|---|---|---|
| 1 | 1a | Add reqwest as required dep | None | ✅ Done |
| 2 | 1b-c | Implement LemonadeProvider + factory methods | Low | ✅ Done |
| 3 | 1 | Fix BUG-2 (similarity score) | None | ✅ Done |
| 4 | 1 | Fix BUG-6 (import dedup) | Low | ✅ Done |
| 5 | 1 | Fix BUG-7 (edge resolution) | Low | ✅ Done |
| 6 | 1e | Remove petgraph + fastembed + ort + all C++ crates | None | ✅ Done |
| 7 | 2a-b | Add SQLite storage, overwrite storage.rs | Medium | ✅ Done |
| 8 | 2c | StorageBackend trait | Medium | ⏭️ Skipped (clean cut, no trait needed) |
| 9 | 2d | Remove VectorSearchEngine, expose FTS5 via KnowledgeGraph | Medium | ✅ Done |
| 10 | 2 | Add sqlite-vec for ANN vector search | Medium | ⏳ Deferred |
| 11 | Lemon | Add `LemonadeModelRegistry` + `ModelRole` classification | Low | ✅ Done |
| 12 | Lemon | Add `GpuResourceManager` with STT-block / LLM-queue policy | Low | ✅ Done |
| 13 | Lemon | Add `LemonadeTtsProvider` (kokoro-v1, CPU) | Low | ✅ Done |
| 14 | Lemon | Add `LemonadeSttProvider` (Whisper, GPU, uses GpuResourceManager) | Low | ✅ Done |
| 15 | Lemon | Add `LemonadeChatProvider` (GLM-4.7, GPU, uses GpuResourceManager) | Low | ✅ Done |
| 16 | Lemon | Add `LemonadeStack` one-call builder | None | ✅ Done |
| 17 | Lemon | Switch default embedding model to `embed-gemma-300m-FLM` (NPU) | None | ✅ Done |
| 18 | Lemon | Add `embed_batch` sequential fallback for FLM/NPU backends | Low | ✅ Done |
| 19 | 3 | Add axum server with REST API | Medium | ⏳ Not started |
| 20 | 3 | Add WebSocket support | Medium | ⏳ Not started |
| 21 | 4 | Add `LemonadeReranker` to `src/lemonade.rs` | Low | ⏳ Not started |
| 22 | 4 | Enhanced search pipeline (FTS5 + vec + rerank) | Medium | ⏳ Not started |
| 23 | 5 | Cargo workspace split | Low | ⏳ Not started |

## What Was Kept ✅

- **`EmbeddingProvider` trait** — clean abstraction; `LemonadeProvider` slots in via HTTP
- **Schema system** — `schema.rs`, `schema_manager.rs`, `schema_ingestion.rs` unchanged
- **Domain types** — `ObjectMetadata`, `Edge`, `EdgeType`, `TextChunk`, `QueryResult`
- **ObjectBuilder** — fluent API unchanged
- **Data ingestion** — JSONL format and two-pass pipeline (BUG-6 + BUG-7 fixed)
- **Embedding queue** — well-architected async queue; now uses `MockEmbeddingProvider` in tests
- **All 13 schema.json files** — unchanged
- **Foundation sample data** — unchanged

## What Was Removed ✅

- `fastembed`, `ort`, `ort-sys` — replaced by Lemonade HTTP
- `hnsw_rs` — HNSW deleted; sqlite-vec is the planned replacement (deferred)
- `fst`, `memmap2` — replaced by SQLite FTS5
- `rocksdb` — replaced by `rusqlite` (bundled SQLite)
- `petgraph` — was unused
- `bincode` — replaced by `serde_json` text columns in SQLite
- `rayon` — removed (minimal use, not needed)
- `gcc-13` / `source env.sh` requirement — no more C++ compilation needed
- `src/vector_search.rs` — deleted entirely
- `FastEmbedProvider`, `LocalEmbeddingModelType`, `FastEmbedModel` enums
- `embedding_cache_dir` param from `KnowledgeGraph::new`
- `get_embedding_provider()` from `KnowledgeGraph`