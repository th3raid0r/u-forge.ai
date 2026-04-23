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

## Remaining work (tracked separately)

- **Separate import data from import schema**: `do_import_data` currently calls `setup_and_index` which also loads schemas. Need `import_data_only` in `pipeline.rs`.
- **Clear Schema** menu item: delete all schemas without touching node data.
- **Clear Data** should not clear schemas (currently `clear_all` deletes both).
- **Grey-out logic** for menu items:
  - "Clear Data" greyed when `node_count == 0`
  - "Clear Schema" greyed when no schemas present
  - "Import Data" greyed when no schema present
