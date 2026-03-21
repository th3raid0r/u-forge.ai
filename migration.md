# Migration Plan: Lemonade Server + SQLite + Web UI

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
| `POST /api/v1/embeddings` | Text → vector embeddings |
| `POST /api/v1/reranking` | Query + docs → relevance scores |
| `POST /api/v1/chat/completions` | LLM generation (future) |
| `GET /api/v1/models` | Available model listing |
| `GET /api/v1/health` | Server health check |
| `POST /api/v1/pull` | Download new models |

API key is required but ignored — use any string (e.g., `"lemonade"`).

### Models

| Capability | Recommended Model | Endpoint |
|---|---|---|
| Embeddings | `nomic-embed-text` | `/api/v1/embeddings` |
| Reranking | `bge-reranker-v2-m3` | `/api/v1/reranking` |
| Chat/Generation | `Llama-3.2-1B-Instruct` or larger | `/api/v1/chat/completions` |

Pull models via CLI or API:
```bash
lemonade-server pull nomic-embed-text
lemonade-server pull bge-reranker-v2-m3
```

---

## Phase 1: Lemonade Embedding Provider (Low Risk, High Value)

**Goal:** Add `LemonadeProvider` as a new `EmbeddingProvider` implementation using HTTP.
**Files changed:** `src/embeddings.rs`, `Cargo.toml`
**Files unchanged:** Everything else — this is purely additive.

### 1a. Make reqwest a required dependency

```toml
# Cargo.toml — change reqwest from optional to required
reqwest = { version = "0.12", features = ["json"] }
```

### 1b. Implement LemonadeProvider

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

### 1c. Add factory methods to EmbeddingManager

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

### 1d. Update KnowledgeGraph::new

Make construction async to support Lemonade probing:

```rust
impl KnowledgeGraph {
    pub async fn new(db_path: &Path, config: &AppConfig) -> Result<Self> {
        let storage = Arc::new(KnowledgeGraphStorage::new(db_path)?);
        let embedding_manager = Arc::new(
            EmbeddingManager::try_new_auto(
                config.lemonade_url.as_deref(),
                config.embedding_model.as_deref(),
                config.cache_dir.clone(),
            ).await?
        );
        let schema_manager = Arc::new(SchemaManager::new(storage.clone()));
        Ok(Self { storage, embedding_manager, schema_manager })
    }
}
```

### 1e. Remove heavy crates (after Phase 1 is working)

```toml
# Remove from Cargo.toml:
# fastembed = { version = "5.0" }      # Only if NOT keeping as fallback
# ort-sys = ...                         # Only needed by fastembed
# ort = ...                             # Only needed by fastembed
petgraph = "0.8"                        # Unused, remove immediately
```

**Decision point:** Keep FastEmbed behind a `local-fallback` feature flag, or remove entirely. If Lemonade Server is always expected to be running, remove it. If offline support matters, keep it gated.

### Phase 1 Bug Fixes (do alongside)

- **BUG-2 fix:** Change `similarity: 1.0 - distance` to `similarity: 1.0 / (1.0 + distance)` in vector_search.rs L367
- **BUG-6 fix:** Add dedup check in `DataIngestion::create_objects` — call `find_by_name` before insert
- **BUG-7 fix:** Query storage name index before session map in edge resolution

---

## Phase 2: SQLite Migration (Medium Risk, High Value)

**Goal:** Replace RocksDB + HNSW + FST with SQLite + sqlite-vec + FTS5.
**This is the biggest single change.** No production data exists, so this is a clean cut.

### 2a. New dependencies

```toml
# Add to Cargo.toml:
rusqlite = { version = "0.32", features = ["bundled", "vtab"] }
# sqlite-vec loaded as extension at runtime

# Remove from Cargo.toml:
rocksdb = "0.23"
hnsw_rs = "0.3"
fst = "0.4"
memmap2 = "0.9"
```

### 2b. SQLite schema

```sql
-- Core tables
CREATE TABLE nodes (
    id TEXT PRIMARY KEY,           -- UUID as text
    object_type TEXT NOT NULL,
    schema_name TEXT,
    name TEXT NOT NULL,
    description TEXT,
    tags TEXT,                     -- JSON array
    properties TEXT NOT NULL,      -- JSON object
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE edges (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    target_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    edge_type TEXT NOT NULL,
    weight REAL NOT NULL DEFAULT 1.0,
    metadata TEXT,                 -- JSON object
    created_at TEXT NOT NULL,
    UNIQUE(source_id, target_id, edge_type)
);

CREATE TABLE chunks (
    id TEXT PRIMARY KEY,
    object_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    chunk_type TEXT NOT NULL,
    content TEXT NOT NULL,
    token_count INTEGER NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE schemas (
    name TEXT PRIMARY KEY,
    definition TEXT NOT NULL       -- JSON
);

-- Full-text search
CREATE VIRTUAL TABLE chunks_fts USING fts5(
    content,
    content_rowid='rowid'
);

-- Vector search (via sqlite-vec extension)
CREATE VIRTUAL TABLE chunk_vectors USING vec0(
    chunk_id TEXT PRIMARY KEY,
    embedding float[768]
);

-- Indexes
CREATE INDEX idx_nodes_type ON nodes(object_type);
CREATE INDEX idx_nodes_name ON nodes(object_type, name);
CREATE INDEX idx_edges_source ON edges(source_id);
CREATE INDEX idx_edges_target ON edges(target_id);
CREATE INDEX idx_edges_type ON edges(edge_type);
CREATE INDEX idx_chunks_object ON chunks(object_id);
```

