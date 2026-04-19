mod render;

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{prelude::*, Context, Empty, Entity, Subscription};
use parking_lot::RwLock;
use u_forge_agent::{AgentParams, GraphAgent};
use u_forge_core::{
    embed_all_chunks,
    ingest::{build_hq_embed_queue, rechunk_and_embed},
    lemonade::{
        resolve_lemonade_url, Capability, GpuResourceManager, LemonadeServerCatalog, ModelSelector,
        ProviderFactory, QualityTier,
    },
    queue::{InferenceQueue, InferenceQueueBuilder},
    types::ObjectId,
    AppConfig, EmbeddingTarget, KnowledgeGraph, ObjectMetadata, SchemaManager,
};
use u_forge_graph_view::GraphSnapshot;

use crate::chat_panel::{AvailableModel, ChatPanel};
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
    pub(crate) graph_canvas: Entity<GraphCanvas>,
    pub(crate) node_panel: Entity<NodePanel>,
    pub(crate) search_panel: Entity<SearchPanel>,
    pub(crate) node_editor: Entity<NodeEditorPanel>,
    pub(crate) chat_panel: Entity<ChatPanel>,
    #[allow(dead_code)]
    pub(crate) selection: Entity<SelectionModel>,
    pub(crate) snapshot: Arc<RwLock<GraphSnapshot>>,
    pub(crate) graph: Arc<KnowledgeGraph>,
    pub(crate) data_file: std::path::PathBuf,
    pub(crate) schema_dir: std::path::PathBuf,
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
    /// Status message displayed in the status bar during/after data operations.
    pub(crate) data_status: Option<String>,
    /// Embedding progress/completion message shown in the status bar.
    pub(crate) embedding_status: Option<String>,
    /// Shared app config.
    pub(crate) app_config: Arc<AppConfig>,
    /// Persistent tokio runtime for async core calls from background tasks.
    pub(crate) tokio_rt: Arc<tokio::runtime::Runtime>,
    /// Standard embedding + reranking queue (None until Lemonade is discovered).
    pub(crate) inference_queue: Option<InferenceQueue>,
    /// High-quality embedding queue (None when HQ embedding is disabled or unavailable).
    pub(crate) hq_queue: Option<InferenceQueue>,
    /// Subscriptions to node panel create/delete events — kept alive so handlers fire.
    _node_subs: Vec<Subscription>,
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
    /// Epoch for the embedding-status sampler. Bumping cancels any in-flight
    /// sampler timers so stale ticks don't overwrite a fresh status string.
    pub(crate) embedding_sampler_epoch: usize,
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

        let mut view = Self {
            graph_canvas,
            node_panel,
            search_panel,
            node_editor,
            chat_panel,
            selection,
            snapshot: snapshot_arc,
            graph,
            data_file,
            schema_dir,
            file_menu_open: false,
            view_menu_open: false,
            sidebar_open: false,
            sidebar_tab: SidebarTab::Nodes,
            right_panel_open: false,
            sidebar_width: DEFAULT_SIDEBAR_W,
            editor_ratio: DEFAULT_EDITOR_RATIO,
            right_panel_width: DEFAULT_RIGHT_PANEL_W,
            data_status: None,
            embedding_status: None,
            app_config,
            tokio_rt,
            inference_queue: None,
            hq_queue: None,
            _node_subs: vec![node_sub_create, node_sub_delete],
            perf_enabled: false,
            last_frame_cost_us: 0,
            frame_times_us: FrameTimeRing::default(),
            embedding_sampler_epoch: 0,
        };

        view.do_init_lemonade(cx);
        view
    }

    /// Rebuild the in-memory snapshot from the graph and push it to all child views.
    pub(crate) fn refresh_snapshot(&mut self, cx: &mut Context<Self>) {
        match u_forge_graph_view::build_snapshot(&self.graph) {
            Ok(snap) => {
                *self.snapshot.write() = snap;
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
                        // Trigger embedding after successful import.
                        view.do_embed_all(cx);
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

        // 2. Save all dirty editor tabs (also discards empty new nodes).
        let (saved, saved_ids, discarded_ids) = self
            .node_editor
            .update(cx, |editor, cx| editor.save_dirty_tabs(cx));

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
                self.do_rechunk_and_embed(saved_ids, cx);
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

        match self.graph.add_object(meta) {
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
                self.data_status = Some(format!("Failed to create node: {e}"));
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
        match self.graph.delete_object(node_id) {
            Ok(()) => {
                self.refresh_snapshot(cx);
            }
            Err(e) => {
                self.data_status = Some(format!("Delete failed: {e}"));
            }
        }
        cx.notify();
    }

    /// Start (or restart) a periodic sampler that appends the current queue
    /// pending-embedding count to `self.embedding_status`. Bumping
    /// `embedding_sampler_epoch` implicitly cancels any prior sampler.
    ///
    /// The sampler polls `InferenceQueue::stats()` every 500 ms and rewrites
    /// `embedding_status` to `"{base_status} ({N} pending)"` when N > 0.
    /// It stops when the epoch changes (i.e. another embedding operation
    /// starts, or `stop_embedding_sampler` is called on completion).
    fn spawn_embedding_sampler(&mut self, base_status: String, cx: &mut Context<Self>) {
        self.embedding_sampler_epoch = self.embedding_sampler_epoch.wrapping_add(1);
        let epoch = self.embedding_sampler_epoch;
        let queue = self.inference_queue.clone();
        let hq_queue = self.hq_queue.clone();

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(500))
                    .await;
                let Some(this) = this.upgrade() else { return };
                let pending = queue.as_ref().map(|q| q.stats().pending_embeddings).unwrap_or(0)
                    + hq_queue
                        .as_ref()
                        .map(|q| q.stats().pending_embeddings)
                        .unwrap_or(0);
                let keep_running = this
                    .update(cx, |view: &mut AppView, cx| {
                        if view.embedding_sampler_epoch != epoch {
                            return false;
                        }
                        view.embedding_status = if pending > 0 {
                            Some(format!("{base_status} ({pending} pending)"))
                        } else {
                            Some(base_status.clone())
                        };
                        cx.notify();
                        true
                    })
                    .ok();
                if keep_running != Some(true) {
                    return;
                }
            }
        })
        .detach();
    }

    /// Cancel the embedding-status sampler by bumping its epoch.
    /// Callers then write the final status string directly.
    fn stop_embedding_sampler(&mut self) {
        self.embedding_sampler_epoch = self.embedding_sampler_epoch.wrapping_add(1);
    }

    /// Asynchronously re-chunk and embed a set of nodes (standard + optional HQ).
    ///
    /// Used by [`do_save`] to keep embeddings in sync with edited node content.
    /// Each node's old chunks are deleted, fresh chunks are created from the
    /// flattened metadata, and embeddings are computed before the task completes.
    fn do_rechunk_and_embed(&mut self, node_ids: Vec<ObjectId>, cx: &mut Context<Self>) {
        let queue = match self.inference_queue.clone() {
            Some(q) => q,
            None => return, // No embedding queue yet — nothing to do.
        };
        let hq_queue = self.hq_queue.clone();
        let graph = self.graph.clone();
        let tokio_rt = self.tokio_rt.clone();
        let count = node_ids.len();

        let base_status = format!("Re-embedding {count} node(s)…");
        self.embedding_status = Some(base_status.clone());
        cx.notify();
        self.spawn_embedding_sampler(base_status, cx);

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    tokio_rt.block_on(async {
                        let hq_ref = hq_queue.as_ref();
                        let mut total_chunks = 0usize;
                        let mut errors = 0usize;
                        for oid in &node_ids {
                            match rechunk_and_embed(&graph, &queue, hq_ref, *oid).await {
                                Ok(n) => total_chunks += n,
                                Err(e) => {
                                    eprintln!("rechunk_and_embed({oid}): {e:#}");
                                    errors += 1;
                                }
                            }
                        }
                        Ok::<_, anyhow::Error>((total_chunks, errors))
                    })
                })
                .await;

            this.update(cx, |view: &mut AppView, cx| {
                view.stop_embedding_sampler();
                match result {
                    Ok((chunks, 0)) if chunks > 0 => {
                        view.embedding_status = Some(format!("Re-embedded {chunks} chunk(s)"));
                    }
                    Ok((chunks, errs)) if errs > 0 => {
                        view.embedding_status = Some(format!(
                            "Re-embedded {chunks} chunk(s), {errs} node(s) failed"
                        ));
                    }
                    Ok(_) => {
                        view.embedding_status = None;
                    }
                    Err(e) => {
                        eprintln!("Re-embed failed: {e}");
                        view.embedding_status = Some(format!("Re-embed failed: {e}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Asynchronously discover Lemonade Server and build the InferenceQueue + ChatProvider.
    /// FTS5 search works immediately even if this fails.
    pub(crate) fn do_init_lemonade(&mut self, cx: &mut Context<Self>) {
        let app_config = self.app_config.clone();
        let tokio_rt = self.tokio_rt.clone();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    tokio_rt.block_on(async move {
                        // Discover Lemonade Server URL.
                        let url = match resolve_lemonade_url().await {
                            Some(u) => u,
                            None => return Err(anyhow::anyhow!("Lemonade Server not reachable")),
                        };

                        // Discover available models.
                        let catalog = LemonadeServerCatalog::discover(&url).await?;
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

                        Ok((url, queue, hq_queue, chat_provider, llm_available, preferred_idx))
                    })
                })
                .await;

            this.update(cx, |view: &mut AppView, cx| {
                match result {
                    Ok((lemonade_url, queue, hq_queue, chat_provider, llm_models, preferred_idx)) => {
                        eprintln!("Lemonade connected — embedding queue ready");
                        view.inference_queue = Some(queue.clone());
                        view.hq_queue = hq_queue.clone();

                        // Wrap HQ queue in Arc before it's consumed below.
                        let hq_arc = hq_queue.clone().map(Arc::new);

                        // Push queues to search panel.
                        view.search_panel.update(cx, |panel, _cx| {
                            panel.set_queues(Some(queue.clone()), hq_queue);
                        });

                        // Build the graph agent and wire it to the chat panel.
                        let graph = view.graph.clone();
                        let system_prompt = view.app_config.chat.system_prompt.clone();
                        let dev = view.app_config.chat.active_device_config();
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
                            max_tool_turns: view.app_config.chat.max_tool_turns,
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
                        view.do_embed_all(cx);
                    }
                    Err(e) => {
                        eprintln!("Lemonade init skipped: {e}");
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn do_export_data(&mut self, cx: &mut Context<Self>) {
        let graph = self.graph.clone();
        let data_file = self.data_file.clone();

        self.data_status = Some("Exporting…".to_string());
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
                        view.data_status = Some(format!(
                            "Exported {node_count} nodes, {edge_count} edges → {filename}"
                        ));
                    }
                    Err(e) => {
                        view.data_status = Some(format!("Export failed: {e}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Trigger bulk embedding of all unembedded chunks (standard + optional HQ).
    pub(crate) fn do_embed_all(&mut self, cx: &mut Context<Self>) {
        let queue = match self.inference_queue.clone() {
            Some(q) => q,
            None => return,
        };
        let hq_queue = self.hq_queue.clone();
        let graph = self.graph.clone();
        let tokio_rt = self.tokio_rt.clone();

        let base_status = "Embedding…".to_string();
        self.embedding_status = Some(base_status.clone());
        cx.notify();
        self.spawn_embedding_sampler(base_status, cx);

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    tokio_rt.block_on(async move {
                        let std_result =
                            embed_all_chunks(&graph, &queue, EmbeddingTarget::Standard).await?;

                        let hq_result = if let Some(hq) = &hq_queue {
                            let r =
                                embed_all_chunks(&graph, hq, EmbeddingTarget::HighQuality).await?;
                            Some(r)
                        } else {
                            None
                        };

                        Ok::<_, anyhow::Error>((std_result, hq_result))
                    })
                })
                .await;

            this.update(cx, |view: &mut AppView, cx| {
                view.stop_embedding_sampler();
                match result {
                    Ok((std_r, hq_r)) => {
                        // Only report if something was actually embedded.
                        // total == 0 means no unembedded chunks existed — nothing to do.
                        view.embedding_status = if std_r.total == 0 {
                            None
                        } else if std_r.stored > 0 {
                            let hq_suffix = hq_r
                                .filter(|hq| hq.stored > 0)
                                .map(|hq| format!(" (+{} HQ)", hq.stored))
                                .unwrap_or_default();
                            Some(format!("Embedded {}{} chunks", std_r.stored, hq_suffix))
                        } else {
                            // Had candidates but none stored (all failed).
                            Some(format!(
                                "Embedding: {}/{} failed",
                                std_r.skipped, std_r.total
                            ))
                        };
                    }
                    Err(e) => {
                        eprintln!("Embedding failed: {e}");
                        view.embedding_status = Some(format!("Embedding failed: {e}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}
