# Feature: File/Folder Chooser

Branch: `feature_File-Folder-Chooser`

## What was built

### `PathPickerModal` — reusable in-app dialog (`src/path_picker.rs`)

A GPUI `Entity` that renders as a centered modal overlay with:
- A `TextFieldView` path input pre-populated from the caller
- A "…" browse button that opens the native OS file/folder picker (via `cx.prompt_for_paths`) and updates the field on selection
- Cancel and a context-labelled Confirm button (e.g. "Import Data", "Import Schema", "Export Data")
- Semi-transparent full-screen backdrop rendered via `deferred()` so it sits above all panels

Key types:
- `PickerMode::File` / `::Directory` — controls whether the native picker shows files or folders
- `PathPickerKind::DataFile` / `::SchemaDir` / `::ExportDir` — tells `AppView` which operation to run on confirm
- `PathConfirmed(PathBuf)` / `PathCancelled` — events emitted to `AppView`

### Consolidated File menu

Three separate operations replaced six menu items:

| Old items | New item |
|---|---|
| "Choose Data File…" + "Import Data" | **"Import Data…"** |
| "Choose Schema Dir…" | **"Import Schema…"** |
| "Export Data" | **"Export Data…"** |

Each new item opens `PathPickerModal` pre-seeded from the relevant `AppState` path, then runs the operation on confirm.

### `AppView` wiring (`app_view/mod.rs`)

- `path_picker: Option<(PathPickerKind, Entity<PathPickerModal>)>` — active modal, or None
- `_path_picker_subs: Vec<Subscription>` — confirm/cancel subscriptions kept alive while modal is open
- `open_path_picker(kind, mode, title, confirm_label, initial_path, window, cx)` — shared helper; creates modal, subscribes, focuses text field
- `do_import_data_picker` / `do_import_schema_picker` / `do_export_data_picker` — launchers
- `do_run_export(out_dir, cx)` — extracted from old `do_export_data`; writes `export_{timestamp}.jsonl` to the chosen directory
- `do_reload_schemas_from(dir, cx)` — async schema reload used by schema picker confirm

### Startup auto-import removed (`main.rs`)

The `if stats.node_count == 0 { … }` block that automatically imported schemas and data on a fresh DB is gone. The graph now starts empty; the user imports explicitly via the picker flow. The schema cache pre-load (needed for the node editor's synchronous schema lookup) is kept.

## Flow fixes (second commit)

### Separated import data from import schema
- `import_data_only(graph, data_file)` added to `ingest/pipeline.rs` — does data import + FTS5 indexing with no schema side-effects
- `do_import_data` now calls `import_data_only` instead of `setup_and_index`

### Separate clear operations
- `KnowledgeGraph::clear_data()` — deletes nodes/edges/chunks/vectors, schemas intact
- `KnowledgeGraph::clear_schemas()` — deletes all schemas, node data intact
- `do_clear_data` uses `clear_data()` (was `clear_all` which wiped schemas too)
- `do_clear_schema` (new) calls `clear_schemas()`, sets `schema_loaded = false`

### Grey-out logic
- `AppState.schema_loaded: bool` — updated on schema import/clear, initialised from DB at startup
- "Import Data…" greyed (50% alpha, no hover/click) when `!schema_loaded`
- "Export Data…" greyed when `node_count == 0`
- "Clear Schema" greyed when `!schema_loaded`
- "Clear Data" greyed when `node_count == 0`

### Menu order
Import Schema → Import Data → Export Data → [separator] → Clear Schema → Clear Data