### 2c. New StorageBackend trait

```rust
/// Abstract storage interface — allows RocksDB and SQLite to coexist during migration
pub trait StorageBackend: Send + Sync {
    fn upsert_node(&self, metadata: &ObjectMetadata) -> Result<ObjectId>;
    fn get_node(&self, id: ObjectId) -> Result<Option<ObjectMetadata>>;
    fn get_all_objects(&self) -> Result<Vec<ObjectMetadata>>;
    fn find_nodes_by_name(&self, object_type: &str, name: &str) -> Result<Option<ObjectMetadata>>;
    fn upsert_edge(&self, edge: &Edge) -> Result<()>;
    fn get_edges(&self, node_id: ObjectId) -> Result<Vec<Edge>>;
    fn delete_node(&self, id: ObjectId) -> Result<()>;
    fn get_stats(&self) -> Result<GraphStats>;
    // ... etc
}
```

Implement for both `RocksDbStorage` (existing, wrapped) and `SqliteStorage` (new). Wire `KnowledgeGraph` to use `Arc<dyn StorageBackend>`. Test both in parallel. Remove RocksDB when SQLite is proven.

### 2d. Absorb VectorSearchEngine into KnowledgeGraph

```rust
pub struct KnowledgeGraph {
    storage: Arc<dyn StorageBackend>,
    embedding_manager: Arc<EmbeddingManager>,
    schema_manager: Arc<SchemaManager>,
    // NEW: vector search is now owned, not floating separately
    // With SQLite, this becomes queries against chunk_vectors table
}
```

### Phase 2 Bug Fixes (resolved by design)

- **BUG-1:** HNSW persistence → eliminated (sqlite-vec persists automatically)
- **BUG-3:** O(N) node deletion → `ON DELETE CASCADE` + indexed foreign keys
- **BUG-4:** O(N) get_stats → `SELECT COUNT(*)` with covering indexes
- **BUG-8:** O(N) get_chunks_for_node → indexed `object_id` column
- **BUG-9:** 'static lifetime on Hnsw → eliminated (no more hnsw_rs)

---

## Phase 3: HTTP Server for UI (Medium Risk, Medium Value)

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

## Phase 4: Enhanced Search (Low Risk, Medium Value)

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

## Phase 5: Cargo Workspace (Low Risk, Organizational)

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

| Step | Phase | Description | Risk | Crate Changes |
|---|---|---|---|---|
| 1 | 1a | Add reqwest as required dep | None | +reqwest |
| 2 | 1b-c | Implement LemonadeProvider + factory methods | Low | Edit embeddings.rs |
| 3 | 1 | Fix BUG-2 (similarity score) | None | 1-line fix |
| 4 | 1 | Fix BUG-6 (import dedup) | Low | Edit data_ingestion.rs |
| 5 | 1 | Fix BUG-7 (edge resolution) | Low | Edit data_ingestion.rs |
| 6 | 1e | Remove petgraph | None | -petgraph |
| 7 | 2a-b | Add SQLite storage implementation | Medium | +rusqlite, new file |
| 8 | 2c | Create StorageBackend trait, wrap both impls | Medium | Refactor storage.rs |
| 9 | 2d | Absorb VectorSearchEngine into KnowledgeGraph | Medium | Refactor lib.rs |
| 10 | 2 | Remove RocksDB, HNSW, FST, memmap2, ort | Medium | -5 crates |
| 11 | 1e | Remove fastembed + ort (or gate behind feature) | Low | -3 crates |
| 12 | 3 | Add axum server with REST API | Medium | +axum, new file |
| 13 | 3 | Add WebSocket support | Medium | New file |
| 14 | 4 | Add LemonadeReranker | Low | New file |
| 15 | 4 | Enhanced search pipeline | Medium | Edit search logic |
| 16 | 5 | Cargo workspace split | Low | Restructure |

## What We Keep

- **`EmbeddingProvider` trait** — perfect abstraction, Lemonade slots in cleanly
- **Schema system** — all of schema.rs, schema_manager.rs, schema_ingestion.rs
- **Domain types** — ObjectMetadata, Edge, EdgeType, TextChunk, QueryResult
- **ObjectBuilder** — fluent API unchanged
- **Data ingestion** — JSONL format and pipeline (with bug fixes)
- **Embedding queue** — well-architected, integrate into KnowledgeGraph
- **All 13 schema.json files** — unchanged
- **Foundation sample data** — unchanged

## What We Remove

- `fastembed` + `ort` + `ort-sys` (replaced by Lemonade HTTP)
- `hnsw_rs` (replaced by sqlite-vec)
- `fst` + `memmap2` (replaced by SQLite FTS5)
- `rocksdb` (replaced by SQLite)
- `petgraph` (unused)
- `env.sh` gcc-13 requirement (no more C++ compilation)
- `HnswVectorStore` struct and all HNSW persistence code
- `FastEmbedProvider` (or gate behind optional feature flag)