use u_forge_ui_traits::{Panel, PanelMetadata, PanelPosition};

use crate::{
    chat_panel::ChatPanel, graph_canvas::GraphCanvas, node_editor::NodeEditorPanel,
    node_panel::NodePanel, search_panel::SearchPanel,
};

macro_rules! panel {
    ($type:ty, $id:literal, $title:literal, $position:expr, $closable:literal) => {
        impl Panel for $type {
            fn metadata(&self) -> PanelMetadata {
                PanelMetadata {
                    id: $id,
                    title: $title,
                    position: $position,
                    closable: $closable,
                }
            }
        }
    };
}

panel!(NodePanel, "nodes", "Nodes", PanelPosition::Left, true);
panel!(SearchPanel, "search", "Search", PanelPosition::Left, true);
panel!(ChatPanel, "chat", "Chat", PanelPosition::Right, true);
panel!(
    NodeEditorPanel,
    "editor",
    "Editor",
    PanelPosition::Bottom,
    true
);
panel!(GraphCanvas, "graph", "Graph", PanelPosition::Center, false);
