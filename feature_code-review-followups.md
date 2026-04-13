# Code Review Follow-ups

Actionable items from the comprehensive code review. These are maintenance and
quality-of-life improvements — none require architectural changes.

---

## Status Key

- ⏳ Not started
- 🔧 In progress
- ✅ Done

---

## P1 — Make `embed_all_chunks` Incremental

**Status:** ✅

`ingest/embedding.rs` — `embed_all_chunks()` gates on
`chunk_count > embedded_count` but then loads *all* chunks and re-embeds them.
For incremental updates (user adds one NPC to a 500-node graph), this is
wasteful.

**Action:**

1. Add a query to `graph/fts.rs` (or `graph/chunks.rs`) that returns only
   chunks without a corresponding entry in `chunks_vec` (or `chunks_vec_hq`):

   ```sql
   SELECT c.id, c.object_id, c.content, c.token_count, c.created_at, c.chunk_type
   FROM chunks c
   LEFT JOIN chunks_vec v ON c.rowid = v.rowid
   WHERE v.rowid IS NULL
   ```

2. Update `embed_all_chunks()` to use this query instead of
   `get_all_objects()` + `get_text_chunks()`.

3. Add a test: insert 10 chunks, embed them, add 2 more, call
   `embed_all_chunks` again, assert only 2 embeddings were generated.

---

## P1 — Demote Search Diagnostic Logs

**Status:** ✅

`search/mod.rs` — The hybrid search function has 7 multi-line diagnostic blocks
at `info!` level that dump full chunk/node state at every pipeline stage. These
are invaluable during development but will flood logs in production.

**Action:**

1. Change all 7 diagnostic `info!("{buf}")` calls to `debug!("{buf}")`.
2. Keep the single-line summary logs (`"Candidate pool: ..."`,
   `"Returning N node results"`) at `info!`.
3. Verify `RUST_LOG=u_forge_core::search=debug` still shows them.

---

## P1 — Switch Storage Mutex to `parking_lot`

**Status:** ✅

`graph/storage.rs` — `KnowledgeGraphStorage` wraps the SQLite `Connection` in
`Arc<std::sync::Mutex<Connection>>`. All 24 storage methods call
`.lock().unwrap()`. `parking_lot::Mutex` eliminates the `.unwrap()` (no
poisoning semantics) and is already a dependency (used by `GpuResourceManager`
and `WorkQueue`).

**Action:**

1. Change `conn: Arc<Mutex<Connection>>` to use `parking_lot::Mutex`.
2. Remove all 24 `.unwrap()` calls on `self.conn.lock()`.
3. Run `cargo test` — behaviour should be identical (parking_lot::Mutex is
   API-compatible for this use case).

**Bonus:** This also unblocks a future migration to `parking_lot::RwLock` for
concurrent readers, which matters once a UI or web server is reading the graph
while background indexing writes to it.

---

## P2 — Clean Up `ObjectType` Enum

**Status:** ⏳

`types.rs` defines `ObjectType` (Character, Location, Faction, Item, Event,
Session, CustomType), but `ObjectMetadata.object_type` is `String`.
`ObjectBuilder` writes strings. The schema system uses strings. The storage
layer uses strings. The enum is not enforced anywhere.

**Action (choose one):**

- **Option A — Delete it.** Remove `ObjectType` entirely. It's dead code.
  `ObjectBuilder` convenience constructors (`character()`, `location()`, etc.)
  already embed the correct string literals.

- **Option B — Wire it in.** Change `ObjectMetadata.object_type` to
  `ObjectType`, implement `Display`/`FromStr` for DB round-tripping, and
  validate at the `add_object` boundary. This is more work but prevents
  typo-based type drift (e.g. `"charcter"` vs `"character"`).

**Recommendation:** Option A for now. The schema system already validates types
against registered schemas — duplicating that validation in a Rust enum adds
maintenance burden without clear benefit.

---

## P2 — `#[allow(dead_code)]` Audit

**Status:** ⏳

Three `#[allow(dead_code)]` annotations exist in `queue/weighted.rs`:

- `WeightedWorkerSlot::name` — stored at construction, never read
- `WeightedWorkerSlot::idle` — stored at construction, never read
- `WeightedEmbedDispatcher::worker_count()` — `pub(super)`, unused

**Action:**

- If `name` and `idle` are reserved for future diagnostics/monitoring, add a
  comment explaining the intent. Otherwise remove them.
- `worker_count()` — either use it in `QueueStats` reporting or remove it.
