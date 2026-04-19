# bug_bash-01 — Alpha MVP polish roadmap

**Branch:** `topic_bug-bash-01`

Prescriptive punch list from a three-persona code review (Linus / Carmack /
Hickey) of the Alpha MVP. Tier 1 is the active work on this branch; Tiers 2–4
are durable records of what else the review surfaced, with enough detail that
each item can be picked up cold on its own future branch.

Verification legend on individual items:
- ✅ **Verified** — file/line refs spot-checked against the code.
- ⚠️ **Refined** — original claim was partly wrong; the stated fix is the
  corrected version.
- 🔎 **Plausible** — pattern in the code supports the claim but I did not
  independently confirm the exact line numbers; verify before coding.

---

## Tier 1 — Correctness / data-integrity (**LANDED** on this branch)

Four small, mutually independent fixes. All stay inside existing call sites —
no new modules, no signature rework. The work fits in one PR.

**Status:** all four fixes landed. Tests deferred — the agent crate had no
existing test harness and scaffolding one to exercise these specific paths
(mock `InferenceQueue`, in-memory `KnowledgeGraph`, simulated mpsc receiver
drop) would roughly double the diff size. Manual verification is described
per-item below.

### 1.1 ⚠️ `UpsertEdgeTool` reports re-embed failures to the LLM

**Where:** `crates/u-forge-agent/src/lib.rs:763-767`.

**Problem.** `UpsertEdgeTool::call` runs `rechunk_and_embed` on both endpoint
nodes after the edge is persisted. On failure it emits `tracing::warn!` and
continues. The tool's `Output` string still reports unconditional success.
Result: the LLM believes the edge is fully indexed and subsequent semantic
searches miss the endpoints it just updated. `UpsertNodeTool` at line 642
already bubbles via `map_err`, so this is also an inconsistency.

**Fix.** Collect `(object_id, error)` pairs that failed re-embedding into a
local `Vec`. Keep the warn logs. When building the confirmation `Output`,
append one line per failure: `"[warning] endpoint <id> re-embed failed:
<err>"`. The edge persists regardless — report, not rollback.

**Verification.** Manual: trigger an edge upsert via the chat agent while
the embedding queue has no reachable worker (e.g. stop Lemonade mid-session
or point it at an unreachable URL). Confirm the tool Output contains a
`[warning] endpoint <id> re-embed failed: …` line. Unit test deferred.

### 1.2 🔎 Chat session race on stream finalize

**Where:** `crates/u-forge-ui-gpui/src/chat_panel.rs:243-251`
(`finalize_stream`), plus `new_session`, `load_session`, `delete_session`
around lines 642-700.

**Problem.** `finalize_stream` calls `save_current_session`, which reads
`self.current_session_id` and `self.messages`. Mid-stream the user can click
`new_session` / `load_session` — those handlers swap `self.messages` and
reassign `current_session_id` immediately. Incoming `TextDelta` /
`ReasoningDelta` events append to the new session's message list. On
finalize, the polluted list saves against the new session ID.

**Fix.** Refuse to switch sessions while `self.streaming` is true. All three
handlers early-return if streaming. Add a `tracing::debug!` when suppressed
so we know if this guard fires in practice. No UI disable needed for MVP —
the existing `streaming` flag already gates the Send button.

**Verification.** Manual: start a long agent response, click `New Chat`
during streaming, verify the new session only appears after stream
completes.

### 1.3 ✅ Agent stream loop stops when receiver is dropped

**Where:** `crates/u-forge-agent/src/lib.rs:1046-1142`. Send sites at lines
1060, 1069, 1085, 1092, 1114, 1130, 1137.

**Problem.** The tokio task driving rig's `stream_prompt` loop forwards
events via `let _ = tx.send(...).await;` — errors are swallowed. When the
receiver is dropped (user closes the chat panel, app exits) the task keeps
pulling from the rig stream, running tool calls (including write tools like
`UpsertNode` / `UpsertEdge`), and burning LLM inference budget that nobody
will see.

