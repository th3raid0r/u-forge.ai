use std::sync::Arc;

use gpui::{
    App, Application, Bounds, KeyBinding, Menu, MenuItem, WindowBounds, WindowOptions, prelude::*,
    px, size,
};
use u_forge_core::AppConfig;
use u_forge_ui_gpui::{
    AppView, ClearData, ClearSchema, ExportData, FitGraph, ImportData, ImportSchema, SaveLayout,
    TogglePerfOverlay, ToggleRightPanel, ToggleSidebar,
    startup::{StartupTimeline, prepare_app},
};

fn main() {
    let startup = StartupTimeline::from_env();
    let tracing_phase = startup.phase("tracing_subscriber_init");
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .init();
    drop(tracing_phase);

    let cfg = {
        let _phase = startup.phase("config_load");
        Arc::new(AppConfig::load_default())
    };
    let data_file = cfg.data.import_file.clone();
    let schema_dir = cfg.data.schema_dir.clone();

    let rt = {
        let _phase = startup.phase("tokio_runtime_create");
        Arc::new(tokio::runtime::Runtime::new().expect("failed to create tokio runtime"))
    };
    let prepared = prepare_app(&cfg, &rt, &startup).expect("failed to prepare application state");
    let snapshot = prepared.snapshot;
    let graph = prepared.graph;
    let schema_mgr = prepared.schema_manager;

    Application::new().run(move |cx: &mut App| {
        let _phase = startup.phase("gpui_application_start");
        // Register keybindings.
        cx.bind_keys([
            KeyBinding::new("ctrl-s", SaveLayout, None),
            KeyBinding::new("ctrl-b", ToggleSidebar, None),
            KeyBinding::new("ctrl-j", ToggleRightPanel, None),
            KeyBinding::new("ctrl-shift-p", TogglePerfOverlay, None),
            KeyBinding::new("ctrl-shift-0", FitGraph, None),
        ]);

        // Register native application menu (macOS menu bar; no-op on Linux).
        cx.set_menus(vec![
            Menu {
                name: "File".into(),
                items: vec![
                    MenuItem::action("Save", SaveLayout),
                    MenuItem::separator(),
                    MenuItem::action("Import Schema…", ImportSchema),
                    MenuItem::action("Import Data…", ImportData),
                    MenuItem::action("Export Data…", ExportData),
                    MenuItem::separator(),
                    MenuItem::action("Clear Schema", ClearSchema),
                    MenuItem::action("Clear Data", ClearData),
                ],
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("Toggle Left Panel", ToggleSidebar),
                    MenuItem::action("Toggle Right Panel", ToggleRightPanel),
                    MenuItem::action("Fit Graph", FitGraph),
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
                    AppView::new_profiled(
                        snapshot, graph, schema_mgr, data_file, schema_dir, cfg, rt, startup, None,
                        cx,
                    )
                })
            },
        )
        .unwrap();
    });
}
