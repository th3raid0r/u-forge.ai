# Feature: High-Performance Native UI

## Status: In Progress
## Prerequisites: `feature_refactor-for-extensibility.md` complete ✓ (workspace split + `get_all_edges()`)

---

## Goal

A native GPU-accelerated UI for exploring and editing knowledge graphs. Built on **GPUI** (Zed editor). All rendering logic is isolated behind a framework-agnostic view model crate so it has zero UI framework dependencies.

Target scale: **~3,000–10,000 nodes, ~20,000 edges** (a mature TTRPG campaign plus a full system reference like D&D 5E PHB + DMG + MM + expansions).

---

## Crates involved

- `crates/u-forge-graph-view/` — view model, layout engine, spatial indexing. **Zero UI framework dependencies.**
- `crates/u-forge-ui-traits/` — rendering contracts shared across UI implementations (see [UI Traits](#ui-traits-u-forge-ui-traits) section)
- `crates/u-forge-ui-gpui/` — GPUI app (primary)

---

## Graph View Model (`u-forge-graph-view`)

This crate converts raw `KnowledgeGraph` data into a structure optimized for frame-rate rendering. It is the most important thing to get right — both UI frameworks share it.

### Dependencies on `u-forge-core`

This crate uses the following `KnowledgeGraph` methods:
- `get_all_objects() -> Result<Vec<ObjectMetadata>>` — fetch all nodes (exists today)
- `get_all_edges() -> Result<Vec<Edge>>` — fetch all edges in one query (**added by the refactor feature**)
- `get_object(id) -> Result<Option<ObjectMetadata>>` — node detail on click
- `get_stats() -> Result<GraphStats>` — node/edge counts for status display

### Key types

```rust
/// Immutable snapshot of the graph, optimized for rendering.
pub struct GraphSnapshot {
    pub nodes: Vec<NodeView>,
    pub edges: Vec<EdgeView>,
    spatial_index: RTree<NodeEntry>,  // rstar R-tree for viewport culling
}

pub struct NodeView {
    pub id: ObjectId,
    pub name: String,
    pub object_type: String,
    pub description: Option<String>,
    pub position: glam::Vec2,
    pub tags: Vec<String>,
}

/// Indices into GraphSnapshot::nodes, NOT ObjectIds. Avoids hashmap lookups per frame.
pub struct EdgeView {
    pub source_idx: usize,
    pub target_idx: usize,
    pub edge_type: String,
    pub weight: f32,
}

pub enum LodLevel { Dot, Label, Full }
```

### Key design decisions

- `EdgeView` stores `source_idx`/`target_idx` as `usize` indices into the node vec — not `ObjectId`. This avoids a hashmap lookup per edge per frame in the render hot path. Build an `ObjectId → usize` map once during snapshot construction.
- Positions are `glam::Vec2`. Use `glam` throughout — it's what most Rust GPU/UI crates already use, which minimizes type conversion at framework boundaries.
- Spatial indexing via `rstar` (R-tree). After layout, all node positions are inserted into the tree. Viewport culling calls `nodes_in_viewport(rect)` and `edges_in_viewport(rect)` — only visible geometry is drawn.
- Three LOD levels keyed on zoom: `Dot` (circles only), `Label` (circles + names), `Full` (circles + name + type + description). At `Dot` level you draw thousands of identical shapes — no text shaping cost.
- The snapshot is **immutable from the UI thread's perspective**. The UI holds `Arc<RwLock<GraphSnapshot>>` and only takes read locks during paint. Background tasks (layout, DB refresh) take write locks after completion and swap in the new snapshot.

### `ObservableGraph`

Lives in `u-forge-graph-view`. Wraps `Arc<KnowledgeGraph>` and emits `tokio::sync::broadcast` events on mutations. The core `KnowledgeGraph` is not modified — `ObservableGraph` wraps its mutating methods and emits events after successful calls.

```rust
pub struct ObservableGraph {
    inner: Arc<KnowledgeGraph>,
    sender: broadcast::Sender<GraphEvent>,
}

pub enum GraphEvent {
    NodeAdded(ObjectId),
    NodeUpdated(ObjectId),
    NodeDeleted(ObjectId),
    EdgeAdded { source: ObjectId, target: ObjectId },
    EdgeDeleted { source: ObjectId, target: ObjectId },
}
```

UI subscribes to these events to trigger incremental snapshot refreshes rather than full rebuilds.

### `build_snapshot()` function

```rust
/// Builds a GraphSnapshot from the full graph state.
/// Uses get_all_objects() + get_all_edges() for initial load.
pub fn build_snapshot(graph: &KnowledgeGraph) -> Result<GraphSnapshot>
```

This is the main entry point. It:
1. Calls `graph.get_all_objects()` to get all nodes
2. Builds `ObjectId → usize` index map
3. Calls `graph.get_all_edges()` to get all edges
4. Maps edges to `EdgeView` using the index map (skip edges referencing unknown nodes)
5. Runs force-directed layout (or loads saved positions if available)
6. Builds the R-tree spatial index

### Force-directed layout

Use grid-cell bucketing for repulsion (O(N) per step, not O(N²)). Divide the bounding box into cells of side length ≈ 2× max repulsion radius. Each node only accumulates repulsion from nodes in its 9 neighboring cells. Attraction is computed only between connected nodes (iterate edges). This handles 10k nodes without Barnes-Hut complexity. Evaluate the `fdg` crate before writing custom layout code.

---

## UI Traits (`u-forge-ui-traits`)

Defines the rendering contract for the GPUI backend. This crate depends only on `glam` and `u-forge-graph-view` types — no UI framework dependencies.

```rust
/// A positioned, styled primitive that the UI framework draws.
pub enum DrawCommand {
    Circle { center: glam::Vec2, radius: f32, color: [u8; 4] },
    Line { from: glam::Vec2, to: glam::Vec2, width: f32, color: [u8; 4] },
    Text { position: glam::Vec2, content: String, size: f32, color: [u8; 4] },
}

/// Viewport state for culling and LOD.
pub struct Viewport {
    pub center: glam::Vec2,
    pub size: glam::Vec2,
    pub zoom: f32,
}

impl Viewport {
    pub fn world_rect(&self) -> (glam::Vec2, glam::Vec2) { /* min, max */ }
    pub fn lod_level(&self) -> LodLevel { /* based on zoom thresholds */ }
    pub fn world_to_screen(&self, world_pos: glam::Vec2) -> glam::Vec2 {
        (world_pos - self.center) * self.zoom + self.size * 0.5
    }
}

/// Trait that each UI backend implements.
pub trait GraphRenderer {
    fn draw_commands(&mut self, commands: &[DrawCommand]);
    fn canvas_size(&self) -> glam::Vec2;
}
```

The view model produces `Vec<DrawCommand>` from a `GraphSnapshot` + `Viewport`. The UI framework consumes them. This keeps the expensive logic (culling, LOD, transform) in framework-agnostic code.

---

## GPUI Prototype (`u-forge-ui-gpui`)

**GPUI version:** Using `gpui = "0.2.2"` from crates.io (blade-graphics backend). The current `main` branch of the Zed repo has restructured into `gpui_platform`/`gpui_linux`/`gpui_wgpu` sub-crates with a different entry point (`gpui_platform::application()` instead of `Application::new()`). Stay on 0.2.2 until there is a clear need to upgrade — the crates.io release is stable enough for our purposes. Do not switch to a git dependency without a targeted API compatibility pass.

### App shell (`AppView`)

`AppView` is the GPUI root view. It owns `Entity<GraphCanvas>`, `Entity<TreePanel>`, `Entity<SelectionModel>`, and boolean toggles. It renders:
- **Menu bar** (28 px, `flex_none`): "File" button (dropdown: Save / Ctrl+S via `SaveLayout` action) and "Nodes ▶/◀" button that dispatches `ToggleSidebar`. Both actions are registered with `cx.set_menus()` and `cx.bind_keys()`. Ctrl+B also toggles the sidebar.
- **Body** (remaining height, `flex_row`, `overflow_hidden()`): optional `TreePanel` (220 px, `flex_none`) on the left + main workspace.
- **Workspace** (`flex_col`, `flex_grow`): 30/70 vertical split — editor placeholder (top) + `GraphCanvas` (bottom).

The File dropdown is rendered with `deferred(anchored(...))` so it paints on top of all other content.

### Selection model (`SelectionModel`)

A shared GPUI entity that both `TreePanel` and `GraphCanvas` observe. Holds `selected_node_idx: Option<usize>` and `selected_node_id: Option<ObjectId>`. Key API:
- `select_by_idx(idx, cx)` — called from canvas clicks; keeps both fields in sync.
- `select_by_id(id, cx)` — called from tree panel clicks; looks up the index via snapshot scan.
- `clear(cx)` — deselects.

Both panels observe the entity via `cx.observe(&selection, callback)`. Observers fire whenever `SelectionModel` calls `cx.notify()`. `GraphCanvas` pans the camera to the selected node when the selection originates externally (tree panel). A `suppress_pan: bool` flag prevents the canvas from panning to a node it just selected itself.

### Tree panel (`TreePanel`)

A 220 px sidebar listing all nodes grouped by `object_type`. Rendered as a two-div structure:
1. Fixed 28 px "NODES" header (`flex_none`).
2. `overflow_y_scroll()` + `flex_grow` content area containing collapsible type groups and node entries.

Nodes are sorted case-insensitively within each group; groups are sorted by type name. All groups start **collapsed** by default — the `collapsed` set is pre-populated with all type names at construction. Clicking a type header toggles its group. Clicking a node entry updates `SelectionModel`.

### GPUI layout constraints (hard-won lessons)

These patterns are required for scroll areas to work correctly inside a flex layout:

- **`overflow_hidden()` on the body container** — without this, flex children that render more content than their computed size will grow the parent rather than clip. Apply to any container that must not exceed its allocated bounds.
- **`min_h_0()` on flex children with scrollable content** — flex items have `min-height: auto` by default, which prevents them from shrinking below their content height. `min_h_0()` overrides this so the item can shrink to its flex-allocated size and let the inner `overflow_y_scroll()` div take over.
- **Separate shell div from scroll area div** — the outer div (`flex_col`, `flex_none` width, `min_h_0()`) provides the fixed size; an inner div (`overflow_y_scroll()`, `flex_grow`) holds the scrollable content. Combining these on a single div causes layout issues.
- **`flex_none` on the sidebar** — the tree panel must not participate in flex stretching; only the workspace should grow.

### Canvas architecture

`GraphCanvas` is a child `Entity<V>` rendered inside `AppView`. It renders a `div#graph-root` (with `overflow_hidden()`) containing a single `gpui::canvas` element.

**Coordinate system.** GPUI's `window.paint_*` calls take _window_ coordinates, but `generate_draw_commands` produces _canvas-local_ coordinates (origin at the canvas element's top-left). When the canvas no longer fills the full window (e.g., sits below a menu bar and editor pane), all paint positions must be offset by `bounds.origin`. `GraphCanvas` stores `canvas_bounds: Arc<RwLock<Bounds<Pixels>>>`, updated at the top of each paint closure. Mouse event handlers read this to subtract `bounds.origin` before calling `screen_to_world`.

**Clipping.** `overflow_hidden()` on `div#graph-root` installs a GPUI `content_mask` that clips all descendant paint ops to the pane's layout bounds. Without this, nodes near the viewport edge paint into adjacent panes.

World-to-screen transform (canvas-local): `screen = (world_pos - pan) * zoom + canvas_size * 0.5`.
Window-space position: `window_pos = screen + bounds.origin`.

All `KnowledgeGraph` access happens on `cx.background_executor()` — never on the GPUI main thread, since `Mutex<Connection>` blocks.

---

## Graph Position Persistence

Node canvas positions are stored in the `node_positions` SQLite table (see `ARCHITECTURE.md` for schema). `KnowledgeGraph::save_layout(&[(ObjectId, f32, f32)])` upserts positions; `KnowledgeGraph::load_layout()` returns an `ObjectId → (x, y)` map. Both are implemented in `u-forge-core/src/graph/positions.rs`.

`build_snapshot()` in `u-forge-graph-view` always runs force-directed layout first (so new nodes get valid initial positions), then overwrites with saved positions for any node that has a stored entry. This means the layout pass is never fully skipped, but its result is overridden for known nodes — giving correct placement for mixed new+existing graphs.

The GPUI canvas saves positions **on every node drag completion** (mouse-up after a node move) rather than only on window close. The database path defaults to `./data/db/` and is configurable via the `[storage] db_path` key in `u-forge.toml`. Import from `memory.json` runs only on first launch (when the graph is empty).

`GraphSnapshot::rebuild_spatial_index()` rebuilds the R-tree from current `nodes[].position` values; called after each drag so hit-testing stays accurate.

---

## Implementation order

1. ✅ **GPUI feasibility spike** — 5k circles + 8k edges with pan/zoom in `u-forge-ui-gpui`. **Go decision.** Batched edge paths (500/batch) + LOD culling keep it responsive. GPUI 0.2.2 (crates.io).
2. ✅ **`u-forge-graph-view`** — `GraphSnapshot`, `build_snapshot()`, R-tree culling, force-directed layout. 5 passing tests.
3. ✅ **`u-forge-ui-traits`** — `DrawCommand`, `Viewport`, `generate_draw_commands()`. 2 passing tests.
4. ✅ **Wire GPUI prototype** — renders `memory.json` (220 nodes, 312 edges) with pan, zoom, LOD, type-colored nodes.
5. ✅ **Position persistence + node dragging** — `node_positions` table, `save_layout()`/`load_layout()`, drag-to-reposition nodes in the canvas, autosave on drag-end, persistent DB.
6. ✅ **App shell** — `AppView` root view wrapping `GraphCanvas`. Menu bar (File > Save, Ctrl+S, `SaveLayout` action). 30/70 flex-grow workspace split: editor placeholder (top) + graph canvas (bottom). Canvas coordinate fix: `bounds.origin` offset applied to all paint positions; mouse events subtract `bounds.origin` before `screen_to_world`. `overflow_hidden()` clips paint to pane bounds.
7. ✅ **Tree view nav + shared selection** — `SelectionModel` entity shared between `TreePanel` and `GraphCanvas`. Tree panel lists nodes by type (collapsed by default, collapsible groups, alphabetical sort). Selecting a node in the tree highlights it in the graph and pans the camera to it. Canvas clicks update the tree highlight. `suppress_pan` flag prevents the canvas from panning when it originated the selection. Sidebar toggled via Ctrl+B or "Nodes" menu bar button. GPUI layout fix: `overflow_hidden()` on body + `min_h_0()` on flex children enables true scroll containment.
8. **`ObservableGraph`** — event-driven incremental updates.
9. **Node detail panel** — click handler → detail view with node data.
10. **Search** — text input → highlight matching nodes.

---

## Verification

Load `defaults/data/memory.json` (220 nodes, 312 edges) into a temp `KnowledgeGraph` and confirm:
- Graph renders without panic
- Pan and zoom are smooth; LOD transitions are visible
- Clicking a node opens a detail panel with correct data
- Search input highlights matching nodes on the canvas
- `cargo test --workspace -- --test-threads=1` passes throughout
