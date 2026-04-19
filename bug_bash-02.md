# bug_bash-02 — Post-Alpha polish roadmap

**Branch:** `topic_bug-bash-02`

Prescriptive punch list from a three-persona code review (Linus / Carmack /
Muratori) following the `bug-bash-01` merge. Tier 1 is the active work on
this branch; Tiers 2–5 are durable records of what else the review surfaced,
with enough detail that each item can be picked up cold on its own future
branch.

Verification legend on individual items:
- ✅ **Verified** — file/line refs spot-checked against the code.
- ⚠️ **Refined** — original claim was partly wrong; the stated fix is the
  corrected version.
- 🔎 **Plausible** — pattern in the code supports the claim but I did not
  independently confirm the exact line numbers; verify before coding.

Items the reviewers raised that are **deliberately excluded** from this
bash (to keep scope honest):

- **Undo `SelectionModel` / `AppState` splits.** Casey argued both are
  under-used abstractions, but bug-bash-01 Tiers 3 and 4.3 introduced them
  on purpose. Revisit only if they stay unused 3+ months from now.
- **Dual-queue `inference_queue` vs `hq_queue` coordination.** Linus flagged
  it but this is a design question, not a fix — belongs in a future feature
  file, not a bash.
- **Replace hand-rolled Lemonade HTTP client with `async-openai`.** Big
  surgery for a maintenance concern; schedule separately.
- **Chat `StoredRole` / `ChatMessageRole` consolidation.** bug-bash-01
  Tier 3 already reshaped this area — let it settle before another pass.

---

## Tier 1 — Crash-on-edge-case panics (**DONE** — fixed on this branch)

Five small, mutually independent fixes that replace `panic!` / `.unwrap()`
with graceful error paths or sentinels. All live inside existing call
sites — no new modules, no signature rework. The work fits in one PR.

Rule of thumb throughout: these are panics that real users will hit, not
hypothetical ones. Schema authors produce malformed JSON. Agent tool-calls
race with UI state. The snapshot rebuilds mid-drag. None of these should
take the whole app down.

### 1.1 ✅ Schema ingestion panics on malformed enum/array property types

**Where:** `crates/u-forge-core/src/schema/ingestion.rs:423, 494, 497`.

**Status: already safe in production.** The three `panic!()` calls at those
line numbers are inside `#[test]` functions (normal assertion pattern, not
production code). The production conversion path in
`convert_json_property_to_schema` already defaults unknown/malformed types
to `PropertyType::String` with no panic path. No code change needed.

### 1.2 ✅ Search panel unwraps missing inference queue

**Where:** `crates/u-forge-ui-gpui/src/search_panel.rs:143, 171`.

**Status: already fixed before this branch.** `do_search` already has an
early-return guard at lines 97–104 that sets `self.error` to
`"Lemonade not available — use FTS5"` and returns before the spawn. The
`queue.as_ref().unwrap()` calls inside the background task are only reachable
when the guard has already confirmed the queue is `Some`. No code change
needed on this branch.

### 1.3 ✅ Node editor edge unwraps on partial endpoints

**Where:**
- `crates/u-forge-ui-gpui/src/node_editor/mod.rs:482-483` — `e.from.unwrap(), e.to.unwrap()` in save path.
- `crates/u-forge-ui-gpui/src/node_editor/field_spec.rs:431` — same pattern inside an iterator map.

**Fixed on this branch.** The `.unwrap()` calls in `save_edges_for_tab` were
already guarded by `.filter(|e| e.is_complete())` (which checks both
endpoints are `Some`), so the panic was already prevented. This PR:

- Replaces the guarded `.unwrap()` with `let (Some(from), Some(to)) = ... else { unreachable!(...) }` to document the invariant.
- Counts incomplete edges in `save_edges_for_tab` (now returns `usize`).
- Bubbles the count through `save_dirty_tabs` (now returns a 4-tuple).
- Sets `AppState::data_status` in `do_save` when any edges were skipped, so
  the user sees "N incomplete edge(s) skipped — fill both endpoints before saving."

