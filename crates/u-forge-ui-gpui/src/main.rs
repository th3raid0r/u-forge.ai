use std::sync::Arc;

use gpui::{
    App, Application, Bounds, KeyBinding, Menu, MenuItem, WindowBounds, WindowOptions, prelude::*,
    px, size,
};
use u_forge_core::AppConfig;
use u_forge_ui_gpui::{
    AppView, ClearData, ClearSchema, DetailsCloseTab, DetailsNextTab, DetailsPreviousTab,
    ExportData, FitGraph, FocusNextRegion, FocusPreviousRegion, ImportData, ImportSchema,
    OpenSettings, SaveActiveItem, SaveAllItems, ToggleDetailsPanel, ToggleFocusedPanelZoom,
    TogglePerfOverlay, ToggleRightPanel, ToggleSearchPanel, ToggleSidebar, UiTheme,
    WorldActivateRow, WorldDeleteRow, WorldNextRow, WorldOpenContextMenu, WorldPreviousRow,
    startup::{StartupTimeline, prepare_app},
};

const DETAILS_NEXT_TAB_KEY: &str = "ctrl-pagedown";
const DETAILS_PREVIOUS_TAB_KEY: &str = "ctrl-pageup";

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
        UiTheme::init(cx);
        // Register keybindings.
        cx.bind_keys([
            KeyBinding::new("ctrl-s", SaveActiveItem, None),
            KeyBinding::new("ctrl-shift-s", SaveAllItems, None),
            KeyBinding::new("ctrl-b", ToggleSidebar, None),
            KeyBinding::new("ctrl-shift-f", ToggleSearchPanel, None),
            KeyBinding::new("ctrl-j", ToggleRightPanel, None),
            KeyBinding::new("ctrl-shift-j", ToggleDetailsPanel, None),
            KeyBinding::new("ctrl-shift-m", ToggleFocusedPanelZoom, None),
            KeyBinding::new("f6", FocusNextRegion, None),
            KeyBinding::new("shift-f6", FocusPreviousRegion, None),
            KeyBinding::new("down", WorldNextRow, Some("WorldPanel")),
            KeyBinding::new("up", WorldPreviousRow, Some("WorldPanel")),
            KeyBinding::new("enter", WorldActivateRow, Some("WorldPanel")),
            KeyBinding::new("delete", WorldDeleteRow, Some("WorldPanel")),
            KeyBinding::new("shift-f10", WorldOpenContextMenu, Some("WorldPanel")),
            KeyBinding::new(DETAILS_NEXT_TAB_KEY, DetailsNextTab, Some("DetailsPanel")),
            KeyBinding::new(
                DETAILS_PREVIOUS_TAB_KEY,
                DetailsPreviousTab,
                Some("DetailsPanel"),
            ),
            KeyBinding::new("ctrl-w", DetailsCloseTab, Some("DetailsPanel")),
            KeyBinding::new("ctrl-,", OpenSettings, None),
            KeyBinding::new("ctrl-shift-p", TogglePerfOverlay, None),
            KeyBinding::new("ctrl-shift-0", FitGraph, None),
        ]);

        // Register native application menu (macOS menu bar; no-op on Linux).
        cx.set_menus(vec![
            Menu {
                name: "File".into(),
                items: vec![
                    MenuItem::action("Save Changes", SaveActiveItem),
                    MenuItem::action("Save All", SaveAllItems),
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
                    MenuItem::action("Toggle World", ToggleSidebar),
                    MenuItem::action("Toggle Search", ToggleSearchPanel),
                    MenuItem::action("Toggle Assistant", ToggleRightPanel),
                    MenuItem::action("Toggle Details", ToggleDetailsPanel),
                    MenuItem::action("Maximize Focused Panel", ToggleFocusedPanelZoom),
                    MenuItem::action("Settings…", OpenSettings),
                    MenuItem::action("Fit Connections", FitGraph),
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

#[cfg(test)]
mod tests {
    use gpui::Keystroke;

    use super::{DETAILS_NEXT_TAB_KEY, DETAILS_PREVIOUS_TAB_KEY};

    #[test]
    fn details_tab_keybindings_use_gpui_named_key_syntax() {
        for binding in [DETAILS_NEXT_TAB_KEY, DETAILS_PREVIOUS_TAB_KEY] {
            assert!(
                Keystroke::parse(binding).is_ok(),
                "invalid binding: {binding}"
            );
        }
    }
}
