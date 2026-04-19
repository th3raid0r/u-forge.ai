use std::sync::Arc;

use gpui::{
    prelude::*, size, App, Application, Bounds, KeyBinding, Menu, MenuItem, WindowBounds,
    WindowOptions, px,
};
use u_forge_core::AppConfig;
use u_forge_graph_view::build_snapshot;
use u_forge_ui_gpui::{
    AppView, ClearData, ImportData, SaveLayout, TogglePerfOverlay, ToggleRightPanel, ToggleSidebar,
};

fn main() {
    let cfg = Arc::new(AppConfig::load_default());
    let data_dir = cfg.storage.db_path.clone();
    let data_file = cfg.data.import_file.clone();
    let schema_dir = cfg.data.schema_dir.clone();

    let rt = Arc::new(tokio::runtime::Runtime::new().expect("failed to create tokio runtime"));

    let (snapshot, graph, schema_mgr) = {
        rt.block_on(async {
            let graph = Arc::new(
                u_forge_core::KnowledgeGraph::new(&data_dir)
                    .expect("failed to open knowledge graph"),
            );

            let stats = graph.get_stats().expect("failed to get stats");
            if stats.node_count == 0 {
                // Import schemas first so they're present before data.
                if std::path::Path::new(&schema_dir).exists() {
                    match u_forge_core::SchemaIngestion::load_schemas_from_directory(
                        &schema_dir,
                        "imported_schemas",
                        "1.0.0",
                    ) {
                        Ok(schema_def) => {
                            let mgr = graph.get_schema_manager();
                            if let Err(e) = mgr.save_schema(&schema_def).await {
                                eprintln!("Warning: could not save schemas: {e}");
                            } else {
                                // Clean up any stale default placeholder.
                                let _ = mgr.delete_schema("default");
                            }
                        }
                        Err(e) => eprintln!("Warning: could not load schemas: {e}"),
                    }
                }

                if data_file.exists() {
                    let mut ingestion = u_forge_core::DataIngestion::new(&graph);
                    ingestion
                        .import_json_data(&data_file)
                        .await
                        .expect("failed to import data");
                    let stats = graph.get_stats().expect("failed to get stats");
                    eprintln!(
                        "Imported {} nodes, {} edges from {}",
                        stats.node_count,
                        stats.edge_count,
                        data_file.display()
                    );
                } else {
                    eprintln!(
                        "Warning: import file '{}' not found, using empty graph",
                        data_file.display()
                    );
                }
            } else {
                eprintln!(
                    "Loaded existing graph: {} nodes, {} edges",
                    stats.node_count, stats.edge_count
                );
            }

            // Pre-load schemas into the synchronous cache so the node editor
            // can call get_object_type_schema() without async.
            //
            // Strategy:
            // - If real schemas exist (anything other than "default"), load them
            //   and delete the stale "default" placeholder so the agent never
            //   sees the hardcoded character/location types alongside npc/player_character.
            // - If the DB has no schemas at all (brand-new or cleared), bootstrap
            //   "default" as a minimal fallback until the user imports.
            let schema_mgr = graph.get_schema_manager();
            let schema_names = schema_mgr.list_schemas().unwrap_or_default();
            let has_real_schemas = schema_names.iter().any(|n| n != "default");
            if schema_names.is_empty() {
                // Fresh DB — bootstrap placeholder so the node editor isn't empty.
                if let Err(e) = schema_mgr.load_schema("default").await {
                    eprintln!("Warning: could not create default schema: {e}");
                }
            } else {
                for name in &schema_names {
                    if name == "default" && has_real_schemas {
                        // Stale placeholder — remove it so the agent only sees
                        // the imported types.
                        let _ = schema_mgr.delete_schema("default");
                        continue;
                    }
                    if let Err(e) = schema_mgr.load_schema(name).await {
                        eprintln!("Warning: could not load schema '{name}': {e}");
                    }
                }
            }

            let snapshot = build_snapshot(&graph).expect("failed to build snapshot");
            (snapshot, graph, schema_mgr)
        })
    };

    Application::new().run(move |cx: &mut App| {
        // Register keybindings.
        cx.bind_keys([
            KeyBinding::new("ctrl-s", SaveLayout, None),
            KeyBinding::new("ctrl-b", ToggleSidebar, None),
            KeyBinding::new("ctrl-j", ToggleRightPanel, None),
            KeyBinding::new("ctrl-shift-p", TogglePerfOverlay, None),
        ]);

        // Register native application menu (macOS menu bar; no-op on Linux).
        cx.set_menus(vec![
            Menu {
                name: "File".into(),
                items: vec![
                    MenuItem::action("Save", SaveLayout),
                    MenuItem::separator(),
                    MenuItem::action("Import Data", ImportData),
                    MenuItem::action("Clear Data", ClearData),
                ],
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("Toggle Left Panel", ToggleSidebar),
                    MenuItem::action("Toggle Right Panel", ToggleRightPanel),
                    MenuItem::separator(),
                    MenuItem::action("Toggle Perf Overlay", TogglePerfOverlay),
                ],
            },
        ]);

        let bounds = Bounds::centered(None, size(px(1200.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|cx| {
                    AppView::new(snapshot, graph, schema_mgr, data_file, schema_dir, cfg, rt, cx)
                })
            },
        )
        .unwrap();
    });
}
