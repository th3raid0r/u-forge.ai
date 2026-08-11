use std::sync::Arc;

use gpui::Context;
use parking_lot::RwLock;
use u_forge_core::ObjectId;
use u_forge_graph_view::GraphSnapshot;

/// Shared selection state observed by both NodePanel and GraphCanvas.
/// When either side changes the selection, it calls `cx.notify()` so
/// observers re-render.
pub(crate) struct SelectionModel {
    /// ObjectId of the currently selected node — the single source of truth.
    pub(crate) selected_node_id: Option<ObjectId>,
    /// The shared snapshot — both panels read from this.
    pub(crate) snapshot: Arc<RwLock<GraphSnapshot>>,
}

impl SelectionModel {
    pub(crate) fn new(snapshot: Arc<RwLock<GraphSnapshot>>) -> Self {
        Self {
            selected_node_id: None,
            snapshot,
        }
    }

    /// Select a node by its snapshot index. Called from graph canvas clicks.
    pub(crate) fn select_by_idx(&mut self, idx: Option<usize>, cx: &mut Context<Self>) {
        self.selected_node_id = idx.map(|i| self.snapshot.read().nodes[i].id);
        cx.notify();
    }

    /// Select a node by ObjectId. Called from node panel / search panel clicks.
    pub(crate) fn select_by_id(&mut self, id: Option<ObjectId>, cx: &mut Context<Self>) {
        self.selected_node_id = id;
        cx.notify();
    }

    pub(crate) fn clear(&mut self, cx: &mut Context<Self>) {
        self.selected_node_id = None;
        cx.notify();
    }
}
