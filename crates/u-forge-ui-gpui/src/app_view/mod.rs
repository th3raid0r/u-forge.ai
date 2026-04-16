mod render;

use std::sync::Arc;

use gpui::{prelude::*, Context, Empty, Entity};
use parking_lot::RwLock;
use u_forge_core::{
    AppConfig, EmbeddingTarget, KnowledgeGraph, SchemaManager,
    embed_all_chunks,
    lemonade::{
        Capability, GpuResourceManager, LemonadeServerCatalog, ProviderFactory,
        resolve_lemonade_url, ModelSelector, QualityTier,
    },
    queue::{InferenceQueue, InferenceQueueBuilder},
    ingest::build_hq_embed_queue,
};
use u_forge_graph_view::GraphSnapshot;

use crate::chat_panel::{AvailableModel, ChatPanel};
use crate::graph_canvas::GraphCanvas;
use crate::node_editor::NodeEditorPanel;
use crate::search_panel::SearchPanel;
use crate::selection_model::SelectionModel;
use crate::tree_panel::TreePanel;

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
    Tree,
    Search,
}

pub struct AppView {
    pub(crate) graph_canvas: Entity<GraphCanvas>,
    pub(crate) tree_panel: Entity<TreePanel>,
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
}

impl AppView {
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
        let graph_canvas = cx.new(|cx| {
            GraphCanvas::new(snapshot_arc.clone(), graph.clone(), selection.clone(), cx)
        });
        let tree_panel =
            cx.new(|_cx| TreePanel::new(snapshot_arc.clone(), selection.clone()));
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
        let chat_panel = cx.new(|cx| {
            ChatPanel::new(
                app_config.chat.system_prompt.clone(),
                app_config.chat.max_history_turns,
                tokio_rt.clone(),
                cx,
            )
        });

        let mut view = Self {
            graph_canvas,
            tree_panel,
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
            sidebar_tab: SidebarTab::Tree,
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
        };

        view.do_init_lemonade(cx);
        view
    }

    /// Rebuild the in-memory snapshot from the graph and push it to all child views.
    pub(crate) fn refresh_snapshot(&mut self, cx: &mut Context<Self>) {
        match u_forge_graph_view::build_snapshot(&self.graph) {
            Ok(snap) => {
                *self.snapshot.write() = snap;
                self.tree_panel.update(cx, |panel, cx| panel.refresh_groups(cx));
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

                        let already_loaded: Vec<String> =
                            catalog.loaded.iter().map(|m| m.model_name.clone()).collect();

                        // Build provider specs for embedding + optional reranker.
                        let mut build_futs = Vec::new();
                        for sel in embed_models.iter().filter(|s| s.quality_tier == QualityTier::Standard) {
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
                            build_futs.push((
                                sel.clone(),
                                Capability::Embedding,
                                weight,
                            ));
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

                        // Select LLM models and build a chat provider for the first one.
                        let llm_models = selector.select_llm_models();
                        let llm_available: Vec<AvailableModel> =
                            llm_models.iter().map(AvailableModel::from).collect();
                        let chat_provider = llm_models.first().map(|sel| {
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

                        Ok((queue, hq_queue, chat_provider, llm_available))
                    })
                })
                .await;

            this.update(cx, |view: &mut AppView, cx| {
                match result {
                    Ok((queue, hq_queue, chat_provider, llm_models)) => {
                        eprintln!("Lemonade connected — embedding queue ready");
                        view.inference_queue = Some(queue.clone());
                        view.hq_queue = hq_queue.clone();

                        // Push queues to search panel.
                        view.search_panel.update(cx, |panel, _cx| {
                            panel.set_queues(Some(queue), hq_queue);
                        });

                        // Push chat provider to chat panel.
                        if let Some(provider) = chat_provider {
                            view.chat_panel.update(cx, |panel, _cx| {
                                panel.set_provider(provider, llm_models);
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

    /// Trigger bulk embedding of all unembedded chunks (standard + optional HQ).
    pub(crate) fn do_embed_all(&mut self, cx: &mut Context<Self>) {
        let queue = match self.inference_queue.clone() {
            Some(q) => q,
            None => return,
        };
        let hq_queue = self.hq_queue.clone();
        let graph = self.graph.clone();
        let tokio_rt = self.tokio_rt.clone();

        self.embedding_status = Some("Embedding…".to_string());
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    tokio_rt.block_on(async move {
                        let std_result =
                            embed_all_chunks(&graph, &queue, EmbeddingTarget::Standard).await?;

                        let hq_result = if let Some(hq) = &hq_queue {
                            let r = embed_all_chunks(&graph, hq, EmbeddingTarget::HighQuality).await?;
                            Some(r)
                        } else {
                            None
                        };

                        Ok::<_, anyhow::Error>((std_result, hq_result))
                    })
                })
                .await;

            this.update(cx, |view: &mut AppView, cx| {
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