**Verification.** Manual: open a node, add a new edge, pick only the
`from` endpoint, click save. Expect a status-bar message naming the skipped
edge count instead of a panic or silent loss.

### 1.4 ✅ Graph canvas drag indexes into a snapshot that may have been rebuilt

**Where:** `crates/u-forge-ui-gpui/src/graph_canvas.rs:182-187`.

**Fixed on this branch (Option 1 — identity-based drag).**
`dragging_node` changed from `Option<usize>` to `Option<ObjectId>`. On
mousedown the node's `ObjectId` is captured from the snapshot. On
mousemove, `iter_mut().find(|n| n.id == drag_id)` locates the node in the
current snapshot regardless of rebuild; if the node has been removed, the
drag is ended silently. The direct `nodes[node_idx]` index access is gone.

### 1.5 ✅ Node editor pagination panics on oversized first field

**Where:** `crates/u-forge-ui-gpui/src/node_editor/render.rs:283, 295`
(and a related `.as_ref().unwrap()` at line 611 in the array-add path).

**Status: already fixed before this branch.** `current_page` is
initialized `vec![Vec::new()]` at line 278, so both `.last().unwrap()` and
`.last_mut().unwrap()` are always safe — there is always at least one
inner vec. The line-611 `array_add_field.as_ref().unwrap()` is guarded by
an `is_adding` check immediately above it (`.is_some_and(...)`), so it is
also unreachable with `None`. No code change needed on this branch.

---

## Tier 2 — Frame-perf wins (deferred)

Measure before coding each of these: enable the perf overlay
(`Ctrl+Shift+P`), watch `frame:<ms>` while reproducing the scenario
described. Skip any that don't show up in the trace.

### 2.1 ✅ Redundant `edge_type.as_str().to_string()` in snapshot rebuild

**Where:** `crates/u-forge-graph-view/src/snapshot.rs:185`.

`edge_type: e.edge_type.as_str().to_string()` allocates a fresh `String`
per edge every time `build_snapshot` runs. `EdgeType` is already a
newtype wrapper around `String`, so the `.as_str().to_string()` round-trip
is a pure re-clone. With 1k+ edges and frequent snapshot rebuilds
(agent-driven graph mutations, refresh cycles), this is thousands of
heap allocs per rebuild.

