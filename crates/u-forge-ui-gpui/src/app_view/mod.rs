mod render;
mod state;

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{Context, Empty, Entity, Subscription, prelude::*};
use parking_lot::RwLock;
use tracing::Instrument;
use u_forge_agent::{AgentParams, GraphAgent};
use u_forge_core::{
    AppConfig, EmbeddingOutcome, EmbeddingPlan, EmbeddingProgress, KnowledgeGraph, ObjectMetadata,
    SchemaManager,
    ingest::build_hq_embed_queue,
    lemonade::{
        Capability, GpuResourceManager, LemonadeRuntime, LemonadeServerCatalog, ModelSelector,
        ProviderFactory, QualityTier, resolve_lemonade_url,
    },
    queue::InferenceQueueBuilder,
    types::ObjectId,
};
use u_forge_graph_view::GraphSnapshot;

use state::AppState;

use crate::chat_panel::{AvailableModel, ChatPanel, ConnectRequested};
use crate::confirmation_modal::{ConfirmationAccepted, ConfirmationCancelled, ConfirmationModal};
use crate::graph_canvas::GraphCanvas;
use crate::node_editor::NodeEditorPanel;
use crate::node_panel::{CreateNodeRequest, DeleteNodeRequest, NodePanel};
use crate::path_picker::{
    PathCancelled, PathConfirmed, PathPickerKind, PathPickerModal, PickerMode,
};
use crate::search_panel::SearchPanel;
use crate::selection_model::SelectionModel;

// ── Root app view ─────────────────────────────────────────────────────────────

/// Menu bar height in pixels.
pub(crate) const MENU_BAR_H: f32 = 28.0;

/// Status bar height in pixels.
pub(crate) const STATUS_BAR_H: f32 = 24.0;

/// Default sidebar (left panel) width in pixels.
pub(crate) const DEFAULT_SIDEBAR_W: f32 = 220.0;

/// Default fraction of workspace height allocated to the editor pane.
pub(crate) const DEFAULT_EDITOR_RATIO: f32 = 0.3;

/// Default right panel width in pixels.
pub(crate) const DEFAULT_RIGHT_PANEL_W: f32 = 280.0;

/// Minimum width for any side panel.
pub(crate) const MIN_PANEL_W: f32 = 120.0;

/// Minimum width for the central workspace.
pub(crate) const MIN_WORKSPACE_W: f32 = 200.0;

/// Minimum fraction for the editor/canvas vertical split.
pub(crate) const MIN_PANE_RATIO: f32 = 0.1;

/// Maximum fraction for the editor/canvas vertical split.
pub(crate) const MAX_PANE_RATIO: f32 = 0.9;

/// Width/height of resize drag handles in pixels.
pub(crate) const RESIZE_HANDLE_SIZE: f32 = 6.0;

// ── Drag marker types ─────────────────────────────────────────────────────────

