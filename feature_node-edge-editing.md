# Feature: Node and Edge Editing UI

## Status: ✅ Complete

## Completed in this session
- Tree panel create (+) and delete (✕) buttons for nodes
- Node editor edge editing section with filterable node-selector dropdowns
- Empty-node cleanup on save (new nodes with blank names are discarded)
- Edge diff/merge on save (deleted edges removed, new edges added)
- Bug fixes for tab replacement, stale subscriptions, dropdown positioning

## Prerequisites: Graph editing tools (`u-forge-agent`) complete ✓
- `UpsertNodeTool` and `UpsertEdgeTool` available in agent tool loop
- `KnowledgeGraph::delete_edge()` added to core

## What Changed

### Core API (`u-forge-core`)
- `KnowledgeGraph::delete_edge(from, to, edge_type)` — removes a single edge (idempotent)
- Used by both UI and agent edge deletion workflows

### Tree Panel (`u-forge-ui-gpui/src/tree_panel.rs`)
- Emits `CreateNodeRequest(object_type)` when "+" button clicked on type header
- Emits `DeleteNodeRequest(ObjectId)` when "✕" button clicked on node entry
- Layout restructured to `justify_between` so buttons align right

### Node Editor (`u-forge-ui-gpui/src/node_editor/`)

#### field_spec.rs
- **`EditableEdge`** — represents an edge being edited (from/to as `Option<ObjectId>`, display names cached)
  - `from_edge(Edge, name_map)` — convert DB edge to editable form
  - `is_complete()` — true when both endpoints + type set
- **`EditorTab`** — extended with:
  - `edited_edges: Vec<EditableEdge>` — edges as user is editing them
  - `original_edges: Vec<Edge>` — edges as they were when tab opened
  - `edge_type_entities: Vec<Entity<TextFieldView>>` — text field for each edge type
  - `is_new: bool` — true when created via tree "+" button (empty-node cleanup flag)
- **`recompute_dirty()`** — now includes edge diff check (order-independent set comparison)
  - New tabs always dirty so they participate in save

#### mod.rs
- **`EdgeNodeDropdown`** — state for filterable node-selector dropdown
  - Holds filter text field, current filter, subscription
- **Tab lifecycle overhaul:**
  - `open_new_node_tab(node_id)` — opens with `is_new = true`
  - `open_tab_for_metadata()` — loads edges from DB, creates editables + entities
  - Tab replacement now skips dirty unpinned tabs (was silently dropping unsaved edits)
- **`save_dirty_tabs(cx)`** — three-phase:
  1. Discard pass: delete empty-new nodes from DB, clean up tabs
  2. Save pass: persist dirty tabs + edge diffs (delete removed, add new)
  3. Refresh: re-fetch `original_edges` from DB so subsequent saves work
- **Edge editing helpers:**
  - `add_edge_row()` — adds blank edge + creates type text field
  - `remove_edge_row(idx)` — removes edge + text field, closes dropdown
  - `open_edge_dropdown(edge_idx, is_target)` — opens filterable node selector
  - `select_edge_node(node_id, name)` — sets endpoint, closes dropdown
- **`remove_stale_edge_refs(deleted_id)`** — removes edges referencing deleted node across all tabs
- **Subscription management:**
  - `rebuild_field_subscriptions(cx)` — text change handlers for properties
  - `rebuild_edge_type_subscriptions(cx)` — text change handlers for edge types
  - Called after tab open, close, active-tab change to keep in sync

#### render.rs
- **`render_edges_section()`** — below property columns:
  - Section header "EDGES (N)"
  - Per-edge rows: `[From ▾] — [edge type] → [To ▾] [✕]`
  - Filterable dropdown when a selector is open
    - Text field at top to filter by node name/type
    - Candidates sorted alphabetically, truncated to 50
    - Shows "No matching nodes" if empty
    - Type badge next to each candidate
  - "+ Add Edge" button at bottom