**Fix.** Option A: move the string out of the raw `Edge` into `EdgeView`
with `edge_type: e.edge_type.into_inner()` (add an `into_inner()` helper
on `EdgeType` if it doesn't exist). Option B: clone the inner `String`
directly — `edge_type: e.edge_type.0.clone()` — saving one allocation
per edge vs the current double-hop. If `EdgeView` lives as long as the
snapshot and raw edges are dropped, option A is strictly better.

**Verification.** Before/after: rebuild a snapshot with ~5k edges under
the perf overlay and record the snapshot-build timing. Expect a
measurable drop in allocator pressure.

### 2.2 ✅ Force-directed layout runs even when all positions are saved

**Where:** `crates/u-forge-graph-view/src/snapshot.rs` (`build_snapshot`).

**What was wrong.** `force_directed_layout` ran unconditionally on every
snapshot rebuild, then its results were immediately overwritten for every
node that had a saved position.  For an established graph (the common case)
every rebuild ran O(iterations × N) layout work that was thrown away.

**Fix applied.** Added an `all_saved` check before the layout call:

```rust
let all_saved = nodes.iter().all(|n| saved_positions.contains_key(&n.id));
if !all_saved {
    force_directed_layout(&mut nodes, &edges);
}
```

Layout still runs when at least one node is new (no saved position), so
newly added nodes get a valid initial position.  When all nodes are already
positioned, the layout pass is skipped entirely.

**Test added.** `snapshot::tests::build_snapshot_skips_layout_when_all_positions_saved`
saves positions after the first build and asserts the second build restores
them exactly (not clobbered by a layout re-run).

**Verification.** Load a graph with saved positions. Toggle the perf overlay
(`Ctrl+Shift+P`). Reproduce a snapshot rebuild (e.g. via agent edge add).
`force_directed_layout` should no longer appear in the rebuild timing.

### 2.3 ✅ `canvas_bounds` `Arc<RwLock<Bounds<Pixels>>>` on hot input path

**Where:** `crates/u-forge-ui-gpui/src/graph_canvas.rs:35, 71, 87, 129, 224`.

The canvas bounds are stored in `Arc<RwLock<Bounds<Pixels>>>`, read
inside mouse handlers (line 87) and written at line 224 from the paint
closure. Parking-lot locks are cheap but every mouse move on the canvas
takes a read lock.

**Fix.** Bounds can be read from GPUI's layout context inside the paint
closure and the input handler. If the external `Arc` is only needed to
hand bounds to a downstream subscriber, replace the `RwLock` with a
single-writer / many-reader `arc_swap::ArcSwap<Bounds<Pixels>>`, or if
the consumer is the view itself, keep bounds as a plain field on
`GraphCanvas` updated once per layout.

**Verification.** Perf overlay while dragging rapidly on a large canvas.
Expect no difference in the 95p frame, but reduced contention in
`perf`-based lock traces.

### 2.4 ✅ `embed_many` submits all jobs at once — no backpressure

**Where:** `crates/u-forge-core/src/queue/dispatch.rs:104-116`.

```rust
futures::future::try_join_all(
    texts.into_iter().map(|text| { ... q.embed(text).await })
).await
```

Every text becomes an immediate queue submission and its future is held
until the batch completes. For bulk import (tens of thousands of chunks
at initial sync) this materialises every pending future simultaneously —
memory grows linearly with the batch, and the queue sees a thundering
herd even though device-side parallelism is fixed.

**Fix.** Use `futures::stream::iter(...).buffer_unordered(N)` with
`N = self.embedding_workers * 2` (or a small multiple). Collect into a
`Vec<Vec<f32>>` preserving order — either by tagging inputs with their
index and sorting results, or by using `buffered()` (preserves order at
the cost of head-of-line blocking).

**Verification.** Invoke a full rechunk-and-embed on a large corpus
(>10k chunks). Record peak RSS before/after. Expect a flat curve instead
of a linear climb.

### 2.5 ✅ Snapshot rebuilds rebuild `legend_types` and R-tree from scratch

**Where:** `crates/u-forge-graph-view/src/snapshot.rs`.

bug-bash-01 Tier 2 added legend cache at the *canvas* level, but the
snapshot itself still recomputed legend-eligible types and the R-tree
spatial index on every rebuild. When the graph was mutated by a single
agent tool call, the full index was re-sorted and rebuilt via O(N log N)
`bulk_load`.

**Fix applied.**

1. `NodeEntry` (in `spatial.rs`) now stores `ObjectId` instead of a
   `usize` index. This makes R-tree entries stable across snapshot
   rebuilds — the entry for a given node is valid in any snapshot where
   that node exists, regardless of index shifts caused by other adds/removes.
   A `PartialEq` impl (by `id`) enables rstar's `remove` to locate entries
   by identity.

2. `GraphSnapshot` gained two new fields:
   - `id_to_idx: HashMap<ObjectId, usize>` — resolves spatial-index entries
     to `nodes` indices in `nodes_in_viewport` (O(1) lookup per hit).
   - `type_counts: HashMap<String, usize>` — per-type node count, maintained
     alongside `legend_types` to detect last-instance removals in O(delta).

3. `build_snapshot_incremental(graph, prev)` was added alongside the
   existing full `build_snapshot`. When all positions are saved (layout
   is skipped, per 2.2), it:
   - Clones `prev.spatial_index` and applies only removes (deleted nodes)
     and inserts (new nodes), each O(log N) instead of O(N log N).
   - Reuses `prev.legend_types` and adjusts `type_counts` in O(delta)
     when no type is newly introduced or fully removed; falls back to
     O(N) `build_legend` only when the type set changes.
   - Falls back to a full `bulk_load` when layout ran (all positions
     changed), same as before.

4. `app_view::refresh_snapshot` now calls `build_snapshot_incremental`
   when the current snapshot is non-empty; `build_snapshot` is used only
   on the initial (empty) load or after `clear_all`.

**Tests added** (`snapshot::tests`):
- `incremental_snapshot_add_node_reuses_rtree`
- `incremental_snapshot_remove_node_updates_rtree`
- `incremental_snapshot_legend_reused_when_types_unchanged`

**Verification.** Perf overlay on a 1k-node graph as the agent calls
`UpsertNodeTool` in a loop. Expect a flat per-mutation cost instead of
O(N log N) for the spatial index rebuild.

---

## Tier 3 — Resource lifecycle / correctness (deferred)

### 3.1 ✅ `run_embedding_plan` detaches tasks without explicit cancellation

**Where:** `crates/u-forge-ui-gpui/src/app_view/mod.rs:476, 523, 551`
(three `.detach()` sites in the pipeline).

The poller and worker tasks are `spawn`-then-`detach`. They self-exit
via an epoch comparison stored on `AppView` — if the epoch advances, the
task notices on its next tick and breaks. This is the declarative
pipeline from bug-bash-01 Tier 4.2 and it works, but it leaves open:

- A crashing task can't cancel its siblings — they keep running against
  a poisoned epoch until their own tick.
- Dropping the `AppView` entity does **not** immediately cancel the
  tasks. They may complete one more iteration and hold an `Arc<KnowledgeGraph>` reference a frame longer than expected.

**Fix.** Added an `Arc<AtomicBool>` cancel flag (`AppState::embedding_cancel`)
alongside the existing epoch. On `run_embedding_plan` the old flag is set
to `true` before a new `Arc<AtomicBool>` is created; both the poller and
the worker receive a clone. The poller checks `is_cancelled` before
sleeping and after waking (sub-500 ms response). The worker holds a
`CancelOnDrop` RAII guard that sets the flag on drop — including panic —
so the poller stops immediately rather than waiting for the epoch bump.
The epoch check remains as the authoritative work-skip guard.

**Verification.** `RUST_LOG=info` start two successive `embed_all()`
calls with a short sleep; verify the first pipeline exits promptly
rather than running its ticks down.

### 3.2 ✅ Embedding dimensions are compile-time constants with no schema version

**Where:** `crates/u-forge-core/src/graph/storage.rs:134, 162`.

The `vec0` virtual table declaration bakes `EMBEDDING_DIMENSIONS` (768)
and `HIGH_QUALITY_EMBEDDING_DIMENSIONS` (4096) into the on-disk schema.
Changing the embedding model forced a DB rebuild with no warning — an
upgrade path that lowers dims silently corrupted the vector index.

**Fix.** Added a `schema_metadata` TEXT key/value table to `SQL_SCHEMA`.
The new `check_or_init_embedding_dims` helper runs inside
`KnowledgeGraphStorage::new` immediately after `execute_batch`:
- First open → inserts `chunks_vec_dims` and `chunks_vec_hq_dims`.
- Subsequent opens → reads back the stored values and compares against
  the compile-time constants.
- Mismatch → returns `EmbeddingDimensionMismatch { table, stored, expected }`
  (a `thiserror` struct, re-exported from `u_forge_core`) with a message
  telling the user to re-index or pin the old model. No auto-migration.

Two new unit tests: `test_open_records_dims_in_schema_metadata` and
`test_open_with_mismatched_dims_returns_error` (tampers the stored value
and verifies the error downcasts to `EmbeddingDimensionMismatch`).

**Verification.** Create a DB with dim=768, tamper `schema_metadata` to
999, reopen — clear `EmbeddingDimensionMismatch` error at open time.

### 3.3 ✅ Chunk sizing heuristic can under-count tokens

**Where:** `crates/u-forge-core/src/text.rs` — `split_text` / hard-cut path.

The embedding tokenizer was upgraded to `o200k_harmony` (commit 3ce5c14)
so the production path uses proper tokenisation — but dense prose with
heavy punctuation, CJK text, or code blocks can still produce chunks
whose tokenised length exceeds the embedding model's context window.
The current chunker had no final clamp for whitespace-free runs.

**Fix.** Added `split_oversized_word` in `text.rs`: recursively bisects
at the character midpoint until every piece satisfies
`count_tokens(piece) <= MAX_CHUNK_TOKENS`. The existing hard-cut path in
`split_text` (previously `pieces.push(oversized)`) now calls
`pieces.extend(split_oversized_word(&candidate))`. A
`tracing::info!` fires each time a hard-split triggers — useful signal
during ingestion of non-Latin corpora. Two new unit tests cover the
base64/no-whitespace case and a 3× MAX_CHUNK_TOKENS CJK blob.

**Verification.** Ingest a document containing a large run of CJK text
or an XML blob. Confirm no embedding calls fail with a context-length
error from Lemonade.

### 3.4 ✅ Array-of-string property parsing loses quoted commas

**Where:** `crates/u-forge-ui-gpui/src/node_editor/render.rs:525` and
`crates/u-forge-ui-gpui/src/node_editor/mod.rs:527`.

Properties stored in the graph are JSON; the node editor previously
flattened arrays to comma-separated strings for a single-line input.
Entering `"a, b", c` parsed as three entries, not two.

**Fix.** Already resolved by the chip UI implemented in the node editor:
arrays render as individual item chips with remove (`x`) buttons and an
inline `+` add field. `commit_array_add` pushes the full typed text as a
single `serde_json::Value::String` without any comma splitting. A value
containing a comma (e.g. `"New York, NY"`) is stored verbatim in the
JSON array and round-trips correctly on reload.

**Verification.** Enter an array value containing a comma, save, reload
the node — the value round-trips unmodified.

---

## Tier 4 — Simplifications (deferred)

Items identified as "over-abstraction that doesn't earn its keep." Each
is a small, contained refactor. None are functional fixes.

### 4.1 🔎 Collapse `FieldKind` into the schema's `PropertyType`

**Where:**
- `crates/u-forge-ui-gpui/src/node_editor/field_spec.rs:49-56, 64, 67-73`
- Branches in `field_spec.rs:249-256` and subsequent rendering code.

`FieldKind` (Text / Number / Boolean / Enum / Array) duplicates
`PropertyType` from the core schema module. The UI also has
`field_kind_from_value`, which infers kind from a raw JSON value — this
is a fallback for nodes whose object type has no schema, but every
call site has a schema available at editor-open time.

**Fix.** Delete the `FieldKind` enum and `field_kind_from_value`. Use
`PropertyType` directly in `FieldSpec`. Adjust the field-height match
arms (`FIELD_H_SINGLE` / `FIELD_H_MULTI`) to match `PropertyType`
variants instead. If a node's object type has no schema, render all
properties as free-text — this is already the fallback; it just
doesn't need a parallel enum.

**Status: not implemented.** `FieldKind` still exists in `field_spec.rs`.

**Verification.** `cargo test --workspace -- --test-threads=1`, then
manual smoke: open a node of every built-in object type in
`defaults/schemas` and verify field rendering is unchanged.

### 4.2 🔎 `TextFieldView` blink epoch is a hand-rolled cancel-safe timer

**Where:** `crates/u-forge-ui-gpui/src/text_field.rs:44-63, 105-145`.

Cursor-blink visibility is tracked with `blink_epoch: u64`, a paired
visibility flag, and a `spawn`-reschedule chain that checks the epoch on
each tick and bails if it doesn't match. This is a manual cancel-on-next-
tick pattern built because the blink spawn isn't tied to a cancellation
handle.

**Fix.** Replace the epoch machinery with a single
`Option<Task<()>>` stored on the view. Drop the task in `on_blur` and
spawn a fresh one in `on_focus` / on any input event. GPUI's `Task`
drops cancel-safely via its cooperative handle, so the reschedule loop
inside the task just flips `cursor_visible` every ~500ms without any
epoch check. Net: ~40 lines of state-machine gone.

**Verification.** `cargo test` clean, then manual: type in a field,
click out, click in rapidly. Blink should stay smooth with no stuck-on
or stuck-off cursors.

### 4.3 🔎 Delete speculative `EmbeddingProviderType` variants

**Where:** `crates/u-forge-core/src/ai/embeddings.rs:20-27`.

```rust
pub enum EmbeddingProviderType {
    Lemonade,
    /// Placeholder for future Ollama integration.
    Ollama,
    /// Placeholder for future cloud integration.
    Cloud,
}
```

`Ollama` and `Cloud` are referenced nowhere outside this enum. They're
a speculative-generality artefact that forces every `match` on the enum
to include `_` catchalls or dead arms.

**Fix.** Delete both variants. Reinstate them the day a new provider
lands; the enum is `serde`-derived so on-disk config with a future
variant will just fail `serde_json::from_str` — which is better than a
silent no-op.

**Status: not implemented.** Both variants still present in `embeddings.rs`.

**Verification.** `cargo test --workspace` clean. Grep the workspace for
`Ollama` and `Cloud` — the only remaining hits should be doc / comment
references, if any.

### 4.4 🔎 Delete the unused `inheritance` field on `ObjectTypeSchema`

**Where:** `crates/u-forge-core/src/schema/definition.rs:129, 141`.

```rust
pub inheritance: Option<String>, // Parent object type for inheritance
```

Grep confirms zero reads anywhere in the workspace. The comment
describes a feature that was never implemented.

**Fix.** Delete the field. Fix any `ObjectTypeSchema { ... }` literal
construction (the default impl at line 141). If schema inheritance
becomes a real feature, design it with resolution semantics then — not
as a silent placeholder.

**Status: not implemented.** Field still present in `definition.rs`.

**Verification.** `cargo build --workspace` — compiler catches all
construction sites. `cargo test --workspace -- --test-threads=1` clean.

---

## Tier 5 — Observability gaps (deferred)

Small `tracing` additions that cost nothing at runtime when disabled but
give critical signal during debugging.

### 5.1 🔎 `InferenceQueue::embed` span lacks queue-depth + worker id

**Where:** `crates/u-forge-core/src/queue/dispatch.rs:76-116` (`embed`
and `embed_many`).

The `#[instrument]` on `embed` records only `text_len`. When a user
reports "embeddings are slow," there's no signal distinguishing "many
pending jobs" from "one slow inference."

**Fix.** Add span fields: `pending_jobs` (read from `self.stats()` on
entry), `selected_worker_id` (recorded in the dispatcher just before
dispatch), and `duration_us` (on span exit, via a `let _guard = span.enter()` + explicit timer).

### 5.2 🔎 UI async pipelines have no `#[instrument]`

**Where:** `crates/u-forge-ui-gpui/src/app_view/mod.rs` — `do_init_lemonade`, `run_embedding_plan`, search-kickoff detached tasks.

Zero `#[instrument]` attributes on any async method in the UI crate.
Every perf report has to be reconstructed from ad-hoc `info!` lines.

**Fix.** Add `#[instrument(skip_all, fields(...))]` to each async entry
point, with enough fields to identify the call (`plan_kind`,
`session_id`, `query_len`). Emit `debug!("milestone: X")` at each phase
boundary inside init — discover, select, build queue, ready.

### 5.3 🔎 Embedding pipeline has no peak-concurrency counter

**Where:** `crates/u-forge-core/src/ingest/embedding.rs` —
`EmbeddingPlan::execute`.

No structured record of how many in-flight embedding jobs accumulated
during a large plan. Sizing batches for new hardware becomes guesswork.

**Fix.** A single `AtomicUsize` on the plan tracking in-flight count,
plus a `max_inflight` gauge updated on each increment. Emit
`info!(target: "u_forge::ingest", max_inflight, total_jobs, duration_ms)`
at plan completion. Feeds Tier 2.4 decisions about buffer size.

---

## Out of scope for this bash — but noted

- **`save_schema` is `async` with no `.await`.** Change signature to sync
  in a separate drive-by PR; touches callers.
- **`ProviderSlot` mixes trait objects and concrete types.** Consistency
  cleanup, no behavioural change. Drive-by.
- **Builder methods (`with_max_tokens`, `with_temperature`) on one-shot
  request types.** Consider collapsing in a future style pass.
- **Config-load all-failed warning.** One-line log change; add
  opportunistically next time someone touches `AppConfig::load_default`.
- **RRF score underflow at rank 100+.** Theoretical. Add a comment if
  you're in the file for something else.