/// Drag marker for resizing the left sidebar edge.
pub(crate) struct ResizeSidebar;
impl Render for ResizeSidebar {
    fn render(&mut self, _: &mut gpui::Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// Drag marker for resizing the editor/canvas vertical split.
pub(crate) struct ResizeEditorCanvas;
impl Render for ResizeEditorCanvas {
    fn render(&mut self, _: &mut gpui::Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// Drag marker for resizing the right panel edge.
pub(crate) struct ResizeRightPanel;
impl Render for ResizeRightPanel {
    fn render(&mut self, _: &mut gpui::Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// Which panel is currently shown in the left sidebar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum SidebarTab {
    Nodes,
    Search,
}

/// Number of frame-cost samples retained for the rolling perf-overlay average.
const FRAME_TIME_WINDOW: usize = 60;

/// Fixed-size ring buffer of recent frame costs in microseconds. Recording
/// is gated on `AppView::perf_enabled`; `clear()` resets the write cursor
/// when the overlay is toggled off so stale samples don't bleed across
/// enable/disable cycles.
#[derive(Debug, Clone)]
pub(crate) struct FrameTimeRing {
    samples: [u64; FRAME_TIME_WINDOW],
    /// Count of valid samples (0..=FRAME_TIME_WINDOW).
    len: usize,
    /// Index of the next write slot (wraps modulo FRAME_TIME_WINDOW).
    write: usize,
}

impl Default for FrameTimeRing {
    fn default() -> Self {
        Self {
            samples: [0; FRAME_TIME_WINDOW],
            len: 0,
            write: 0,
        }
    }
}

impl FrameTimeRing {
    pub(crate) fn push(&mut self, sample: u64) {
        self.samples[self.write] = sample;
        self.write = (self.write + 1) % FRAME_TIME_WINDOW;
        if self.len < FRAME_TIME_WINDOW {
            self.len += 1;
        }
    }

    pub(crate) fn clear(&mut self) {
        self.len = 0;
        self.write = 0;
    }

    /// Mean of the recorded samples, or `None` when the buffer is empty.
    pub(crate) fn average(&self) -> Option<u64> {
        if self.len == 0 {
            return None;
        }
        let sum: u64 = self.samples[..self.len].iter().sum();
        Some(sum / self.len as u64)
    }
}

pub struct AppView {
    // ── Non-render state (graph, queues, config, status strings) ─────────────
    pub(crate) state: AppState,
    // ── GPUI entity handles ───────────────────────────────────────────────────
    pub(crate) graph_canvas: Entity<GraphCanvas>,
    pub(crate) node_panel: Entity<NodePanel>,
    pub(crate) search_panel: Entity<SearchPanel>,
    pub(crate) node_editor: Entity<NodeEditorPanel>,
    pub(crate) chat_panel: Entity<ChatPanel>,
    #[allow(dead_code)]
    pub(crate) selection: Entity<SelectionModel>,
    // ── UI layout state ───────────────────────────────────────────────────────
    pub(crate) file_menu_open: bool,
    pub(crate) view_menu_open: bool,
    pub(crate) sidebar_open: bool,
    pub(crate) sidebar_tab: SidebarTab,
    pub(crate) right_panel_open: bool,
    /// Current sidebar width in pixels (user-resizable).
    pub(crate) sidebar_width: f32,
    /// Fraction of workspace height for the editor pane (0.0..1.0).
    pub(crate) editor_ratio: f32,
    /// Current right panel width in pixels (user-resizable).
    pub(crate) right_panel_width: f32,
    // ── Path picker modal ─────────────────────────────────────────────────────
    /// Active path-picker dialog and which field it's editing, or None.
    pub(crate) path_picker: Option<(PathPickerKind, Entity<PathPickerModal>)>,
    /// Subscriptions for the active path picker's confirm/cancel events.
    _path_picker_subs: Vec<Subscription>,
    // ── Destructive-action confirmation ──────────────────────────────────────
    pub(crate) confirmation: Option<Entity<ConfirmationModal>>,
    pending_destructive_action: Option<DestructiveAction>,
    _confirmation_subs: Vec<Subscription>,
    // ── GPUI bookkeeping ──────────────────────────────────────────────────────
    /// Subscriptions kept alive so handlers fire (node events, chat connect).
    _node_subs: Vec<Subscription>,
    /// Coalesces core mutation events into incremental graph refreshes.
    _graph_change_task: Option<gpui::Task<()>>,
    // ── Perf overlay ──────────────────────────────────────────────────────────
    /// Whether the perf overlay is visible.
    pub(crate) perf_enabled: bool,
    /// Frame cost (µs) of the last rendered frame, measured via canvas timer
    /// (render-tree build + GPUI layout pass + paint start).
    pub(crate) last_frame_cost_us: u64,
    /// Fixed-size ring buffer of recent frame costs (µs). Written by the
    /// timing canvas only while the perf overlay is visible; summed once
    /// per frame to compute `avg_ms`. Fixed array avoids any per-frame
    /// allocation from the prior `VecDeque`.
    pub(crate) frame_times_us: FrameTimeRing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DestructiveAction {
    DeleteNode(ObjectId),
    ClearData,
    ClearSchema,
}

impl AppView {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        snapshot: GraphSnapshot,
        graph: Arc<KnowledgeGraph>,
        schema_mgr: Arc<SchemaManager>,
        data_file: std::path::PathBuf,
        schema_dir: std::path::PathBuf,
        app_config: Arc<AppConfig>,
        tokio_rt: Arc<tokio::runtime::Runtime>,
        cx: &mut Context<Self>,
    ) -> Self {
        let snapshot_arc = Arc::new(RwLock::new(snapshot));

        // Build child entities — clone Arc handles before they move into AppState.
        let selection = cx.new(|_cx| SelectionModel::new(snapshot_arc.clone()));
        let graph_canvas = cx
            .new(|cx| GraphCanvas::new(snapshot_arc.clone(), graph.clone(), selection.clone(), cx));
        let node_panel = cx.new(|_cx| NodePanel::new(snapshot_arc.clone(), selection.clone()));

        // Subscribe to node panel create/delete events.
        let node_sub_create = cx.subscribe(
            &node_panel,
            |this: &mut Self, _panel, event: &CreateNodeRequest, cx| {
                this.create_node(&event.0, cx);
            },
        );
        let node_sub_delete = cx.subscribe(
            &node_panel,
            |this: &mut Self, _panel, event: &DeleteNodeRequest, cx| {
                this.request_delete_node(event.0, cx);
            },
        );
        let search_panel = cx.new(|cx| {
            SearchPanel::new(
                selection.clone(),
                graph.clone(),
                app_config.clone(),
                tokio_rt.clone(),
                cx,
            )
        });
        let node_editor = cx.new(|cx| {
            NodeEditorPanel::new(
                snapshot_arc.clone(),
                selection.clone(),
                graph.clone(),
                schema_mgr,
                cx,
            )
        });
        let db_path = app_config.storage.db_path.clone();
        let chat_panel = cx.new(|cx| {
            ChatPanel::new(
                app_config.chat.system_prompt.clone(),
                app_config.chat.max_context_tokens,
                app_config.chat.response_reserve,
                &db_path,
                tokio_rt.clone(),
                cx,
            )
        });
        let connect_sub = cx.subscribe(
            &chat_panel,
            |this: &mut Self, _panel, _ev: &ConnectRequested, cx| {
                this.chat_panel.update(cx, |panel, _cx| {
                    panel.set_connecting(true);
                });
                this.do_init_lemonade(cx);
            },
        );

        let state = AppState::new(
            graph,
            snapshot_arc,
            data_file,
            schema_dir,
            app_config,
            tokio_rt,
        );

        let mut view = Self {
            state,
            graph_canvas,
            node_panel,
            search_panel,
            node_editor,
            chat_panel,
            selection,
            file_menu_open: false,
            view_menu_open: false,
            sidebar_open: false,
            sidebar_tab: SidebarTab::Nodes,
            right_panel_open: false,
            sidebar_width: DEFAULT_SIDEBAR_W,
            editor_ratio: DEFAULT_EDITOR_RATIO,
            right_panel_width: DEFAULT_RIGHT_PANEL_W,
            path_picker: None,
            _path_picker_subs: vec![],
            confirmation: None,
            pending_destructive_action: None,
            _confirmation_subs: vec![],
            _node_subs: vec![node_sub_create, node_sub_delete, connect_sub],
            _graph_change_task: None,
            perf_enabled: false,
            last_frame_cost_us: 0,
            frame_times_us: FrameTimeRing::default(),
        };

        let mut graph_changes = view.state.graph.subscribe_changes();
        view._graph_change_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let (receiver, event) = cx
                    .background_executor()
                    .spawn(async move {
                        let event = graph_changes.recv().await;
                        (graph_changes, event)
                    })
                    .await;
                graph_changes = receiver;
                if matches!(event, Err(tokio::sync::broadcast::error::RecvError::Closed)) {
                    return;
                }

                // Imports and agent tool chains can commit bursts of changes.
                // One frame-sized debounce turns them into one snapshot refresh.
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(16))
                    .await;
                while graph_changes.try_recv().is_ok() {}
                let Some(this) = this.upgrade() else { return };
                if this
                    .update(cx, |view: &mut AppView, cx| view.refresh_snapshot(cx))
                    .is_err()
                {
                    return;
                }
            }
        }));

        view.do_init_lemonade(cx);
        view
    }

