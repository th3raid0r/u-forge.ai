use std::sync::Arc;

use gpui::{
    prelude::*, size, App, Application, Bounds, KeyBinding, Menu, MenuItem, WindowBounds,
    WindowOptions, px,
};
use u_forge_core::AppConfig;
use u_forge_graph_view::build_snapshot;
use u_forge_ui_gpui::{AppView, ClearData, ImportData, SaveLayout, ToggleRightPanel, ToggleSidebar};

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

            // Pre-load schemas so they're available synchronously in the UI.
            let schema_mgr = graph.get_schema_manager();
            if let Err(e) = schema_mgr.load_schema("default").await {
                eprintln!("Warning: could not load default schema: {e}");
            }
            if let Err(e) = schema_mgr.load_schema("imported_schemas").await {
                eprintln!("Warning: could not load imported schemas: {e}");
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
