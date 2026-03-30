# Feature: High-Performance Native UI

## Status: Not started
## Prerequisites: `feature_refactor-for-extensibility.md` complete (workspace split + `get_all_edges()`)

---

## Goal

A native GPU-accelerated UI for exploring and editing knowledge graphs. Primary framework is **GPUI** (Zed editor). The codebase must support swapping UI frameworks, so all rendering logic is isolated behind a framework-agnostic view model crate that neither GPUI nor egui depend on.

Target scale: **~3,000–10,000 nodes, ~20,000 edges** (a mature TTRPG campaign plus a full system reference like D&D 5E PHB + DMG + MM + expansions).

---

## Crates involved

- `crates/u-forge-graph-view/` — view model, layout engine, spatial indexing. **Zero UI framework dependencies.**
- `crates/u-forge-ui-traits/` — rendering contracts shared across UI implementations (see [UI Traits](#ui-traits-u-forge-ui-traits) section)
- `crates/u-forge-ui-gpui/` — GPUI prototype (primary)
- `crates/u-forge-ui-egui/` — egui fallback (only if GPUI feasibility spike fails)

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

Defines the rendering contract that both GPUI and egui backends implement. This crate depends only on `glam` and `u-forge-graph-view` types — no UI framework dependencies.

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

**GPUI API instability warning:** GPUI is Zed's internal UI framework and does not follow semver. Pin to a specific git revision in `Cargo.toml` (e.g., `gpui = { git = "https://github.com/zed-industries/zed", rev = "..." }`). Expect API breakage on updates.

### Feasibility spike (do this first)

Before building the full app: render 5,000 randomly-positioned circles with pan and zoom using `gpui::canvas`. Confirm > 60fps on the target hardware. **Failure criteria:** if the spike cannot achieve 60fps with 5,000 circles after reasonable optimization (batching, viewport culling), switch to the egui fallback for the canvas. GPUI can still be used for panels and chrome in that case.

### Canvas architecture

The graph canvas is a single `gpui::canvas` element. Its paint closure reads the snapshot (non-blocking read lock), calls `nodes_in_viewport`, determines LOD from zoom level, then draws edges then nodes. World-to-screen transform: `screen = (world_pos - pan) * zoom + canvas_center`.

All `KnowledgeGraph` access happens on `cx.background_executor()` — never on the GPUI main thread, since `Mutex<Connection>` blocks.

---

## egui Fallback (`u-forge-ui-egui`)

Only implement if the GPUI spike fails. Same `GraphSnapshot`, same layout engine. Replace `gpui::canvas` with an `egui::Painter` inside `egui::CentralPanel`. The draw call structure is identical: edges first, then nodes, applying the same world-to-screen transform via the shared `Viewport` type.

---

## Graph Position Persistence

Once a prototype is validated, add a `node_positions` table to SQLite (in `u-forge-core`) with `ON DELETE CASCADE` on `node_id`. Expose `save_layout()` and `load_layout()` on the `KnowledgeGraph` facade. Update `build_snapshot()` in `u-forge-graph-view` to use saved positions when available, skipping the layout pass if all nodes have stored positions.

```sql
CREATE TABLE IF NOT EXISTS node_positions (
    node_id TEXT PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    x       REAL NOT NULL,
    y       REAL NOT NULL,
    layout_version INTEGER NOT NULL DEFAULT 1
);
```

---

## Implementation order

1. **GPUI feasibility spike** — 5k circles + pan/zoom in `u-forge-ui-gpui`. Go/no-go decision.
2. **`u-forge-graph-view`** — `GraphSnapshot`, `build_snapshot()`, R-tree culling, force-directed layout. Test with `memory.json` data.
3. **`u-forge-ui-traits`** — `DrawCommand`, `Viewport`, `GraphRenderer` trait.
4. **Wire GPUI (or egui) prototype** — render `memory.json` graph with pan, zoom, LOD.
5. **`ObservableGraph`** — event-driven incremental updates.
6. **Node detail panel** — click handler → detail view with node data.
7. **Search** — text input → highlight matching nodes.
8. **Position persistence** — `node_positions` table + `save_layout()`/`load_layout()`.

---

## Verification

Load `defaults/data/memory.json` (220 nodes, 312 edges) into a temp `KnowledgeGraph` and confirm:
- Graph renders without panic
- Pan and zoom are smooth; LOD transitions are visible
- Clicking a node opens a detail panel with correct data
- Search input highlights matching nodes on the canvas
- `cargo test --workspace -- --test-threads=1` passes throughout