    /// Rebuild the in-memory snapshot from the graph and push it to all child views.
    ///
    /// Uses `build_snapshot_incremental` when a previous snapshot exists so
    /// legend bookkeeping can reuse the prior type set. Spatial state is always
    /// bulk-rebuilt from the newly committed node positions.
    pub(crate) fn refresh_snapshot(&mut self, cx: &mut Context<Self>) {
        let snapshot_start = std::time::Instant::now();
        let result = {
            let prev = self.state.snapshot.read();
            if prev.nodes.is_empty() && prev.edges.is_empty() {
                u_forge_graph_view::build_snapshot(&self.state.graph)
            } else {
                u_forge_graph_view::build_snapshot_incremental(&self.state.graph, &prev)
            }
        };
        match result {
            Ok(snap) => {
                let duration_ms = snapshot_start.elapsed().as_millis() as u64;
                tracing::info!(
                    nodes = snap.nodes.len(),
                    edges = snap.edges.len(),
                    duration_ms,
                    "Graph snapshot refreshed"
                );
                let selected_still_exists = self
                    .selection
                    .read(cx)
                    .selected_node_id
                    .is_none_or(|selected| snap.nodes.iter().any(|node| node.id == selected));
                *self.state.snapshot.write() = snap;
                self.graph_canvas
                    .update(cx, |canvas, _cx| canvas.reconcile_snapshot_refresh());
                if !selected_still_exists {
                    self.selection
                        .update(cx, |selection, cx| selection.clear(cx));
                }
                self.node_panel
                    .update(cx, |panel, cx| panel.refresh_groups(cx));
                cx.notify();
            }
            Err(e) => {
                eprintln!("Failed to rebuild snapshot: {e}");
            }
        }
    }

    pub(crate) fn do_clear_data(&mut self, cx: &mut Context<Self>) {
        match self.state.graph.clear_data() {
            Ok(()) => {
                self.state.data_status = Some("Data cleared.".to_string());
                self.refresh_snapshot(cx);
            }
            Err(e) => {
                self.state.data_status = Some(format!("Clear failed: {e}"));
                cx.notify();
            }
        }
    }

    pub(crate) fn do_clear_schema(&mut self, cx: &mut Context<Self>) {
        match self.state.graph.clear_schemas() {
            Ok(()) => {
                self.state.schema_loaded = false;
                self.state.data_status = Some("Schemas cleared.".to_string());
                cx.notify();
            }
            Err(e) => {
                self.state.data_status = Some(format!("Clear schema failed: {e}"));
                cx.notify();
            }
        }
    }

    pub(crate) fn request_clear_data(&mut self, cx: &mut Context<Self>) {
        self.open_confirmation(
            DestructiveAction::ClearData,
            "Clear all data",
            "Delete every node, edge, chunk, and saved layout position? This cannot be undone.",
            "Clear Data",
            cx,
        );
    }

    pub(crate) fn request_clear_schema(&mut self, cx: &mut Context<Self>) {
        self.open_confirmation(
            DestructiveAction::ClearSchema,
            "Clear schemas",
            "Remove all imported schemas? Existing graph data is not changed, but schema validation will be unavailable.",
            "Clear Schema",
            cx,
        );
    }

    fn request_delete_node(&mut self, node_id: ObjectId, cx: &mut Context<Self>) {
        let node_name = self
            .state
            .snapshot
            .read()
            .nodes
            .iter()
            .find(|node| node.id == node_id)
            .map(|node| node.name.clone())
            .unwrap_or_else(|| node_id.to_string());
        self.open_confirmation(
            DestructiveAction::DeleteNode(node_id),
            "Delete node",
            &format!(
                "Delete “{node_name}” and its connected edges and text chunks? This cannot be undone."
            ),
            "Delete Node",
            cx,
        );
    }

    fn open_confirmation(
        &mut self,
        action: DestructiveAction,
        title: &str,
        message: &str,
        confirm_label: &str,
        cx: &mut Context<Self>,
    ) {
        let modal = cx.new(|_cx| {
            ConfirmationModal::new(
                title.to_string(),
                message.to_string(),
                confirm_label.to_string(),
            )
        });
        let accepted = cx.subscribe(&modal, |this, _modal, _event: &ConfirmationAccepted, cx| {
            let action = this.pending_destructive_action.take();
            this.confirmation = None;
            this._confirmation_subs.clear();
            if let Some(action) = action {
                this.execute_destructive_action(action, cx);
            } else {
                cx.notify();
            }
        });
        let cancelled = cx.subscribe(
            &modal,
            |this, _modal, _event: &ConfirmationCancelled, cx| {
                this.pending_destructive_action = None;
                this.confirmation = None;
                this._confirmation_subs.clear();
                cx.notify();
            },
        );
        self.pending_destructive_action = Some(action);
        self.confirmation = Some(modal);
        self._confirmation_subs = vec![accepted, cancelled];
        cx.notify();
    }

