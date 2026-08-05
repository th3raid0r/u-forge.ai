use std::sync::Arc;

use parking_lot::RwLock;
use u_forge_core::{
    AppConfig, KnowledgeGraph,
    lemonade::{EmbeddedLemonade, LemonadeConnection, LemonadeServerCatalog},
    queue::InferenceQueue,
};
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
    /// Owned private Lemonade process; absent for explicit external servers.
    pub(crate) embedded_lemonade: Option<Arc<EmbeddedLemonade>>,
    /// Shared runtime connection used by inference and the reopenable setup flow.
    pub(crate) lemonade_connection: Option<Arc<LemonadeConnection>>,
    /// Last live catalog snapshot used to explain setup/readiness state.
    pub(crate) lemonade_catalog: Option<LemonadeServerCatalog>,
    /// True when at least one non-default schema is present in the graph DB.
    pub(crate) schema_loaded: bool,
    /// Status message displayed in the status bar during/after data operations.
    pub(crate) data_status: Option<String>,
    /// Embedding progress/completion message shown in the status bar.
    pub(crate) embedding_status: Option<String>,
    /// Single authority for which embedding plan may update UI progress.
    pub(crate) embedding_plan: EmbeddingPlanAuthority,
}

#[derive(Debug, Default)]
pub(crate) struct EmbeddingPlanAuthority {
    generation: u64,
    active: bool,
}

impl EmbeddingPlanAuthority {
    /// Start a plan, returning its generation and whether older work remains
    /// active in the queue.
    pub(crate) fn start(&mut self) -> (u64, bool) {
        let superseded = self.active;
        self.generation = self.generation.wrapping_add(1);
        self.active = true;
        (self.generation, superseded)
    }

    pub(crate) fn is_current(&self, generation: u64) -> bool {
        self.active && self.generation == generation
    }

    pub(crate) fn finish(&mut self, generation: u64) -> bool {
        if !self.is_current(generation) {
            return false;
        }
        self.active = false;
        true
    }
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
        let schema_loaded = graph
            .get_schema_manager()
            .list_schemas()
            .map(|names| names.iter().any(|n| n != "default"))
            .unwrap_or(false);
        Self {
            graph,
            snapshot,
            data_file,
            schema_dir,
            app_config,
            tokio_rt,
            schema_loaded,
            inference_queue: None,
            hq_queue: None,
            embedded_lemonade: None,
            lemonade_connection: None,
            lemonade_catalog: None,
            data_status: None,
            embedding_status: None,
            embedding_plan: EmbeddingPlanAuthority::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EmbeddingPlanAuthority;

    #[test]
    fn embedding_plan_authority_rejects_superseded_updates() {
        let mut authority = EmbeddingPlanAuthority::default();
        let (first, superseded) = authority.start();
        assert!(!superseded);
        assert!(authority.is_current(first));

        let (second, superseded) = authority.start();
        assert!(superseded);
        assert!(!authority.is_current(first));
        assert!(authority.is_current(second));
        assert!(!authority.finish(first));
        assert!(authority.finish(second));
        assert!(!authority.is_current(second));
    }
}