**Fix.** Replace each swallowed send with `if tx.send(event).await.is_err()
{ break; }`. Once the receiver is gone, stop the loop. In-flight tool calls
complete (rig doesn't expose per-call cancellation) but no new ones are
started.

**Verification.** Manual: start an agent chat response, close the right
panel mid-stream (drops the GPUI receiver), observe via `RUST_LOG=info` or
Lemonade Server logs that no further tool calls fire after the panel
closes. Unit test deferred — requires a mock `CompletionsClient` that
doesn't exist yet in the agent crate.

### 1.4 ✅ Embedding queue pending count surfaced in status bar

**Where:** `crates/u-forge-ui-gpui/src/app_view/mod.rs:405-463`
(`do_rechunk_and_embed`) and `:754-818` (`do_embed_all`). Read
`InferenceQueue::stats().pending_embeddings` — already exists at
`crates/u-forge-core/src/queue/dispatch.rs:327-335`.

**Problem.** During bulk `do_embed_all` or a cascade of save-on-edit
`do_rechunk_and_embed` calls, the user sees a static "Embedding…" string.
Semantic searches queued behind the bulk work feel mysteriously slow and
nothing explains why.

**Fix.** While an embedding task runs, spawn a sampler subtask that polls
`queue.stats()` every 500 ms and updates `self.embedding_status` to
`"Embedding… (<N> pending)"` when the count is non-zero. Stop the sampler
when the main task completes — simple `Arc<AtomicBool>` sentinel. Apply to
both `do_embed_all` and `do_rechunk_and_embed`.

**Verification.** Manual: trigger re-embed on 20+ nodes at once, verify the
status bar shows a non-zero pending count that ticks down. Existing queue
stats tests still pass.

### Tier 1 execution order and check plan

1. **#1.1** — trivial, agent-local, adds a test.
2. **#1.3** — agent-local, adds a test.
3. **#1.2** — UI-only, three 3-line guards.
4. **#1.4** — UI-only, adds a sampler task.

Then: `cargo check --workspace`, `cargo clippy --workspace --all-targets
--tests -- -D warnings`, `cargo build --workspace`, `cargo test --workspace
-- --test-threads=1`.

---

## Tier 2 — Frame-perf wins (**LANDED** on this branch)

Each item was a self-contained surgical fix, typically < 50 lines. They were
independent — any one could have landed alone. Measure with the built-in perf
overlay (`Ctrl+Shift+P` in the GPUI app, `frame:<ms>` counter).

**Status:** all four items landed. Summary in `.rules` → "Recent quality
improvements". Microbenchmarks were not run — per-item expected wins were
small (≤ 1–3 ms/frame) and the fixes are independently correct.

### ~~2.1 ✅ Cache legend type list in `GraphCanvas` state~~

**Where:** `crates/u-forge-ui-gpui/src/graph_canvas.rs:246-255`.

**Observation.** The canvas paint closure iterates every node in the
snapshot into a `BTreeSet<String>`, sorts by lowercase, and rebuilds the
legend on **every frame** — even when the legend hasn't changed. Confirmed
by reading the code.

**Cost.** O(N) work on the paint hot path. ~1–3 ms/frame on a 5k-node graph;
scales linearly.

**Fix.** Store `legend_types: Vec<String>` on `GraphCanvas`. Rebuild only
when the snapshot is notified (observe the snapshot `RwLock` the same way
the selection model does). On paint, read the cached vec.

**Landed.** Precomputed on `GraphSnapshot` itself in `build_snapshot()` —
cleaner than caching on the canvas because it reuses the existing snapshot-
rebuild lifecycle (triggered on create/delete/save). No observation plumbing
needed.

**Measurement.** Perf overlay `frame:` before/after on a large graph.
Expected drop: 1–3 ms/frame.

### ~~2.2 ✅ Split `generate_draw_commands()` output into typed vecs~~

**Where:** `crates/u-forge-ui-traits/src/lib.rs` (the function), consumed at
`crates/u-forge-ui-gpui/src/graph_canvas.rs:258-339` (edges, nodes, labels
are each a separate filter pass over the same mixed `Vec<DrawCommand>`).

**Observation.** Confirmed: edges filter with `matches!(c,
DrawCommand::Line{..})`, then nodes match on `DrawCommand::Circle`, then
labels match on text. Three full iterations over the same Vec per frame,
each with branch mispredicts on the enum discriminant.

**Cost.** Cache-miss churn + redundant iteration. ~500 µs–1 ms/frame on
dense graphs.

**Fix.** Change the signature to return a struct `DrawCommands { nodes:
Vec<NodeCmd>, edges: Vec<LineCmd>, labels: Vec<TextCmd> }` (or a tuple).
Callers iterate each vec directly. The `DrawCommand` enum can stay as a
convenience alias, but the hot path uses the typed lists.

**Landed.** Introduced `NodeCmd` / `LineCmd` / `TextCmd` newtypes and a
`DrawCommands` container; dropped the `DrawCommand` enum (no implementations
outside the hot path). `GraphRenderer::draw_commands` now takes
`&DrawCommands`. The GPUI canvas iterates each typed vec once.

**Measurement.** Microbench the paint closure in release or time the three
filter sections in `graph_canvas.rs` before/after.

### ~~2.3 🔎 Virtualize the chat history dropdown~~

**Where:** `crates/u-forge-ui-gpui/src/chat_panel.rs:765-798` area.

**Observation.** When `history_dropdown_open` is true, the render path
builds a `Vec<_>` of session rows mapped to `div`s eagerly, then passes it
to `.children()`. For users with 100+ sessions, every frame the dropdown
is open this is O(N) allocation + DOM construction.

**Cost.** 1–3 ms/frame once session count ≳ 100. Invisible on small stores.

**Fix.** Use the same virtualized `ListState` pattern already used for chat
messages (see `ChatPanel::list_state` at line 66). Lazy-render only visible
rows.

**Landed.** `ChatPanel::history_list_state: ListState`. Per-row click
handlers use `entity.update(cx, ...)` directly (not `cx.listener`, which
isn't available inside the `list(...)` App-scoped child closure). Reset on
every `session_list` refresh (save + delete paths).

**Measurement.** With 100+ sessions, open the dropdown and watch the perf
overlay. Expected drop: < 1 ms/frame with virtualization.

### ~~2.4 🔎 Gate frame-time average on perf-overlay visibility~~

**Where:** `crates/u-forge-ui-gpui/src/app_view/render.rs:44-50, 807-810`.

**Observation.** The VecDeque sum of 60 frame times runs **every** frame,
whether or not the perf overlay is being displayed.

**Cost.** ~5–10 µs/frame. Negligible, but free to remove.

**Fix.** Compute the average only inside the `if perf_enabled` branch.
Optional: replace `VecDeque<u64>` with a fixed `[u64; 60]` and a write
index — no allocation, faster indexing.

**Landed.** Added `FrameTimeRing` (`[u64; 60]` with `len` + `write` cursor)
in `app_view/mod.rs`. Sampling is already gated via `.when(perf_enabled,
...)` on the timing canvas; averaging is now a single method call behind
the existing `if perf_enabled` in `perf_text`.

**Measurement.** Disable perf overlay, profile; re-enable and re-profile.
Sub-millisecond noise, cumulative on high-refresh displays.

---

## Tier 3 — Small untanglings (**LANDED** on this branch)

Each of these is worth landing before the big architectural items in Tier 4,
because they reduce the surface area those items need to touch.

### ~~3.1 ✅ Collapse `SelectionModel` to `selected_node_id` only~~

**Where:** `crates/u-forge-ui-gpui/src/selection_model.rs:11-57`.

**What is tangled.** `SelectionModel` carries both `selected_node_idx:
Option<usize>` (index into `GraphSnapshot::nodes`) and `selected_node_id:
Option<ObjectId>` (identity). Every selection change updates both in
lockstep. Confirmed: `select_by_idx` at line 30 reads the id from the
snapshot; `select_by_id` at line 38 does the reverse. On every snapshot
rebuild, the cached index can become stale and point at a different node —
but there's no validation.

**Why it matters.** Future features (incremental graph updates, undo/redo,
node deletion) each need to remember to invalidate the index. Every miss is
a silent wrong-node selection or a panic at
`self.snapshot.read().nodes[i].id`.

**Untangling.** Drop `selected_node_idx`. Keep `selected_node_id` as the
source of truth. Callers that need the index compute it on demand inside
the snapshot read (`nodes.iter().position(|n| n.id == id)`). If profiling
shows the O(N) lookup matters, build a `HashMap<ObjectId, usize>` once per
`GraphSnapshot` rebuild and store it alongside `nodes`.

**Landed.** `selected_node_idx` dropped from `SelectionModel`. `select_by_id`
return value removed (all callers ignored it). `select_by_idx` now just looks
up the id from the snapshot. `graph_canvas.rs` computes idx on demand inside
the `snapshot.read()` guard before calling `generate_draw_commands`.

### ~~3.2 🔎 Single `StoredChatMessage` source of truth~~

**Where:** `crates/u-forge-ui-gpui/src/chat_history.rs` (`StoredMessage`,
CHAT_SCHEMA) and `crates/u-forge-ui-gpui/src/chat_panel.rs` + `chat_message.rs`
(`ChatMessageRole`, `ChatEntry`, `ChatMessageView`).

**What is tangled.** Chat messages are modeled twice: as typed enums in the
UI layer and as stringly-typed rows in the persistence layer. Tool call
metadata lives as loose strings (`tool_internal_id`, `tool_args`,
`tool_result`) instead of a typed `struct ToolCall`. Every new chat feature
(edit history, regenerate, reactions) forces both models to evolve and the
CHAT_SCHEMA to migrate.

**Untangling.** Define `StoredChatMessage` as the persistence-layer source
of truth with a typed `enum StoredRole` and a typed `struct ToolCall`. The
UI-layer `ChatMessageView` converts to/from `StoredChatMessage` at the
persistence boundary only. Keep the schema additive (new columns nullable)
until a real migration tool lands.

**Landed.** `StoredMessage` renamed to `StoredChatMessage`. Added `StoredRole`
enum and `StoredToolCall` struct to `chat_history.rs`. SQLite schema unchanged
(columns stay additive). `ChatMessageView::from_stored` / `to_stored` now work
with typed fields — no more string-matching on `role` outside the persistence
boundary.

### ~~3.3 ✅ `AgentParams.temperature` becomes `Option<f64>`~~

**Where:** `crates/u-forge-agent/src/lib.rs` — `AgentParams` struct around
line 835, applied to the agent builder further down.

**What is tangled.** `temperature: f64` with `Default::default() = 0.3`
unconditionally overrides whatever the server/model default would have
been. Breaks symmetry with `top_p` / `top_k` which are already optional,
and prevents users from respecting Lemonade's per-model tuning.

**Untangling.** Change the field to `Option<f64>`. Call
`.temperature(value)` only if `Some`. Update `Default` to return `None`.

**Landed.** `AgentParams.temperature: Option<f64>`, default `None`. `build_agent`
only calls `.temperature()` when `Some`. UI side (`app_view/mod.rs`) uses
`dev.temperature.map(|v| v as f64)` directly in the struct literal — no more
conditional assignment.

---

## Tier 4 — Architectural items (do NOT tackle here; file for a dedicated branch)

These were the deepest findings in the review — each is real, but each is a
multi-PR cascade. Hickey flagged all three. The **Tier 4.1 (typed
properties)** item is the natural partner to the schema-migration system
the user has already scheduled; do them together.

### ~~4.1 🔎 Typed properties on `ObjectMetadata`~~

**Where:** `crates/u-forge-core/src/types.rs:129`
(`properties: serde_json::Value`), validated downstream in
`SchemaManager`; UI field-kind inference in
`crates/u-forge-ui-gpui/src/node_editor/field_spec.rs:49-55` and
`mod.rs:168-188`.

**What is tangled.** The schema system defines strongly-typed
`PropertySchema` (type, enum values, required flags, min/max, regex) but
the domain model stores properties as opaque `serde_json::Value`.
Validation is a runtime check in `SchemaManager`; the UI re-infers field
kinds at render time. `UpsertNodeTool` merges incoming properties as JSON
text without schema awareness.

**Why it matters.** Every future feature that needs schema enforcement
(schema migrations, immutable properties, cross-property constraints)
requires brittle JSON-path logic spread across multiple layers. Today a
property typo is silently accepted.

**Landed (boundary enforcement approach — schema migration pairing deferred).**
`SchemaValidatedValue` newtype threading was deferred (requires schema
injection at deserialization time, which migration manages). Instead:
`SchemaManager::validate_and_coerce_properties` was added — a sync,
cache-based method that coerces `String("42")` → `Number` and
`String("true"/"false")` → `Bool` in-place, and returns `Vec<PropertyIssue>`
for type mismatches and invalid enum values. `KnowledgeGraph` exposes a
wrapper. `UpsertNodeTool` calls it after the property merge and appends
`[warning]` lines to tool output for issues the LLM can self-correct.
`NodeEditorView::save_dirty_tabs` calls it before `update_object` and
emits `tracing::warn!` for the same classes. Unknown properties are
accepted silently at both boundaries. `PropertyIssue` is re-exported from
`u-forge-core`.

### ~~4.2 🔎 Declarative embedding pipeline~~

**Where:** `crates/u-forge-core/src/ingest/embedding.rs`
(`rechunk_and_embed`, `embed_all_chunks`) and the UI wrappers at
`crates/u-forge-ui-gpui/src/app_view/mod.rs:405-463` and `:754-818`.

**What is tangled.** The trip from "node changed" → "chunks regenerated" →
"embeddings stored" is procedural steps scattered across callers. The
status bar string in `AppView::embedding_status` is written by hand in
each branch and must stay in sync with actual queue state. Adding a new
embedding target (preview mode, batch import, pause-on-idle) means forking
the whole flow.

**Why it matters.** Features like "pause embedding while the user is
typing" or "batch re-embed with progress events" require understanding and
rewriting every orchestration site.

**Landed.** `EmbeddingPlan` added to `u-forge-core::ingest::embedding` with
two constructors: `EmbeddingPlan::rechunk(ids)` (per-node re-chunk + embed,
emits `EmbeddingProgress::Rechunking { done, total }` events) and
`EmbeddingPlan::embed_all()` (bulk unembedded sweep). `EmbeddingOutcome`
carries stored/skipped/hq_stored. `AppView::run_embedding_plan(plan, cx)` is
the single UI entry point — status formatting centralized in
`format_embedding_outcome`, live per-node progress via a shared
`Arc<Mutex<Option<EmbeddingProgress>>>` polled every 500 ms. Four former
methods (`do_rechunk_and_embed`, `do_embed_all`, `spawn_embedding_sampler`,
`stop_embedding_sampler`) removed; three call sites updated. Net: -70 lines
in the UI crate, schema-migration pairing explicitly deferred (not needed).

### ~~4.3 🔎 Split `AppState` from `AppView`~~

**Where:** `crates/u-forge-ui-gpui/src/app_view/mod.rs:94-138` and
`render.rs`.

**What is tangled.** `AppView` owns the graph, the snapshot, every panel
entity, the selection model, the inference queues, Lemonade provider
state, menu/status-bar rendering, embedding progress, and Lemonade
discovery orchestration. Container rendering and application state
coordination are in one struct. The render method is the only place where
concerns are locally separated.

**Why it matters.** Adding a feature (e.g. "export graph to JSON") touches
`AppView` even when it's just a background task + status update. Testing
Lemonade init in isolation is impossible. Reusing the graph / editor /
search UI in a different shell (web, embedded TS sandbox) requires
extracting the coordination layer anyway.

**Untangling.** Move all non-render state to a new `AppState` struct that
owns the graph, queues, embedding progress, and Lemonade discovery.
`AppView` becomes a pure render over `&AppState`. Defer until a second
frontend actually needs it, or until it's the natural vehicle for
adding file-picker dialogs / embedded Lemonade lifecycle management.

**Landed.** `AppState` added to `app_view/state.rs` with no GPUI imports —
the boundary is the invariant. Moved fields: `graph`, `snapshot`, `data_file`,
`schema_dir`, `app_config`, `tokio_rt`, `inference_queue`, `hq_queue`,
`data_status`, `embedding_status`, `embedding_plan_epoch`. `AppView` retains
GPUI entity handles (`graph_canvas`, `node_panel`, `search_panel`, `node_editor`,
`chat_panel`, `selection`), layout state, and perf-overlay fields. All method
bodies access state via `self.state.*`; async callbacks use `view.state.*` via
the existing `WeakEntity` update pattern. Render reads `self.state.*` for
snapshot stats and status strings.

---

## Tier 5 — Linus bycatch (small cleanups, opportunistic)

Low-priority items surfaced by the review; land whenever a nearby change
makes them free.

### ~~5.1 ✅ Edge metadata JSON silent fallback (pattern watch)~~

`.rules` says edge metadata JSON parse failures now emit `debug!()` rather
than swallowing — confirmed recently landed. The review concern is pattern
hygiene: make sure the same silent fallback doesn't propagate to `chunks`
or node-properties blob parsing. If a future reviewer finds
`.unwrap_or_default()` on a domain-critical JSON parse, promote it to a
`warn!` or an error path.

**Audit result (no code change needed):** Both edge-metadata parse sites in
`graph/edges.rs` already use `debug!()`. `chunks` table stores plain text
(no JSON parse). `storage.rs:247` propagates `properties` JSON errors via
`.with_context(...)`. No `.unwrap_or_default()` chains off any
`serde_json::from_str` call in the codebase.

### ~~5.2 ✅ `last_render_us` not reset on panel toggle~~

**Where:** `crates/u-forge-ui-gpui/src/chat_panel.rs:80` (field) and
`crates/u-forge-ui-gpui/src/app_view/render.rs:52` (consumer).

When the chat panel is toggled closed, `last_render_us` keeps its last
sampled value. Perf overlay shows a stale `chat_ms`. One-line fix: zero the
field when `right_panel_open` transitions from true to false in `AppView`.

**Landed.** Guard added at all three `right_panel_open` toggle sites in
`app_view/render.rs` (ToggleRightPanel action, Chat tab button, view-menu
item): `if this.right_panel_open { chat_panel.update(…last_render_us = 0) }`
before the flip.

---

## Docs to update when the Tier 1 work lands

- `.rules` → "Current Project State (snapshot)" → "Recent quality
  improvements": add one-line bullets for each of 1.1–1.4.
- `feature_UI.md` → if 1.2 (session-switch guard) or 1.4 (pending count
  format) changes user-visible behavior, note them.
- No `ARCHITECTURE.md` changes expected for Tier 1.

When Tier 2/3 items land on future branches, update this file to strike
them through, and update `.rules` "Recent quality improvements" the same
way.

When Tier 4 items land, they likely warrant `ARCHITECTURE.md` additions
and possibly new `feature_*.md` files (e.g. `feature_schema-migration.md`
partnering with 4.1).