    fn execute_destructive_action(&mut self, action: DestructiveAction, cx: &mut Context<Self>) {
        match action {
            DestructiveAction::DeleteNode(node_id) => self.delete_node_by_id(node_id, cx),
            DestructiveAction::ClearData => self.do_clear_data(cx),
            DestructiveAction::ClearSchema => self.do_clear_schema(cx),
        }
    }

    pub(crate) fn do_import_data(&mut self, cx: &mut Context<Self>) {
        if !self.state.schema_loaded {
            self.state.data_status = Some("Import schema before importing data.".to_string());
            tracing::info!(
                ui_action = "import_data",
                phase = "blocked_no_schema",
                "UI action blocked"
            );
            cx.notify();
            return;
        }

        let graph = self.state.graph.clone();
        let data_file = self.state.data_file.clone();
        tracing::info!(
            ui_action = "import_data",
            phase = "clicked",
            data_file = %data_file.display(),
            "UI action started"
        );

        self.state.data_status = Some("Importing…".to_string());
        cx.notify();

        cx.spawn(async move |this, cx| {
            let import_start = std::time::Instant::now();
            let result = u_forge_core::ingest::import_data_only(&graph, &data_file).await;
            let import_duration_ms = import_start.elapsed().as_millis() as u64;
            tracing::info!(
                ui_action = "import_data",
                phase = "core_import_finished",
                duration_ms = import_duration_ms,
                success = result.is_ok(),
                "UI action phase finished"
            );

            this.update(cx, |view: &mut AppView, cx| {
                match result {
                    Ok(stats) => {
                        let reused = if stats.objects_reused > 0 {
                            format!(", {} nodes reused", stats.objects_reused)
                        } else {
                            String::new()
                        };
                        let dropped = if stats.dropped_properties > 0 {
                            format!(", {} fields dropped", stats.dropped_properties)
                        } else {
                            String::new()
                        };
                        let skipped_nodes = if stats.object_records_skipped > 0 {
                            format!(", {} nodes skipped", stats.object_records_skipped)
                        } else {
                            String::new()
                        };
                        let skipped_edges = if stats.edge_records_skipped > 0 {
                            format!(", {} edges skipped", stats.edge_records_skipped)
                        } else {
                            String::new()
                        };
                        let diagnostics = stats
                            .diagnostics_path
                            .as_ref()
                            .map(|path| format!(", diagnostics: {}", path.display()))
                            .unwrap_or_default();
                        view.state.data_status = Some(format!(
                            "Import done — {} nodes, {} edges{}{}{}{}{}",
                            stats.objects_created,
                            stats.relationships_created,
                            reused,
                            dropped,
                            skipped_nodes,
                            skipped_edges,
                            diagnostics
                        ));
                        let snapshot_start = std::time::Instant::now();
                        view.refresh_snapshot(cx);
                        tracing::info!(
                            ui_action = "import_data",
                            phase = "snapshot_finished",
                            duration_ms = snapshot_start.elapsed().as_millis() as u64,
                            "UI action phase finished"
                        );
                        // Trigger embedding after successful import.
                        view.run_embedding_plan(EmbeddingPlan::embed_all(), cx);
                    }
                    Err(e) => {
                        view.state.data_status = Some(format!("Import failed: {e}"));
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn do_import_data_picker(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let initial = self.state.data_file.to_string_lossy().into_owned();
        self.open_path_picker(
            PathPickerKind::DataFile,
            PickerMode::File,
            "Import Data",
            &initial,
            window,
            cx,
        );
    }

    pub(crate) fn do_import_schema_picker(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let initial = self.state.schema_dir.to_string_lossy().into_owned();
        self.open_path_picker(
            PathPickerKind::SchemaDir,
            PickerMode::Directory,
            "Import Schema",
            &initial,
            window,
            cx,
        );
    }

    fn open_path_picker(
        &mut self,
        kind: PathPickerKind,
        mode: PickerMode,
        title: &str,
        initial_path: &str,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let modal = cx.new(|cx| PathPickerModal::new(mode, title, title, initial_path, cx));

        let confirm_sub = cx.subscribe(&modal, |this, _modal, event: &PathConfirmed, cx| {
            let path = event.0.clone();
            match this.path_picker.as_ref().map(|(k, _)| k) {
                Some(PathPickerKind::DataFile) => {
                    this.state.data_file = path;
                    this.path_picker = None;
                    this._path_picker_subs.clear();
                    this.do_import_data(cx);
                }
                Some(PathPickerKind::SchemaDir) => {
                    this.state.schema_dir = path.clone();
                    this.path_picker = None;
                    this._path_picker_subs.clear();
                    this.do_reload_schemas_from(path, cx);
                }
                Some(PathPickerKind::ExportDir) => {
                    this.path_picker = None;
                    this._path_picker_subs.clear();
                    this.do_run_export(path, cx);
                }
                None => {}
            }
            cx.notify();
        });

        let cancel_sub = cx.subscribe(&modal, |this, _modal, _: &PathCancelled, cx| {
            this.path_picker = None;
            this._path_picker_subs.clear();
            cx.notify();
        });

        // Focus the text field so the user can start typing immediately.
        window.focus(&modal.read(cx).path_field.read(cx).focus);

        self.path_picker = Some((kind, modal));
        self._path_picker_subs = vec![confirm_sub, cancel_sub];
        cx.notify();
    }

    pub(crate) fn do_reload_schemas_from(
        &mut self,
        dir: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) {
        let graph = self.state.graph.clone();
        tracing::info!(
            ui_action = "import_schema",
            phase = "clicked",
            schema_dir = %dir.display(),
            "UI action started"
        );
        self.state.data_status = Some("Loading schemas…".to_string());
        cx.notify();

        cx.spawn(async move |this, cx| {
            let total_start = std::time::Instant::now();
            let load_start = std::time::Instant::now();
            match u_forge_core::SchemaIngestion::load_schemas_from_directory(
                &dir,
                "imported_schemas",
                "1.0.0",
            ) {
                Ok(schema_def) => {
                    tracing::info!(
                        ui_action = "import_schema",
                        phase = "schema_files_loaded",
                        duration_ms = load_start.elapsed().as_millis() as u64,
                        object_types = schema_def.object_types.len(),
                        edge_types = schema_def.edge_types.len(),
                        "UI action phase finished"
                    );
                    let mgr = graph.get_schema_manager();
                    let save_start = std::time::Instant::now();
                    // Remove the built-in placeholder before saving the imported schema set.
                    let _ = mgr.delete_schema("default");
                    let result = mgr.save_schema(&schema_def).await;
                    tracing::info!(
                        ui_action = "import_schema",
                        phase = "schema_saved",
                        duration_ms = save_start.elapsed().as_millis() as u64,
                        success = result.is_ok(),
                        "UI action phase finished"
                    );
                    this.update(cx, |view, cx| {
                        match result {
                            Ok(_) => {
                                view.state.schema_loaded = true;
                                view.state.data_status =
                                    Some("Schema directory loaded".to_string());
                            }
                            Err(e) => {
                                view.state.data_status = Some(format!("Schema load failed: {e}"));
                            }
                        }
                        tracing::info!(
                            ui_action = "import_schema",
                            phase = "finished",
                            duration_ms = total_start.elapsed().as_millis() as u64,
                            success = view.state.schema_loaded,
                            "UI action finished"
                        );
                        cx.notify();
                    })
                    .ok();
                }
                Err(e) => {
                    tracing::info!(
                        ui_action = "import_schema",
                        phase = "schema_files_loaded",
                        duration_ms = load_start.elapsed().as_millis() as u64,
                        success = false,
                        "UI action phase finished"
                    );
                    this.update(cx, |view, cx| {
                        view.state.data_status = Some(format!("Schema load failed: {e}"));
                        tracing::info!(
                            ui_action = "import_schema",
                            phase = "finished",
                            duration_ms = total_start.elapsed().as_millis() as u64,
                            success = false,
                            "UI action finished"
                        );
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    pub(crate) fn do_save(&mut self, cx: &mut Context<Self>) {
        // 1. Save layout positions.
        self.graph_canvas.read(cx).save_layout();

        // 2. Save all dirty editor tabs (also discards empty new nodes).
        let (saved, saved_ids, discarded_ids, skipped_edges) = self
            .node_editor
            .update(cx, |editor, cx| editor.save_dirty_tabs(cx));

        if skipped_edges > 0 {
            self.state.data_status = Some(format!(
                "{skipped_edges} incomplete edge(s) skipped — fill both endpoints before saving."
            ));
        }

        // If any nodes were discarded, refresh the full snapshot.
        if !discarded_ids.is_empty() {
            eprintln!("Discarded {} empty new node(s).", discarded_ids.len());
            self.refresh_snapshot(cx);
        }

        if saved > 0 {
            eprintln!("Saved {} node(s).", saved);

            // Refresh snapshot fully when edges may have changed.
            self.refresh_snapshot(cx);

            // 3. Re-chunk and embed every saved node so semantic search stays current.
            if !saved_ids.is_empty() {
                self.run_embedding_plan(EmbeddingPlan::rechunk(saved_ids), cx);
            }
        }

        cx.notify();
    }

    // ── Node create / delete (driven by node panel events) ────────────────

    /// Create a new empty node of the given type, persist it, refresh the
    /// snapshot, and navigate to it in the editor (marked as `is_new`).
    fn create_node(&mut self, object_type: &str, cx: &mut Context<Self>) {
        let meta = ObjectMetadata::new(object_type.to_string(), String::new());
        let node_id = meta.id;

        match self.state.graph.add_object(meta) {
            Ok(_id) => {
                // Refresh snapshot so the node panel and canvas see the new node.
                self.refresh_snapshot(cx);

                // Select the new node — this triggers the editor's observer,
                // but we use `open_new_node_tab` directly so the `is_new` flag
                // is set correctly.
                self.selection.update(cx, |sel, cx| {
                    sel.select_by_id(Some(node_id), cx);
                });
                self.node_editor.update(cx, |editor, cx| {
                    editor.open_new_node_tab(node_id, cx);
                });

                // Ensure the sidebar is open on the nodes tab so the user sees
                // the newly created node.
                self.sidebar_open = true;
                self.sidebar_tab = SidebarTab::Nodes;
            }
            Err(e) => {
                self.state.data_status = Some(format!("Failed to create node: {e}"));
            }
        }
        cx.notify();
    }

    /// Delete a node by its `ObjectId`, close any open editor tab for it,
    /// and refresh the snapshot.
    fn delete_node_by_id(&mut self, node_id: ObjectId, cx: &mut Context<Self>) {
        // Delete from DB (cascades to edges, chunks, etc.).
        match self.state.graph.delete_object(node_id) {
            Ok(()) => {
                // Mutate dependent UI state only after the database commit succeeds.
                self.node_editor.update(cx, |editor, cx| {
                    if let Some(idx) = editor.tabs.iter().position(|t| t.node_id == node_id) {
                        editor.close_tab(idx, cx);
                    }
                    editor.remove_stale_edge_refs(node_id);
                    cx.notify();
                });
                if self.selection.read(cx).selected_node_id == Some(node_id) {
                    self.selection
                        .update(cx, |selection, cx| selection.clear(cx));
                }
                self.refresh_snapshot(cx);
            }
            Err(e) => {
                self.state.data_status = Some(format!("Delete failed: {e}"));
            }
        }
        cx.notify();
    }

    /// Format the `embedding_status` string from a completed [`EmbeddingOutcome`].
    /// Returns `None` when nothing was actually embedded (total == 0).
    fn format_embedding_outcome(outcome: &EmbeddingOutcome) -> Option<String> {
        if outcome.stored == 0 && outcome.skipped == 0 {
            return None;
        }
        let hq_suffix = if outcome.hq_stored > 0 {
            format!(" (+{} HQ)", outcome.hq_stored)
        } else {
            String::new()
        };
        if outcome.skipped > 0 {
            Some(format!(
                "Embedded {}{hq_suffix} chunk(s), {} failed",
                outcome.stored, outcome.skipped
            ))
        } else {
            Some(format!("Embedded {}{hq_suffix} chunk(s)", outcome.stored))
        }
    }

    /// Run an [`EmbeddingPlan`] asynchronously, updating `embedding_status`
    /// from progress events as work proceeds.
    ///
    /// Replaces the former `do_rechunk_and_embed` / `do_embed_all` /
    /// `spawn_embedding_sampler` / `stop_embedding_sampler` quartet.
    /// A newer plan supersedes older UI progress. Queue work already dispatched
    /// by an older plan continues in the background until queue cancellation is
    /// supported.
    pub(crate) fn run_embedding_plan(&mut self, plan: EmbeddingPlan, cx: &mut Context<Self>) {
        let queue = match self.state.inference_queue.clone() {
            Some(q) => q,
            None => {
                tracing::info!(
                    ui_action = "embedding",
                    phase = "skipped_no_queue",
                    "UI action skipped"
                );
                return;
            }
        };
        let hq_queue = self.state.hq_queue.clone();
        if !queue.has_embedding() && !hq_queue.as_ref().is_some_and(|q| q.has_embedding()) {
            tracing::info!(
                ui_action = "embedding",
                phase = "skipped_no_provider",
                "UI action skipped"
            );
            return;
        }
        let graph = self.state.graph.clone();
        let tokio_rt = self.state.tokio_rt.clone();

        let plan_kind = plan.kind();
        match plan.has_pending_work(&graph, hq_queue.as_ref().is_some_and(|q| q.has_embedding())) {
            Ok(true) => {}
            Ok(false) => {
                self.state.embedding_status = None;
                tracing::info!(
                    ui_action = "embedding",
                    phase = "skipped_no_work",
                    plan_kind,
                    "UI action skipped"
                );
                cx.notify();
                return;
            }
            Err(e) => {
                self.state.embedding_status = Some(format!("Embedding check failed: {e}"));
                tracing::info!(
                    ui_action = "embedding",
                    phase = "check_failed",
                    plan_kind,
                    error = %e,
                    "UI action failed"
                );
                cx.notify();
                return;
            }
        }

        tracing::info!(
            ui_action = "embedding",
            phase = "scheduled",
            plan_kind,
            hq_enabled = hq_queue.as_ref().is_some_and(|q| q.has_embedding()),
            "UI action scheduled"
        );
        let (generation, superseded) = self.state.embedding_plan.start();
        self.state.embedding_status = Some(if superseded {
            format!("{} (previous work still finishing)", plan.label())
        } else {
            plan.label()
        });
        if superseded {
            tracing::info!(
                ui_action = "embedding",
                phase = "superseded",
                plan_kind,
                "Previous embedding work remains queued but may no longer update UI status"
            );
        }
        cx.notify();

        // Shared progress state written by the tokio worker, read by the poller.
        let progress_state: Arc<parking_lot::Mutex<Option<EmbeddingProgress>>> =
            Arc::new(parking_lot::Mutex::new(None));
        let progress_write = Arc::clone(&progress_state);

        // Poller: reads shared progress every 500 ms and refreshes the status bar.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(500))
                    .await;
                let Some(this) = this.upgrade() else { return };
                let snap = progress_state.lock().clone();
                let keep_running = this
                    .update(cx, |view: &mut AppView, cx| {
                        if !view.state.embedding_plan.is_current(generation) {
                            return false;
                        }
                        if let Some(EmbeddingProgress::Rechunking { done, total }) = snap {
                            view.state.embedding_status =
                                Some(format!("Re-embedding… ({done}/{total})"));
                            cx.notify();
                        }
                        true
                    })
                    .ok();
                if keep_running != Some(true) {
                    return;
                }
            }
        })
        .detach();

        // Worker: runs the plan on the tokio runtime and reports its outcome only
        // while its generation remains authoritative for UI state.
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(
                    async move {
                        let embedding_start = std::time::Instant::now();
                        tracing::info!(
                            ui_action = "embedding",
                            phase = "started",
                            plan_kind,
                            "UI action started"
                        );
                        let outcome = tokio_rt.block_on(async move {
                            plan.execute(&graph, &queue, hq_queue.as_ref(), move |p| {
                                *progress_write.lock() = Some(p)
                            })
                            .await
                        });
                        tracing::info!(
                            ui_action = "embedding",
                            phase = "finished",
                            plan_kind,
                            duration_ms = embedding_start.elapsed().as_millis() as u64,
                            stored = outcome.stored,
                            skipped = outcome.skipped,
                            hq_stored = outcome.hq_stored,
                            "UI action finished"
                        );
                        outcome
                    }
                    .instrument(tracing::info_span!("embedding_plan", plan_kind)),
                )
                .await;

            this.update(cx, |view: &mut AppView, cx| {
                if !view.state.embedding_plan.finish(generation) {
                    return;
                }
                view.state.embedding_status = Self::format_embedding_outcome(&outcome);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Asynchronously discover Lemonade Server and build the InferenceQueue + ChatProvider.
    /// FTS5 search works immediately even if this fails.
    pub(crate) fn do_init_lemonade(&mut self, cx: &mut Context<Self>) {
        let app_config = self.state.app_config.clone();
        let tokio_rt = self.state.tokio_rt.clone();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(
                    async move {
                        tokio_rt.block_on(async move {
                            // Discover Lemonade Server URL.
                            let url = match resolve_lemonade_url().await {
                                Some(u) => u,
                                None => {
                                    return Err(anyhow::anyhow!("Lemonade Server not reachable"));
                                }
                            };
                            tracing::debug!("milestone: discover — server reachable at {url}");

                            // Discover available models.
                            let catalog = LemonadeServerCatalog::discover(&url).await?;
                            tracing::debug!(
                                loaded = catalog.loaded.len(),
                                models = catalog.models.len(),
                                "milestone: select — catalog fetched"
                            );
                            let selector = ModelSelector::new(
                                &catalog,
                                &app_config.models,
                                &app_config.embedding,
                            );
                            let embed_models = selector.select_embedding_models();
                            let reranker_sel = selector.select_reranker();

                            let already_loaded: Vec<String> = catalog
                                .loaded
                                .iter()
                                .map(|m| m.model_name.clone())
                                .collect();

                            // Build provider specs for embedding + optional reranker.
                            let mut build_futs = Vec::new();
                            for sel in embed_models
                                .iter()
                                .filter(|s| s.quality_tier == QualityTier::Standard)
                            {
                                let weight = match sel.recipe.as_str() {
                                    "flm" => app_config.embedding.npu_weight,
                                    "llamacpp" => match sel.backend.as_deref() {
                                        Some("rocm") | Some("vulkan") | Some("metal") => {
                                            app_config.embedding.gpu_weight
                                        }
                                        _ => app_config.embedding.cpu_weight,
                                    },
                                    _ => app_config.embedding.cpu_weight,
                                };
                                build_futs.push((sel.clone(), Capability::Embedding, weight));
                            }
                            if let Some(r_sel) = reranker_sel {
                                build_futs.push((r_sel, Capability::Reranking, 100));
                            }

                            let gpu_mgr = GpuResourceManager::new();
                            let url_owned = url.clone();
                            let loaded = already_loaded.clone();

                            let provider_futs: Vec<_> = build_futs
                                .iter()
                                .map(|(sel, cap, weight)| {
                                    let s = sel.clone();
                                    let c = *cap;
                                    let w = *weight;
                                    let base = url_owned.clone();
                                    let ld = loaded.clone();
                                    let gm = Arc::clone(&gpu_mgr);
                                    async move {
                                        ProviderFactory::build(&s, c, &base, w, Some(gm), &ld).await
                                    }
                                })
                                .collect();

                            let build_results = futures::future::join_all(provider_futs).await;
                            let mut providers = Vec::new();
                            for build_result in build_results {
                                match build_result {
                                    Ok(provider) => providers.push(provider),
                                    Err(error) => tracing::warn!(
                                        %error,
                                        "Lemonade capability provider unavailable"
                                    ),
                                }
                            }

                            let queue = InferenceQueueBuilder::new()
                                .with_providers(providers)
                                .with_config((*app_config).clone())
                                .build();
                            tracing::debug!(
                                embedding_workers = queue.embedding_worker_count(),
                                "milestone: build queue — providers ready"
                            );

                            // Build optional HQ embedding queue.
                            let hq_queue = build_hq_embed_queue(&catalog, &app_config).await;

                            // Select ALL LLM models for the UI picker (no device-slot dedup).
                            let all_llm = selector.select_all_llm_models();
                            let llm_available: Vec<AvailableModel> =
                                all_llm.iter().map(AvailableModel::from).collect();

                            // Determine the preferred model for initial connection.
                            // Use the active device config's explicit model override,
                            // falling back to the first GPU model, then the first model.
                            let preferred_model_id =
                                app_config.chat.active_device_config().model.clone();

                            let preferred_idx = preferred_model_id
                                .as_ref()
                                .and_then(|pref| all_llm.iter().position(|m| m.model_id == *pref))
                                .or_else(|| {
                                    // Fallback: first GPU-backed model in the list.
                                    all_llm.iter().position(|m| {
                                        matches!(
                                            m.backend.as_deref(),
                                            Some("rocm") | Some("vulkan") | Some("metal")
                                        )
                                    })
                                })
                                .unwrap_or(0);

                            let chat_provider = all_llm.get(preferred_idx).map(|sel| {
                                let gpu = match sel.recipe.as_str() {
                                    "llamacpp" => match sel.backend.as_deref() {
                                        Some("rocm") | Some("vulkan") | Some("metal") => {
                                            Some(Arc::clone(&gpu_mgr))
                                        }
                                        _ => None,
                                    },
                                    _ => None,
                                };
                                u_forge_core::LemonadeChatProvider::new(&url, &sel.model_id, gpu)
                            });

                            tracing::debug!(
                                llm_count = all_llm.len(),
                                preferred_idx,
                                "milestone: ready — init complete"
                            );
                            let runtime = Arc::new(LemonadeRuntime::new(url.clone()));
                            Ok((
                                url,
                                queue,
                                hq_queue,
                                chat_provider,
                                llm_available,
                                preferred_idx,
                                runtime,
                            ))
                        })
                    }
                    .instrument(tracing::info_span!("lemonade_init")),
                )
                .await;

            this.update(cx, |view: &mut AppView, cx| {
                match result {
                    Ok((
                        lemonade_url,
                        queue,
                        hq_queue,
                        chat_provider,
                        llm_models,
                        preferred_idx,
                        runtime,
                    )) => {
                        eprintln!("Lemonade connected — capabilities discovered");
                        let has_embedding = queue.has_embedding()
                            || hq_queue.as_ref().is_some_and(|q| q.has_embedding());
                        let has_chat = chat_provider.is_some();
                        view.state.inference_queue = Some(queue.clone());
                        view.state.hq_queue = hq_queue.clone();

                        // Wrap HQ queue in Arc before it's consumed below.
                        let hq_arc = hq_queue.clone().map(Arc::new);

                        // Push queues to search panel.
                        view.search_panel.update(cx, |panel, _cx| {
                            panel.set_queues(Some(queue.clone()), hq_queue);
                        });

                        // Build the graph agent and wire it to the chat panel.
                        let graph = view.state.graph.clone();
                        let system_prompt = view.state.app_config.chat.system_prompt.clone();
                        let dev = view.state.app_config.chat.active_device_config();
                        let agent_params = AgentParams {
                            temperature: dev.temperature.map(|v| v as f64),
                            max_tokens: dev.max_tokens.map(|v| v as u64),
                            top_p: dev.top_p.map(|v| v as f64),
                            top_k: dev.top_k,
                            min_p: dev.min_p.map(|v| v as f64),
                            frequency_penalty: dev.frequency_penalty.map(|v| v as f64),
                            presence_penalty: dev.presence_penalty.map(|v| v as f64),
                            repetition_penalty: dev.repetition_penalty.map(|v| v as f64),
                            seed: dev.seed,
                            stop: dev.stop.clone(),
                            max_tool_turns: view.state.app_config.chat.max_tool_turns,
                        };
                        if has_chat {
                            match GraphAgent::new(
                                &lemonade_url,
                                graph,
                                Arc::new(queue),
                                hq_arc,
                                system_prompt,
                                agent_params,
                            ) {
                                Ok(agent) => {
                                    view.chat_panel.update(cx, |panel, _cx| {
                                        panel.set_agent(agent);
                                    });
                                }
                                Err(e) => {
                                    eprintln!("GraphAgent init failed: {e}");
                                }
                            }
                        }

                        // Push chat provider to chat panel (model list + direct streaming fallback).
                        if let Some(provider) = chat_provider {
                            view.chat_panel.update(cx, |panel, _cx| {
                                panel.set_provider(provider, llm_models, preferred_idx, runtime);
                            });
                        } else {
                            view.chat_panel.update(cx, |panel, _cx| {
                                panel.set_connect_failed("No downloaded LLM models available");
                            });
                        }

                        // Trigger bulk embedding for any unembedded chunks.
                        if has_embedding {
                            view.run_embedding_plan(EmbeddingPlan::embed_all(), cx);
                        }
                    }
                    Err(e) => {
                        eprintln!("Lemonade init skipped: {e}");
                        let msg = format!("{e}");
                        view.chat_panel.update(cx, |panel, _cx| {
                            panel.set_connect_failed(&msg);
                        });
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn do_export_data_picker(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let initial = self
            .state
            .data_file
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_string_lossy()
            .into_owned();
        self.open_path_picker(
            PathPickerKind::ExportDir,
            PickerMode::Directory,
            "Export Data",
            &initial,
            window,
            cx,
        );
    }

    pub(crate) fn do_run_export(&mut self, out_dir: std::path::PathBuf, cx: &mut Context<Self>) {
        let graph = self.state.graph.clone();

        self.state.data_status = Some("Exporting…".to_string());
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let objects = graph.get_all_objects()?;
                    let edges = graph.get_all_edges()?;

                    let id_to_name: HashMap<u_forge_core::types::ObjectId, String> =
                        objects.iter().map(|o| (o.id, o.name.clone())).collect();

                    let mut lines = Vec::with_capacity(objects.len() + edges.len());

                    for obj in &objects {
                        let mut props = match &obj.properties {
                            serde_json::Value::Object(m) => m.clone(),
                            _ => serde_json::Map::new(),
                        };
                        props
                            .entry("name".to_string())
                            .or_insert_with(|| serde_json::Value::String(obj.name.clone()));

                        let entry = serde_json::json!({
                            "entitytype": "node",
                            "id": obj.id.to_string(),
                            "nodetype": obj.object_type,
                            "properties": props,
                        });
                        lines.push(serde_json::to_string(&entry)?);
                    }

                    for edge in &edges {
                        let from = id_to_name
                            .get(&edge.from)
                            .cloned()
                            .unwrap_or_else(|| edge.from.to_string());
                        let to = id_to_name
                            .get(&edge.to)
                            .cloned()
                            .unwrap_or_else(|| edge.to.to_string());
                        let entry = serde_json::json!({
                            "entitytype": "edge",
                            "from": from,
                            "to": to,
                            "edgeType": edge.edge_type.0,
                        });
                        lines.push(serde_json::to_string(&entry)?);
                    }

                    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
                    let out_path = out_dir.join(format!("export_{timestamp}.jsonl"));
                    std::fs::write(&out_path, lines.join("\n"))?;

                    Ok::<_, anyhow::Error>((out_path, objects.len(), edges.len()))
                })
                .await;

            this.update(cx, |view: &mut AppView, cx| {
                match result {
                    Ok((path, node_count, edge_count)) => {
                        let filename = path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned();
                        view.state.data_status = Some(format!(
                            "Exported {node_count} nodes, {edge_count} edges → {filename}"
                        ));
                    }
                    Err(e) => {
                        view.state.data_status = Some(format!("Export failed: {e}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}
