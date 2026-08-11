use std::sync::Arc;

use gpui::{
    App, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, prelude::*, px, size,
};
#[cfg(target_os = "linux")]
use gpui::{WindowBackgroundAppearance, WindowDecorations};
use u_forge::{
    ActionContext, AppView, Assets, UiTheme, action_key_bindings, native_menus,
    startup::{StartupTimeline, prepare_app},
    window_chrome::{APPLICATION_ID, APPLICATION_NAME},
};
use u_forge_core::AppConfig;

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

    let application = Application::new().with_assets(Assets);
    application.run(move |cx: &mut App| {
        let _phase = startup.phase("gpui_application_start");
        UiTheme::init(cx);
        cx.bind_keys(action_key_bindings());

        // Register native application menu (macOS menu bar; no-op on Linux).
        cx.set_menus(native_menus(&ActionContext {
            show_advanced_controls: cfg.ui.show_advanced_controls,
            ..ActionContext::default()
        }));

        let bounds = Bounds::centered(None, size(px(1200.), px(800.)), cx);
        let mut window_options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(APPLICATION_NAME.into()),
                ..Default::default()
            }),
            app_id: Some(APPLICATION_ID.to_owned()),
            ..Default::default()
        };
        #[cfg(target_os = "linux")]
        {
            // Ask the compositor for application chrome, then honor the mode
            // that it actually negotiates via Window::window_decorations().
            window_options.window_decorations = Some(WindowDecorations::Client);
            window_options.window_background = WindowBackgroundAppearance::Transparent;
        }
        cx.open_window(window_options, |window, cx| {
            window.set_window_title(APPLICATION_NAME);
            window.set_app_id(APPLICATION_ID);
            let view = cx.new(|cx| {
                AppView::new_profiled(
                    snapshot, graph, schema_mgr, data_file, schema_dir, cfg, rt, startup, None, cx,
                )
            });
            let weak = view.downgrade();
            window.on_window_should_close(cx, move |window, cx| {
                weak.update(cx, |view, cx| view.should_close_window(window, cx))
                    .unwrap_or(true)
            });
            view
        })
        .unwrap();
    });
}
