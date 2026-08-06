pub mod app_view;
pub mod chat_history;
pub mod chat_message;
pub mod chat_panel;
mod confirmation_modal;
mod dock_state;
pub mod graph_canvas;
pub mod node_editor;
pub mod node_panel;
mod panel_contracts;
pub mod path_picker;
pub mod search_panel;
pub mod selection_model;
mod setup_panel;
pub mod startup;
pub mod text_field;
pub mod ui;

#[cfg(test)]
mod startup_tests;

pub use app_view::AppView;
pub use ui::theme::UiTheme;

use gpui::actions;
actions!([
    SaveLayout,
    OpenSettings,
    ToggleSidebar,
    ToggleRightPanel,
    ToggleDetailsPanel,
    ClearData,
    ClearSchema,
    ImportData,
    ImportSchema,
    ExportData,
    TogglePerfOverlay,
    FitGraph
]);
