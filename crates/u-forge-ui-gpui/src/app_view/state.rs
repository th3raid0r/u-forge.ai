use std::sync::Arc;

use parking_lot::RwLock;
use u_forge_core::{queue::InferenceQueue, AppConfig, KnowledgeGraph};
use u_forge_graph_view::GraphSnapshot;

/// Non-render application state owned by [`super::AppView`].
///
/// All fields here are free of GPUI types — no `Entity`, no `Context`, no
/// `Subscription`. That boundary makes this struct testable in isolation and
/// gives future frontends (web, embedded TS sandbox) a seam to reuse without
/// pulling in the GPUI render layer.
pub(crate) struct AppState {
    pub(crate) graph: Arc<KnowledgeGraph>,
    pub(crate) snapshot: Arc<RwLock<GraphSnapshot>>,
    pub(crate) data_file: std::path::PathBuf,
    pub(crate) schema_dir: std::path::PathBuf,
    pub(crate) app_config: Arc<AppConfig>,
    pub(crate) tokio_rt: Arc<tokio::runtime::Runtime>,
    /// Standard embedding + reranking queue (None until Lemonade is discovered).
    pub(crate) inference_queue: Option<InferenceQueue>,
    /// High-quality embedding queue (None when HQ embedding is disabled or unavailable).
    pub(crate) hq_queue: Option<InferenceQueue>,
    /// Status message displayed in the status bar during/after data operations.
    pub(crate) data_status: Option<String>,
    /// Embedding progress/completion message shown in the status bar.
    pub(crate) embedding_status: Option<String>,
    /// Epoch for the embedding-plan status poller. Bumping cancels any
    /// in-flight poller timer so stale ticks don't overwrite a fresh status.
    pub(crate) embedding_plan_epoch: usize,
}

impl AppState {
    pub(crate) fn new(
        graph: Arc<KnowledgeGraph>,
        snapshot: Arc<RwLock<GraphSnapshot>>,
        data_file: std::path::PathBuf,
        schema_dir: std::path::PathBuf,
        app_config: Arc<AppConfig>,
        tokio_rt: Arc<tokio::runtime::Runtime>,
    ) -> Self {
        Self {
            graph,
            snapshot,
            data_file,
            schema_dir,
            app_config,
            tokio_rt,
            inference_queue: None,
            hq_queue: None,
            data_status: None,
            embedding_status: None,
            embedding_plan_epoch: 0,
        }
    }
}
