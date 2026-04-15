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

`AppView` is the GPUI root view. It owns `Entity<GraphCanvas>`, `Entity<TreePanel>`, `Entity<NodeEditorPanel>`, `Entity<SelectionModel>`, the shared `Arc<RwLock<GraphSnapshot>>`, and boolean toggles (`sidebar_open`, `right_panel_open`, `file_menu_open`, `view_menu_open`). It renders:
- **Menu bar** (28 px, `flex_none`): "File" button (dropdown: Save / Ctrl+S) and "View" button (dropdown: checkable Left Panel / Ctrl+B, checkable Right Panel / Ctrl+J). Both dropdowns rendered with `deferred(anchored(...))`.
- **Body** (remaining height, `flex_row`, `overflow_hidden()`): optional `TreePanel` (220 px, `flex_none`) on the left + main workspace + optional right panel (280 px, `flex_none`) for chat (placeholder).
- **Workspace** (`flex_col`, `flex_grow`): 30/70 vertical split — `NodeEditorPanel` (top) + `GraphCanvas` (bottom).
- **Status bar** (24 px, `flex_none`, bottom): left section has panel toggle buttons (Tree, more coming), center shows graph stats (node/edge count from snapshot), right has a Chat toggle button. All toggle buttons highlight when their panel is open.

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

### TextFieldView — canvas-based text rendering