### App View (`u-forge-ui-gpui/src/app_view/mod.rs`)
- Subscribes to `CreateNodeRequest` and `DeleteNodeRequest` from tree panel
- **`create_node(object_type, cx)`** — create empty node, persist, refresh, open in editor marked `is_new`
- **`delete_node_by_id(node_id, cx)`** — close tab, clean stale edges in other tabs, delete from DB (cascades), refresh
- **`do_save(cx)`** — updated:
  - Calls `save_dirty_tabs(cx)` which returns `(saved, saved_ids, discarded_ids)`
  - Handles discarded nodes separately (full refresh)
  - Removed dead in-memory snapshot patching (was immediately overwritten)
  - Full `refresh_snapshot()` after any changes to ensure tree + canvas sync

## Bug Fixes Applied
1. Tab replacement no longer silently drops dirty unpinned tabs
2. Property `"name"` key no longer overwrites node name
3. Subscriptions properly rebuilt after tab close/discard
4. `close_tab()` clears stale dropdown/array state
5. Node deletion cleans up edges in other open tabs
6. Dropdown overlay positioning now adapts to all panel widths
7. Edge candidates sorted alphabetically before truncation

## UI/UX Details

### Tree Panel
- Type group headers show count: `Character (3)`
- "+" button green, hover effect
- "✕" button red with muted hover, only visible on hover over node entry
- Auto-expands group when creating a new node of that type

### Node Editor — Edge Section
- Below property columns, full width
- Each edge row uses horizontal flex layout
- "From" button → left of row (clickable to filter nodes)
- Em dash separator
- Edge type text field (editable, single-line)
- Arrow separator
- "To" button → right of row (clickable to filter)
- Delete button (✕) far right
- Dropdown overlay:
  - Blue border when active
  - Positioned at top of row, extends downward
  - Filter field always visible at top
  - Scrollable candidate list (max 220px height)
  - Click a candidate to select and close

### Save Behavior
- Ctrl+S saves layout + dirty tabs + discards empty-new nodes
- Each discarded node logged: "Discarded 1 empty new node(s)."
- Each saved node logged: "Saved 3 node(s)."
- After save, full `refresh_snapshot()` ensures all panels sync

## Testing Notes
- Tree panel buttons work correctly (verified clean compile + tests pass)
- Edge editing render section integrated (verified no clippy warnings)
- No stale subscriptions left after tab close/discard (subscription rebuilds explicit)
- Dropdown filtering case-insensitive on name/type, sorted alphabetically
- Empty-new nodes deleted immediately on save (DB + UI both cleaned)
- Edge diffs properly applied (order-independent, handles multi-tab edits)

## Known Limitations
- Dropdown doesn't close on click-away (click a candidate to dismiss)
- Duplicate edges not explicitly prevented (upsert is idempotent but UI shows dupes until save)
- No visual warning when adding an edge to a node that will be deleted
- Edge candidates limited to first 50 (alphabetical sort applied, good for most use cases)

## Files Modified
- `crates/u-forge-core/src/graph/edges.rs` — added `delete_edge()`
- `crates/u-forge-core/src/lib.rs` — exposed `delete_edge()` on facade
- `crates/u-forge-ui-gpui/src/tree_panel.rs` — added +/delete buttons + events
- `crates/u-forge-ui-gpui/src/node_editor/field_spec.rs` — `EditableEdge`, extended `EditorTab`
- `crates/u-forge-ui-gpui/src/node_editor/mod.rs` — edge lifecycle + dropdown + fixes
- `crates/u-forge-ui-gpui/src/node_editor/render.rs` — edge editing section + dropdown UI
- `crates/u-forge-ui-gpui/src/app_view/mod.rs` — tree event wiring + node create/delete

## Verification
```bash
cargo check --workspace  # Clean
cargo test --workspace -- --test-threads=1  # All pass
cargo clippy -p u-forge-ui-gpui -p u-forge-core  # No new warnings
```
