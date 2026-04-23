mod render;
mod state;

use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use gpui::{prelude::*, Context, Empty, Entity, Subscription};
use tracing::Instrument;
use parking_lot::RwLock;
use u_forge_agent::{AgentParams, GraphAgent};
use u_forge_core::{
    ingest::build_hq_embed_queue,
    lemonade::{
        resolve_lemonade_url, Capability, GpuResourceManager, LemonadeServerCatalog, ModelSelector,
        ProviderFactory, QualityTier,
    },
    queue::InferenceQueueBuilder,
    types::ObjectId,
    AppConfig, EmbeddingOutcome, EmbeddingPlan, EmbeddingProgress, KnowledgeGraph, ObjectMetadata,
    SchemaManager,
};
use u_forge_graph_view::GraphSnapshot;

use state::AppState;

use crate::chat_panel::{AvailableModel, ChatPanel, ConnectRequested};
use crate::graph_canvas::GraphCanvas;
use crate::node_editor::NodeEditorPanel;
use crate::search_panel::SearchPanel;
use crate::selection_model::SelectionModel;
use crate::node_panel::{CreateNodeRequest, DeleteNodeRequest, NodePanel};

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
    // ── GPUI bookkeeping ──────────────────────────────────────────────────────
    /// Subscriptions kept alive so handlers fire (node events, chat connect).
    _node_subs: Vec<Subscription>,
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
                this.delete_node_by_id(event.0, cx);
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
            _node_subs: vec![node_sub_create, node_sub_delete, connect_sub],
            perf_enabled: false,
            last_frame_cost_us: 0,
            frame_times_us: FrameTimeRing::default(),
        };

        view.do_init_lemonade(cx);
        view
    }

    /// Rebuild the in-memory snapshot from the graph and push it to all child views.
    ///
    /// Uses `build_snapshot_incremental` when a previous snapshot exists so that
    /// single-mutation events (e.g. agent `UpsertNodeTool`) apply R-tree and
    /// legend deltas in O(delta × log N) instead of rebuilding from scratch.
    pub(crate) fn refresh_snapshot(&mut self, cx: &mut Context<Self>) {
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
                *self.state.snapshot.write() = snap;
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
        match self.state.graph.clear_all() {
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

    pub(crate) fn do_import_data(&mut self, cx: &mut Context<Self>) {
        let graph = self.state.graph.clone();
        let data_file = self.state.data_file.clone();
        let schema_dir = self.state.schema_dir.to_string_lossy().into_owned();

        self.state.data_status = Some("Importing…".to_string());
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
                        view.state.data_status = Some(format!(
                            "Import done — {} nodes, {} edges",
                            stats.objects_created, stats.relationships_created
                        ));
                        view.refresh_snapshot(cx);
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
        // Close the editor tab for this node if one is open.
        self.node_editor.update(cx, |editor, cx| {
            if let Some(idx) = editor.tabs.iter().position(|t| t.node_id == node_id) {
                editor.close_tab(idx, cx);
            }
            cx.notify();
        });

        // Remove stale edge references in other open tabs before deleting.
        self.node_editor.update(cx, |editor, _cx| {
            editor.remove_stale_edge_refs(node_id);
        });

        // Clear the selection if it pointed to this node.
        let selected = self.selection.read(cx).selected_node_id;
        if selected == Some(node_id) {
            self.selection.update(cx, |sel, cx| sel.clear(cx));
        }

        // Delete from DB (cascades to edges, chunks, etc.).
        match self.state.graph.delete_object(node_id) {
            Ok(()) => {
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
    /// Any previously running plan is implicitly cancelled via epoch bump.
    pub(crate) fn run_embedding_plan(&mut self, plan: EmbeddingPlan, cx: &mut Context<Self>) {
        let queue = match self.state.inference_queue.clone() {
            Some(q) => q,
            None => return,
        };
        let hq_queue = self.state.hq_queue.clone();
        let graph = self.state.graph.clone();
        let tokio_rt = self.state.tokio_rt.clone();

        let plan_kind = plan.kind();
        self.state.embedding_status = Some(plan.label());
        cx.notify();

        // Cancel any previously running pipeline (sets the old flag → tasks exit on next tick).
        self.state.embedding_cancel.store(true, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.state.embedding_cancel = Arc::clone(&cancel);

        // Bump epoch to cancel any previously running poller.
        self.state.embedding_plan_epoch = self.state.embedding_plan_epoch.wrapping_add(1);
        let epoch = self.state.embedding_plan_epoch;

        // Shared progress state written by the tokio worker, read by the poller.
        let progress_state: Arc<parking_lot::Mutex<Option<EmbeddingProgress>>> =
            Arc::new(parking_lot::Mutex::new(None));
        let progress_write = Arc::clone(&progress_state);

        let cancel_poller = Arc::clone(&cancel);
        // Poller: reads shared progress every 500 ms and refreshes the status bar.
        cx.spawn(async move |this, cx| {
            loop {
                if cancel_poller.load(Ordering::Relaxed) {
                    return;
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(500))
                    .await;
                if cancel_poller.load(Ordering::Relaxed) {
                    return;
                }
                let Some(this) = this.upgrade() else { return };
                let snap = progress_state.lock().clone();
                let keep_running = this
                    .update(cx, |view: &mut AppView, cx| {
                        if view.state.embedding_plan_epoch != epoch {
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

        // Worker: runs the plan on the tokio runtime and reports outcome.
        // `_cancel_guard` sets the cancel flag on drop (including panic), which
        // stops the poller immediately rather than waiting for an epoch bump.
        cx.spawn(async move |this, cx| {
            let _cancel_guard = state::CancelOnDrop(Arc::clone(&cancel));
            let outcome = cx
                .background_executor()
                .spawn(
                    async move {
                        tokio_rt.block_on(async move {
                            plan.execute(
                                &graph,
                                &queue,
                                hq_queue.as_ref(),
                                move |p| *progress_write.lock() = Some(p),
                            )
                            .await
                        })
                    }
                    .instrument(tracing::info_span!("embedding_plan", plan_kind)),
                )
                .await;

            this.update(cx, |view: &mut AppView, cx| {
                // Stop the poller by advancing the epoch.
                view.state.embedding_plan_epoch =
                    view.state.embedding_plan_epoch.wrapping_add(1);
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
                            None => return Err(anyhow::anyhow!("Lemonade Server not reachable")),
                        };
                        tracing::debug!("milestone: discover — server reachable at {url}");

                        // Discover available models.
                        let catalog = LemonadeServerCatalog::discover(&url).await?;
                        tracing::debug!(
                            loaded = catalog.loaded.len(),
                            models = catalog.models.len(),
                            "milestone: select — catalog fetched"
                        );
                        let selector =
                            ModelSelector::new(&catalog, &app_config.models, &app_config.embedding);
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
                        let providers: Vec<_> = build_results.into_iter().flatten().collect();

                        if providers.is_empty() {
                            return Err(anyhow::anyhow!("No embedding providers available"));
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
                        let preferred_model_id = app_config
                            .chat
                            .active_device_config()
                            .model
                            .clone();

                        let preferred_idx = preferred_model_id
                            .as_ref()
                            .and_then(|pref| all_llm.iter().position(|m| m.model_id == *pref))
                            .or_else(|| {
                                // Fallback: first GPU-backed model in the list.
                                all_llm.iter().position(|m| {
                                    matches!(m.backend.as_deref(), Some("rocm") | Some("vulkan") | Some("metal"))
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
                        Ok((url, queue, hq_queue, chat_provider, llm_available, preferred_idx))
                    })
                }
                .instrument(tracing::info_span!("lemonade_init")),
                )
                .await;

            this.update(cx, |view: &mut AppView, cx| {
                match result {
                    Ok((lemonade_url, queue, hq_queue, chat_provider, llm_models, preferred_idx)) => {
                        eprintln!("Lemonade connected — embedding queue ready");
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

                        // Push chat provider to chat panel (model list + direct streaming fallback).
                        if let Some(provider) = chat_provider {
                            view.chat_panel.update(cx, |panel, _cx| {
                                panel.set_provider(provider, llm_models, preferred_idx);
                            });
                        }

                        // Trigger bulk embedding for any unembedded chunks.
                        view.run_embedding_plan(EmbeddingPlan::embed_all(), cx);
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

    pub(crate) fn do_export_data(&mut self, cx: &mut Context<Self>) {
        let graph = self.state.graph.clone();
        let data_file = self.state.data_file.clone();

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

                    let out_dir = data_file
                        .parent()
                        .unwrap_or_else(|| std::path::Path::new("."));
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
