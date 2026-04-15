mod render;

use std::sync::Arc;

use gpui::{prelude::*, Context, Entity};
use parking_lot::RwLock;
use u_forge_core::{KnowledgeGraph, SchemaManager};
use u_forge_graph_view::GraphSnapshot;

use crate::graph_canvas::GraphCanvas;
use crate::node_editor::NodeEditorPanel;
use crate::selection_model::SelectionModel;
use crate::tree_panel::TreePanel;

// ── Root app view ─────────────────────────────────────────────────────────────

/// Menu bar height in pixels.
pub(crate) const MENU_BAR_H: f32 = 28.0;

/// Status bar height in pixels.
pub(crate) const STATUS_BAR_H: f32 = 24.0;

pub struct AppView {
    pub(crate) graph_canvas: Entity<GraphCanvas>,
    pub(crate) tree_panel: Entity<TreePanel>,
    pub(crate) node_editor: Entity<NodeEditorPanel>,
    #[allow(dead_code)]
    pub(crate) selection: Entity<SelectionModel>,
    pub(crate) snapshot: Arc<RwLock<GraphSnapshot>>,
    pub(crate) graph: Arc<KnowledgeGraph>,
    pub(crate) data_file: std::path::PathBuf,
    pub(crate) schema_dir: std::path::PathBuf,
    pub(crate) file_menu_open: bool,
    pub(crate) view_menu_open: bool,
    pub(crate) sidebar_open: bool,
    pub(crate) right_panel_open: bool,
    /// Status message displayed in the status bar during/after data operations.
    pub(crate) data_status: Option<String>,
}

impl AppView {
    pub fn new(
        snapshot: GraphSnapshot,
        graph: Arc<KnowledgeGraph>,
        schema_mgr: Arc<SchemaManager>,
        data_file: std::path::PathBuf,
        schema_dir: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) -> Self {
        let snapshot_arc = Arc::new(RwLock::new(snapshot));
        let selection = cx.new(|_cx| SelectionModel::new(snapshot_arc.clone()));
        let graph_canvas = cx.new(|cx| {
            GraphCanvas::new(snapshot_arc.clone(), graph.clone(), selection.clone(), cx)
        });
        let tree_panel =
            cx.new(|_cx| TreePanel::new(snapshot_arc.clone(), selection.clone()));
        let node_editor = cx.new(|cx| {
            NodeEditorPanel::new(
                snapshot_arc.clone(),
                selection.clone(),
                graph.clone(),
                schema_mgr,
                cx,
            )
        });
        Self {
            graph_canvas,
            tree_panel,
            node_editor,
            selection,
            snapshot: snapshot_arc,
            graph,
            data_file,
            schema_dir,
            file_menu_open: false,
            view_menu_open: false,
            sidebar_open: false,
            right_panel_open: false,
            data_status: None,
        }
    }

    /// Rebuild the in-memory snapshot from the graph and push it to all child views.
    pub(crate) fn refresh_snapshot(&mut self, cx: &mut Context<Self>) {
        match u_forge_graph_view::build_snapshot(&self.graph) {
            Ok(snap) => {
                *self.snapshot.write() = snap;
                cx.notify();
            }
            Err(e) => {
                eprintln!("Failed to rebuild snapshot: {e}");
            }
        }
    }

    pub(crate) fn do_clear_data(&mut self, cx: &mut Context<Self>) {
        match self.graph.clear_all() {
            Ok(()) => {
                self.data_status = Some("Data cleared.".to_string());
                self.refresh_snapshot(cx);
            }
            Err(e) => {
                self.data_status = Some(format!("Clear failed: {e}"));
                cx.notify();
            }
        }
    }

    pub(crate) fn do_import_data(&mut self, cx: &mut Context<Self>) {
        let graph = self.graph.clone();
        let data_file = self.data_file.clone();
        let schema_dir = self.schema_dir.to_string_lossy().into_owned();

        self.data_status = Some("Importing…".to_string());
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = u_forge_core::ingest::setup_and_index(
                &graph,
                &schema_dir,
                data_file.to_str().unwrap_or(""),
            )
            .await;

            this.update(cx, |view: &mut AppView, cx| {
                match result {
                    Ok(stats) => {
                        view.data_status = Some(format!(
                            "Import done — {} nodes, {} edges",
                            stats.objects_created, stats.relationships_created
                        ));
                        view.refresh_snapshot(cx);
                    }
                    Err(e) => {
                        view.data_status = Some(format!("Import failed: {e}"));
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn do_save(&mut self, cx: &mut Context<Self>) {
        // 1. Save layout positions.
        self.graph_canvas.read(cx).save_layout();

        // 2. Save all dirty editor tabs.
        let saved = self
            .node_editor
            .update(cx, |editor, _cx| editor.save_dirty_tabs());

        if saved > 0 {
            // Update the snapshot so tree panel and canvas reflect edits.
            let editor = self.node_editor.read(cx);
            let mut snap = self.snapshot.write();
            for tab in &editor.tabs {
                if let Some(node) = snap.nodes.iter_mut().find(|n| n.id == tab.node_id) {
                    node.name = tab.name.clone();
                    if let Some(desc) = tab
                        .edited_values
                        .get("description")
                        .and_then(|v| v.as_str())
                    {
                        node.description = if desc.is_empty() {
                            None
                        } else {
                            Some(desc.to_string())
                        };
                    }
                }
            }
            drop(snap);
            eprintln!("Saved {} node(s).", saved);
        }

        cx.notify();
    }
}