`TextFieldView` implements `EntityInputHandler` (GPUI's platform IME protocol) and renders text + cursor using `shape_line` / `shape_text` + `ShapedLine::paint()` / `WrappedLine::paint()` directly on a `canvas` element — **not** as a `SharedString` child of a div.

**Why not div children?** When text is placed as a div child, GPUI's own text layout system owns the glyph positions. There is no public API to query those positions from outside the div's render tree. Any cursor overlay div positioned with hardcoded pixel offsets will drift from the actual glyphs as soon as the font, DPI, or font-size varies. This was the root cause of all five cursor/click bugs encountered.

**Correct approach (same as Zed's `EditorElement`):**
1. Call `window.text_system().shape_line(text, font_size, &[run], None)` (single-line) or `shape_text(text, font_size, &[run], Some(wrap_width), None)` (multiline with wrapping).
2. Call `shaped.paint(origin, line_height, window, cx)` to draw the glyphs.
3. Use `ShapedLine::x_for_index(byte_idx)` / `WrappedLineLayout::position_for_index(byte_idx, line_h)` to compute the pixel-exact cursor position from the same shaped data.
4. Use `ShapedLine::closest_index_for_x(px)` / `WrappedLineLayout::closest_index_for_position(point, line_h)` to map a click position back to a byte index.

**Key metrics:**
- Font size: `window.rem_size() * 0.75` (matches `text_xs()` = 0.75 rem).
- Line height: `(font_size * 1.618_034).round()` — GPUI's default line height is `phi()` = `relative(1.618034)`, applied to font size. The hardcoded 16px that was there before was wrong.
- Character advance: `window.text_system().em_advance(font_id, font_size)` — used for the click-to-cursor fallback when no layout is cached yet.

**Stored layout for click mapping:**
```rust
enum TextFieldLayout {
    Single(ShapedLine),                      // single-line field
    Multi(Vec<(usize, WrappedLine)>),        // (byte_start, line) per '\n'-segment
}
```
Updated every paint frame in `TextFieldView::shaped_layout`. The `on_mouse_down` handler converts the window-coordinate click to text-area-local coords (subtract `field_origin` + padding), then calls into this cached layout for exact glyph-level hit testing.

**Element origin:** Stored as `field_origin_x / field_origin_y` from `bounds.origin` inside the paint closure. Event positions from `MouseDownEvent::position` are in window coordinates — subtracting the stored origin converts them to element-local.

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

The GPUI canvas saves positions **on explicit save** (Ctrl+S or File > Save) rather than on every drag-end. The `SaveLayout` action triggers `do_save()` which saves both layout positions and any dirty editor tabs. The database path defaults to `./data/db/` and is configurable via the `[storage] db_path` key in `u-forge.toml`. Import from `memory.json` runs only on first launch (when the graph is empty).

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
8. ✅ **Status bar + View menu** — Status bar (24 px) at bottom: left panel-toggle buttons (Tree), centered graph stats (node/edge count), right Chat toggle. View menu dropdown with checkable Left Panel (Ctrl+B) and Right Panel (Ctrl+J) items. Right panel placeholder (280 px, "Chat — coming soon"). `ToggleRightPanel` action registered.
9. ✅ **`ObservableGraph`** — `GraphEvent` enum (`NodeAdded`, `NodeUpdated`, `NodeDeleted`, `EdgeAdded`, `EdgeDeleted`). `ObservableGraph` wraps `Arc<KnowledgeGraph>`, forwards mutating calls, broadcasts events via `tokio::sync::broadcast`. `Deref<Target = KnowledgeGraph>` exposes all read-only methods directly. `NodeView` now carries `properties: serde_json::Value` so the full schema-defined payload is available in the snapshot without extra DB queries.
10. ✅ **Node detail panel** — (Superseded by item 10b.) Original `NodeDetailPanel` entity displayed read-only pretty-printed JSON. Replaced by the schema-driven editor below.
10b. ✅ **Schema-driven editor with browser tabs** — `NodeEditorPanel` replaces `NodeDetailPanel` (top 30% of workspace). Browser-style tab system: selecting a node opens it in an editor tab; unpinned tabs are replaced when a new node is selected; pinned tabs stay open. Each tab renders a schema-driven form generated from `ObjectTypeSchema` properties. Form fields: `TextFieldView` (custom text input widget implementing `EntityInputHandler`) for String/Text/Number, clickable checkbox for Boolean, anchored dropdown overlay for Enum, tag-chip list with add/remove for Array. Multi-column layout: fields flow vertically into columns (300 px each); columns overflow into additional pages with "< Prev" / "Next >" navigation buttons. Dirty state: tabs turn orange (`0xfab387`) when edited values differ from DB. Save all: Ctrl+S / File > Save persists all dirty tabs via `KnowledgeGraph::update_object()` and saves layout positions, then patches the in-memory snapshot so tree panel and canvas reflect name changes. `SchemaManager::get_object_type_schema()` added as a sync cache accessor; default schema pre-loaded at startup.
10c. ✅ **`TextFieldView` cursor and click accuracy** — Rewrote `TextFieldView::Render` to paint text via `shape_line`/`shape_text` + `ShapedLine::paint()`/`WrappedLine::paint()` on a `canvas` element instead of using a `SharedString` div child. Cursor position uses `x_for_index`/`position_for_index` on the shaped layout (pixel-exact). Click-to-cursor uses `closest_index_for_x`/`closest_index_for_position` on the same cached `TextFieldLayout` enum. Line height corrected to `font_size * phi (1.618)` (GPUI default). Element origin stored each frame so `MouseDownEvent::position` (window coords) is correctly converted to text-area-local coordinates. See "TextFieldView — canvas-based text rendering" section above.
10d. ✅ **Unified text fields, scroll support, and editable array adds** — All text fields now use wrapped rendering (`shape_text` with `wrap_width`) and dynamically grow from single-line height (28 px) up to a cap (120 px) based on content. When content exceeds the cap, the field becomes scrollable via mouse wheel, with `overflow_hidden()` clipping and a `scroll_offset` applied to paint. `ensure_cursor_visible()` is called from key/mouse handlers (never from paint) to avoid render-loop oscillation. `TextFieldLayout` simplified to a single struct (no more `Single`/`Multi` enum). Array "+" button spawns an inline `TextFieldView` that commits on Enter via a new `TextSubmit` event. `set_content` now resets cursor to position 0 so fields don't auto-scroll to the bottom on load.
11. **Search** — text input → highlight matching nodes.

---

## Verification

Load `defaults/data/memory.json` (220 nodes, 312 edges) into a temp `KnowledgeGraph` and confirm:
- Graph renders without panic
- Pan and zoom are smooth; LOD transitions are visible
- Clicking a node opens a detail panel with correct data
- Search input highlights matching nodes on the canvas
- `cargo test --workspace -- --test-threads=1` passes throughout
