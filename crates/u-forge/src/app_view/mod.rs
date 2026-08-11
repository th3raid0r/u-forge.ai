mod render;
mod state;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    App, Context, Empty, Entity, FocusHandle, Focusable, Pixels, Point, Subscription, Window,
    point, prelude::*, px,
};
use parking_lot::RwLock;
use tracing::Instrument;
use u_forge_agent::{AgentParams, GraphAgent};
use u_forge_core::{
    AppConfig, EmbeddingOutcome, EmbeddingPlan, EmbeddingProgress, KnowledgeGraph, ObjectMetadata,
    SchemaManager,
    ingest::build_hq_embed_queue_with_connection,
    lemonade::{
        Capability, EffectiveChatLimits, EmbeddedLemonade, GpuResourceManager,
        LemonadeChatProvider, LemonadeConnection, LemonadeManagement, LemonadeOwnership,
        LemonadeRuntime, LemonadeServerCatalog, ManagementEventKind, ManagementProgressEvent,
        ModelSelector, ProviderFactory, QualityTier, SetupRole, chat_component_state,
        component_state, initial_setup_components, resolve_runtime_connection,
        select_setup_backend,
    },
    queue::{CancellationToken, InferenceQueueBuilder},
    types::ObjectId,
};
use u_forge_graph_view::GraphSnapshot;

use state::AppState;

use crate::actions::{ActionContext, native_menus};
use crate::chat_panel::{
    AvailableModel, ChatPanel, ConnectRequested, ToggleAssistantZoomRequested,
};
use crate::confirmation_modal::{
    ConfirmationAccepted, ConfirmationAlternative, ConfirmationCancelled, ConfirmationModal,
};
use crate::dock_state::{DockFocusIntent, DockState};
use crate::graph_canvas::GraphCanvas;
use crate::node_editor::{CloseDirtyTabRequested, NodeEditorPanel};
use crate::node_panel::{CreateNodeRequest, DeleteNodeRequest, NodePanel};
use crate::panel_contracts::{PanelId, WorldCanvasViewId};
use crate::path_picker::{
    PathCancelled, PathConfirmed, PathPickerKind, PathPickerModal, PickerMode,
};
use crate::search_panel::SearchPanel;
use crate::selection_model::SelectionModel;
use crate::settings_view::{SettingsRebuildRequested, SettingsSaveRequested, SettingsView};
use crate::setup_panel::{
    SetupBackendInstallRequested, SetupClosed, SetupDownloadOperation, SetupDownloadRequested,
    SetupPanel, SetupRefreshRequested, SetupRequested,
};
use crate::startup::{LEMONADE_METADATA_READY_MESSAGE, StartupMilestone, StartupTimeline};
use crate::ui::theme::UiTheme;
use crate::window_chrome::WindowControlFocusHandles;

// ── Root app view ─────────────────────────────────────────────────────────────

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
    pub(crate) setup_panel: Entity<SetupPanel>,
    /// Single-flight lifecycle for Lemonade discovery and capability loading.
    lemonade_init_state: LemonadeInitState,
    /// Rejects late UI writes from superseded discovery attempts.
    lemonade_init_generation: u64,
    /// Process-wide launch clock shared with startup work and paint callbacks.
    pub(crate) startup: StartupTimeline,
    #[allow(dead_code)]
    pub(crate) selection: Entity<SelectionModel>,
    // ── UI layout state ───────────────────────────────────────────────────────
    pub(crate) file_menu_open: bool,
    pub(crate) view_menu_open: bool,
    pub(crate) file_menu_button_focus: FocusHandle,
    pub(crate) view_menu_button_focus: FocusHandle,
    pub(crate) file_menu_focus: FocusHandle,
    pub(crate) view_menu_focus: FocusHandle,
    /// Bottom-left window coordinates measured from the File/View buttons.
    pub(crate) menu_anchors: Rc<Cell<[Point<Pixels>; 2]>>,
    pub(crate) window_control_focus: WindowControlFocusHandles,
    pub(crate) setup_open: bool,
    pub(crate) active_world_canvas_view: WorldCanvasViewId,
    pub(crate) settings_view: Option<Entity<SettingsView>>,
    settings_close_after_save: bool,
    _settings_subs: Vec<Subscription>,
    pub(crate) ui_font_size: f32,
    pub(crate) ui_interface_size: f32,
    pub(crate) show_advanced_controls: bool,
    pub(crate) window_controls_left: bool,
    pub(crate) dock_state: DockState,
    /// Last focused descendant per workspace region, used when a dock is
    /// revisited after F6 traversal or a close/reopen cycle.
    last_region_focus: HashMap<FocusRegion, FocusHandle>,
    last_selected_panel: Option<PanelId>,
    workspace_state_path: std::path::PathBuf,
    workspace_persist_task: Option<gpui::Task<()>>,
    /// Owns the user-initiated import so replacement and shutdown are explicit.
    import_cancellation: Option<CancellationToken>,
    import_task: Option<gpui::Task<()>>,
    import_generation: u64,
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
    /// Ensures owned Lemonade cleanup is awaited on an ordinary application quit.
    _app_quit_sub: Subscription,
    /// Waits for Ctrl-C on the Tokio runtime once an embedded server is active.
    _lemonade_signal_task: Option<tokio::task::JoinHandle<()>>,
    /// Coalesces core mutation events into incremental graph refreshes.
    _graph_change_task: Option<gpui::Task<()>>,
    /// Delivers Lemonade backend/model SSE progress independently of setup UI.
    _management_event_task: Option<gpui::Task<()>>,
    /// Targets with an active client-owned transfer, used by the exit guard.
    active_management_operations: HashSet<String>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum FocusRegion {
    Panel(PanelId),
    WorldCanvas,
}

fn next_focus_index(current: Option<usize>, len: usize, reverse: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match (current, reverse) {
        (Some(0), true) | (None, true) => len - 1,
        (Some(index), true) => index - 1,
        (Some(index), false) => (index + 1) % len,
        (None, false) => 0,
    })
}

#[derive(Clone)]
struct LemonadeMetadata {
    connection: Arc<LemonadeConnection>,
    embedded: Option<Arc<EmbeddedLemonade>>,
    catalog: LemonadeServerCatalog,
    downloads: Result<serde_json::Value, String>,
}

struct LemonadeActivation {
    queue: u_forge_core::queue::InferenceQueue,
    hq_queue: Option<u_forge_core::queue::InferenceQueue>,
    chat_provider: Option<LemonadeChatProvider>,
    llm_models: Vec<AvailableModel>,
    preferred_idx: usize,
    runtime: Arc<LemonadeRuntime>,
    effective_limits: Option<EffectiveChatLimits>,
}

struct LemonadeChatActivation {
    provider: Option<LemonadeChatProvider>,
    models: Vec<AvailableModel>,
    preferred_idx: usize,
    runtime: Arc<LemonadeRuntime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LemonadeInitState {
    Offline,
    Discovering,
    CapabilitiesLoading,
    Ready,
    Degraded,
    Failed,
}

async fn discover_lemonade_metadata(
    existing_connection: Option<Arc<LemonadeConnection>>,
    existing_embedded: Option<Arc<EmbeddedLemonade>>,
    max_loaded_models: usize,
    startup: StartupTimeline,
) -> anyhow::Result<LemonadeMetadata> {
    let (connection, embedded) = {
        let _phase = startup.phase("lemonade_connection_resolve");
        match existing_connection {
            Some(connection) => (connection, existing_embedded),
            None => resolve_runtime_connection().await?,
        }
    };
    tracing::debug!(url = %connection.api_base(), "Lemonade server reachable");

    if connection.ownership() == LemonadeOwnership::Embedded {
        let changed = LemonadeManagement::new(connection.clone())
            .set_max_loaded_models(max_loaded_models, false)
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "could not configure embedded Lemonade max_loaded_models={max_loaded_models}: {error}"
                )
            })?;
        tracing::debug!(
            max_loaded_models,
            changed,
            "Embedded Lemonade residency limit is configured"
        );
    }

    let catalog_connection = connection.clone();
    let downloads_connection = connection.clone();
    let catalog_timeline = startup.clone();
    let downloads_timeline = startup.clone();
    let (catalog, downloads) = tokio::join!(
        async move {
            let _phase = catalog_timeline.phase("lemonade_catalog_discovery");
            LemonadeServerCatalog::discover_with_connection(catalog_connection).await
        },
        async move {
            let _phase = downloads_timeline.phase("lemonade_downloads_query");
            LemonadeManagement::new(downloads_connection)
                .downloads()
                .await
                .map_err(|error| error.to_string())
        }
    );
    let catalog = catalog?;
    tracing::debug!(
        loaded = catalog.loaded.len(),
        models = catalog.models.len(),
        "Lemonade metadata fetched"
    );
    Ok(LemonadeMetadata {
        connection,
        embedded,
        catalog,
        downloads,
    })
}

async fn forward_management_events(
    mut receiver: u_forge_core::lemonade::ManagementProgressReceiver,
    events: &tokio::sync::broadcast::Sender<ManagementProgressEvent>,
) -> anyhow::Result<()> {
    let mut latest: Option<ManagementProgressEvent> = None;
    while let Some(event) = receiver.recv().await {
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                if let Some(mut failed_event) = latest {
                    failed_event.kind = ManagementEventKind::Failed;
                    failed_event.message = Some(error.to_string());
                    let _ = events.send(failed_event);
                }
                return Err(error);
            }
        };
        let terminal = event.is_terminal();
        let failed = event.kind == u_forge_core::lemonade::ManagementEventKind::Failed;
        let message = event.message.clone();
        latest = Some(event.clone());
        let _ = events.send(event);
        if failed {
            anyhow::bail!(message.unwrap_or_else(|| "Lemonade management operation failed".into()));
        }
        if terminal {
            return Ok(());
        }
    }
    anyhow::bail!("Lemonade management stream closed without completion")
}

/// Ensure the small CPU retrieval baseline for the runtime owned by u-forge.
/// Optional accelerators and chat models remain explicit setup choices.
async fn provision_managed_baseline(
    connection: Arc<LemonadeConnection>,
    catalog: LemonadeServerCatalog,
    events: tokio::sync::broadcast::Sender<ManagementProgressEvent>,
) -> anyhow::Result<bool> {
    if connection.ownership() != LemonadeOwnership::Embedded {
        return Ok(false);
    }
    let manager = LemonadeManagement::new(connection);
    let mut changed = false;
    let cpu = catalog
        .backends
        .iter()
        .find(|backend| backend.recipe == "llamacpp" && backend.backend == "cpu")
        .ok_or_else(|| {
            anyhow::anyhow!("Lemonade did not report an installable llama.cpp CPU backend")
        })?;
    if cpu.state != "installed" {
        let receiver = manager
            .install_backend_stream("llamacpp", "cpu", true)
            .await?;
        forward_management_events(receiver, &events).await?;
        changed = true;
    }

    for component in initial_setup_components().into_iter().filter(|component| {
        matches!(
            component.role,
            SetupRole::StandardEmbedding | SetupRole::Reranking
        )
    }) {
        let state = component_state(&catalog, &component);
        if let u_forge_core::lemonade::SetupComponentState::Conflict(message) = state {
            anyhow::bail!(message);
        }
        if state.needs_pull() {
            let pull = component.pull_spec();
            let receiver = manager
                .pull_stream(
                    pull.model_name,
                    pull.checkpoint,
                    pull.recipe,
                    pull.embedding,
                    true,
                )
                .await?;
            forward_management_events(receiver, &events).await?;
            changed = true;
        }
    }
    Ok(changed)
}

/// Build chat-facing catalog/profile state without loading a model. This is
/// deliberately synchronous so the Assistant chrome is usable as soon as
/// metadata arrives, while embedding and reranking providers continue loading.
fn prepare_lemonade_chat(
    connection: Arc<LemonadeConnection>,
    catalog: &LemonadeServerCatalog,
    app_config: &AppConfig,
) -> LemonadeChatActivation {
    let selector = ModelSelector::new(catalog, &app_config.models, &app_config.embedding);
    let all_llm = selector.select_all_llm_models();
    let preferred_model_id = app_config.chat.active_device_config().model.clone();
    let (preferred_idx, selection_diagnostic) = select_preferred_llm_index(
        &all_llm,
        app_config.chat.preferred_device.clone(),
        preferred_model_id.as_deref(),
    );
    let models = all_llm
        .iter()
        .enumerate()
        .map(|(index, selected)| {
            let device_config = chat_device_config_for_model(&app_config.chat, selected).clone();
            let configured_generation = device_config
                .max_tokens
                .map(|value| value as usize)
                .unwrap_or(app_config.chat.response_reserve);
            let mut effective_limits = selected
                .reconcile_chat_limits(
                    app_config.chat.max_context_tokens,
                    app_config.chat.response_reserve,
                    configured_generation,
                    configured_generation,
                )
                .map_err(|error| tracing::warn!(%error, "chat context is unusable"))
                .ok();
            let context = effective_limits
                .as_ref()
                .map_or(app_config.chat.max_context_tokens, |limits| limits.context)
                .max(2);
            let reserve = effective_limits.as_ref().map_or_else(
                || {
                    app_config
                        .chat
                        .response_reserve
                        .min(context.saturating_sub(1))
                },
                |limits| limits.response_reserve,
            );
            let (agent_budget, invalid_agent_budget) = match app_config.chat.agent.reconcile(
                context,
                reserve,
                app_config.chat.max_tool_turns,
            ) {
                Ok(budget) => (budget, None),
                Err(error) => {
                    tracing::warn!(%error, "agent budget configuration is unusable");
                    let fallback = u_forge_core::AgentBudgetConfig::default()
                        .reconcile(context, reserve, app_config.chat.max_tool_turns)
                        .expect("safe fallback agent budget reconciles");
                    (
                        fallback,
                        Some(format!(
                            "invalid agent budget configuration ({error}); safe defaults applied"
                        )),
                    )
                }
            };
            if let Some(limits) = &mut effective_limits {
                limits.diagnostics.extend(agent_budget.diagnostics.clone());
                limits.diagnostics.extend(invalid_agent_budget);
            }
            if index == preferred_idx
                && let (Some(limits), Some(diagnostic)) =
                    (&mut effective_limits, &selection_diagnostic)
            {
                limits.diagnostics.push(diagnostic.clone());
            }
            AvailableModel::from(selected).with_chat_profile(
                device_config,
                effective_limits,
                app_config.chat.max_tool_turns,
                agent_budget,
            )
        })
        .collect::<Vec<_>>();
    let gpu_manager = GpuResourceManager::new();
    let provider = all_llm.get(preferred_idx).map(|selected| {
        let gpu = all_llm
            .iter()
            .any(|model| selected_model_device(model) == "gpu")
            .then(|| Arc::clone(&gpu_manager));
        LemonadeChatProvider::from_connection(connection.clone(), &selected.model_id, gpu)
    });
    LemonadeChatActivation {
        provider,
        models,
        preferred_idx,
        runtime: Arc::new(LemonadeRuntime::from_connection(connection)),
    }
}

async fn activate_lemonade_capabilities(
    connection: Arc<LemonadeConnection>,
    catalog: LemonadeServerCatalog,
    app_config: Arc<AppConfig>,
    startup: StartupTimeline,
) -> anyhow::Result<LemonadeActivation> {
    let selector = {
        let _phase = startup.phase("lemonade_model_selection");
        ModelSelector::new(&catalog, &app_config.models, &app_config.embedding)
    };
    let embed_models = selector.select_embedding_models();
    let reranker_sel = selector.select_reranker();
    let already_loaded: Vec<String> = catalog
        .loaded
        .iter()
        .map(|model| model.model_name.clone())
        .collect();

    let mut build_specs = Vec::new();
    // HQ is an additive retrieval lane, never a replacement for the standard
    // lane. Even a one-slot server must attempt the standard provider first;
    // HQ may then degrade away if the server cannot host both models.
    for selected in standard_embedding_models(&embed_models) {
        let weight = match selected.recipe.as_str() {
            "flm" => app_config.embedding.npu_weight,
            "llamacpp" => match selected.backend.as_deref() {
                Some("rocm" | "vulkan" | "metal") => app_config.embedding.gpu_weight,
                _ => app_config.embedding.cpu_weight,
            },
            _ => app_config.embedding.cpu_weight,
        };
        build_specs.push((selected.clone(), Capability::Embedding, weight));
    }
    if let Some(selected) = reranker_sel {
        build_specs.push((selected, Capability::Reranking, 100));
    }

    let gpu_manager = GpuResourceManager::new();
    let mut providers = Vec::new();
    // Lemonade's residency limit is per model type and may be one. Build every
    // standard embedding provider in selection order before HQ construction so
    // provider probes never race each other for that single embedding slot.
    for (selected, capability, weight) in build_specs {
        let result = {
            let _phase = startup.phase(format!(
                "provider_build.{capability:?}.{}",
                selected.model_id
            ));
            ProviderFactory::build_with_connection(
                &selected,
                capability,
                connection.clone(),
                weight,
                Some(gpu_manager.clone()),
                &already_loaded,
            )
            .await
        };
        match result {
            Ok(provider) => providers.push(provider),
            Err(error) => {
                tracing::warn!(%error, "Lemonade capability provider unavailable");
            }
        }
    }

    let queue = {
        let _phase = startup.phase("standard_inference_queue_build");
        InferenceQueueBuilder::new()
            .with_providers(providers)
            .with_config((*app_config).clone())
            .build()
    };
    tracing::debug!(
        embedding_workers = queue.embedding_worker_count(),
        "Standard inference queue ready"
    );

    let hq_queue = if queue.has_embedding() {
        let _phase = startup.phase("hq_inference_queue_build");
        build_hq_embed_queue_with_connection(&catalog, &app_config, connection.clone()).await
    } else {
        tracing::warn!(
            "HQ embedding lane skipped because no standard embedding provider is available"
        );
        None
    };

    let all_llm = selector.select_all_llm_models();
    let preferred_model_id = app_config.chat.active_device_config().model.clone();
    let (preferred_idx, selection_diagnostic) = select_preferred_llm_index(
        &all_llm,
        app_config.chat.preferred_device.clone(),
        preferred_model_id.as_deref(),
    );
    let llm_models = all_llm
        .iter()
        .enumerate()
        .map(|(index, selected)| {
            let device_config = chat_device_config_for_model(&app_config.chat, selected).clone();
            let configured_generation = device_config
                .max_tokens
                .map(|value| value as usize)
                .unwrap_or(app_config.chat.response_reserve);
            let mut effective_limits = selected
                .reconcile_chat_limits(
                    app_config.chat.max_context_tokens,
                    app_config.chat.response_reserve,
                    configured_generation,
                    configured_generation,
                )
                .map_err(|error| tracing::warn!(%error, "chat context is unusable"))
                .ok();
            let context = effective_limits
                .as_ref()
                .map_or(app_config.chat.max_context_tokens, |limits| limits.context)
                .max(2);
            let reserve = effective_limits.as_ref().map_or_else(
                || {
                    app_config
                        .chat
                        .response_reserve
                        .min(context.saturating_sub(1))
                },
                |limits| limits.response_reserve,
            );
            let (agent_budget, invalid_agent_budget) = match app_config.chat.agent.reconcile(
                context,
                reserve,
                app_config.chat.max_tool_turns,
            ) {
                Ok(budget) => (budget, None),
                Err(error) => {
                    tracing::warn!(%error, "agent budget configuration is unusable");
                    let fallback = u_forge_core::AgentBudgetConfig::default()
                        .reconcile(context, reserve, app_config.chat.max_tool_turns)
                        .expect("safe fallback agent budget reconciles");
                    (
                        fallback,
                        Some(format!(
                            "invalid agent budget configuration ({error}); safe defaults applied"
                        )),
                    )
                }
            };
            if let Some(limits) = &mut effective_limits {
                limits.diagnostics.extend(agent_budget.diagnostics.clone());
                limits.diagnostics.extend(invalid_agent_budget);
            }
            if index == preferred_idx
                && let (Some(limits), Some(diagnostic)) =
                    (&mut effective_limits, &selection_diagnostic)
            {
                limits.diagnostics.push(diagnostic.clone());
            }
            AvailableModel::from(selected).with_chat_profile(
                device_config,
                effective_limits,
                app_config.chat.max_tool_turns,
                agent_budget,
            )
        })
        .collect::<Vec<_>>();
    let effective_limits = llm_models
        .get(preferred_idx)
        .and_then(|model| model.effective_limits.clone());
    let chat_provider = all_llm.get(preferred_idx).map(|selected| {
        let gpu = all_llm
            .iter()
            .any(|model| selected_model_device(model) == "gpu")
            .then(|| Arc::clone(&gpu_manager));
        LemonadeChatProvider::from_connection(connection.clone(), &selected.model_id, gpu)
    });
    tracing::debug!(
        llm_count = all_llm.len(),
        preferred_idx,
        "Lemonade capability activation complete"
    );
    Ok(LemonadeActivation {
        queue,
        hq_queue,
        chat_provider,
        llm_models,
        preferred_idx,
        runtime: Arc::new(LemonadeRuntime::from_connection(connection)),
        effective_limits,
    })
}

fn standard_embedding_models(
    models: &[u_forge_core::lemonade::SelectedModel],
) -> impl Iterator<Item = &u_forge_core::lemonade::SelectedModel> {
    models
        .iter()
        .filter(|selected| selected.quality_tier == QualityTier::Standard)
}

impl AppView {
    pub(crate) fn toggle_dock_panel(
        &mut self,
        panel: PanelId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if panel == PanelId::Assistant && self.dock_state.is_panel_active(panel) {
            self.chat_panel
                .update(cx, |chat, _cx| chat.last_render_us = 0);
        }
        let focus = self.dock_state.toggle_panel(panel);
        self.sync_dock_presentational_state(cx);
        self.apply_dock_focus_intent(focus, window, cx);
        self.schedule_workspace_persist(cx);
        cx.notify();
    }

    fn apply_dock_focus_intent(&mut self, intent: DockFocusIntent, window: &mut Window, cx: &App) {
        match intent {
            DockFocusIntent::Panel(panel) => {
                self.focus_region(FocusRegion::Panel(panel), window, cx)
            }
            DockFocusIntent::WorldCanvas => self.focus_region(FocusRegion::WorldCanvas, window, cx),
        }
    }

    pub(crate) fn cycle_workspace_focus(
        &mut self,
        reverse: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let regions = if let Some(zoomed) = self.dock_state.zoomed_panel() {
            vec![FocusRegion::Panel(zoomed)]
        } else {
            let mut regions = Vec::with_capacity(4);
            if self
                .dock_state
                .is_open(crate::panel_contracts::DockPosition::Left)
            {
                regions.push(FocusRegion::Panel(
                    self.dock_state
                        .active_panel(crate::panel_contracts::DockPosition::Left),
                ));
            }
            regions.push(FocusRegion::WorldCanvas);
            if self
                .dock_state
                .is_open(crate::panel_contracts::DockPosition::Bottom)
            {
                regions.push(FocusRegion::Panel(PanelId::Details));
            }
            if self
                .dock_state
                .is_open(crate::panel_contracts::DockPosition::Right)
            {
                regions.push(FocusRegion::Panel(PanelId::Assistant));
            }
            regions
        };
        if regions.is_empty() {
            return;
        }
        let current = regions
            .iter()
            .position(|region| self.region_contains_focus(*region, window, cx));
        let Some(next) = next_focus_index(current, regions.len(), reverse) else {
            return;
        };
        self.focus_region(regions[next], window, cx);
    }

    fn region_contains_focus(&self, region: FocusRegion, window: &Window, cx: &App) -> bool {
        match region {
            FocusRegion::Panel(PanelId::World) => self
                .node_panel
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx),
            FocusRegion::Panel(PanelId::Search) => self
                .search_panel
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx),
            FocusRegion::Panel(PanelId::Assistant) => self
                .chat_panel
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx),
            FocusRegion::Panel(PanelId::Details) => self
                .node_editor
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx),
            FocusRegion::WorldCanvas => self
                .graph_canvas
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx),
        }
    }

    fn region_focus_handle(&self, region: FocusRegion, cx: &App) -> FocusHandle {
        match region {
            FocusRegion::Panel(PanelId::World) => self.node_panel.read(cx).focus_handle(cx),
            FocusRegion::Panel(PanelId::Search) => self.search_panel.read(cx).focus_handle(cx),
            FocusRegion::Panel(PanelId::Assistant) => self.chat_panel.read(cx).focus_handle(cx),
            FocusRegion::Panel(PanelId::Details) => self.node_editor.read(cx).focus_handle(cx),
            FocusRegion::WorldCanvas => self.graph_canvas.read(cx).focus_handle(cx),
        }
    }

    fn remember_current_region_focus(&mut self, window: &Window, cx: &App) {
        let Some(focused) = window.focused(cx) else {
            return;
        };
        for region in [
            FocusRegion::Panel(PanelId::World),
            FocusRegion::Panel(PanelId::Search),
            FocusRegion::WorldCanvas,
            FocusRegion::Panel(PanelId::Details),
            FocusRegion::Panel(PanelId::Assistant),
        ] {
            if self
                .region_focus_handle(region, cx)
                .contains(&focused, window)
            {
                self.last_region_focus.insert(region, focused);
                if let FocusRegion::Panel(panel) = region {
                    self.last_selected_panel = Some(panel);
                }
                return;
            }
        }
    }

    fn focus_region(&mut self, region: FocusRegion, window: &mut Window, cx: &App) {
        self.remember_current_region_focus(window, cx);
        let root_focus = self.region_focus_handle(region, cx);
        if let Some(previous) = self.last_region_focus.get(&region)
            && root_focus.contains(previous, window)
        {
            previous.focus(window);
        } else {
            root_focus.focus(window);
        }
    }

    pub(crate) fn toggle_focused_panel_zoom(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = if self
            .node_panel
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx)
        {
            Some(PanelId::World)
        } else if self
            .search_panel
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx)
        {
            Some(PanelId::Search)
        } else if self
            .chat_panel
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx)
        {
            Some(PanelId::Assistant)
        } else if self
            .node_editor
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx)
        {
            Some(PanelId::Details)
        } else {
            self.last_selected_panel
        };
        if let Some(panel) = panel {
            self.dock_state.toggle_zoom(panel);
            self.sync_dock_presentational_state(cx);
            self.schedule_workspace_persist(cx);
            cx.notify();
        }
    }

    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_view.is_none() {
            let settings = cx.new(|cx| SettingsView::new((*self.state.app_config).clone(), cx));
            let saved = cx.subscribe(
                &settings,
                |this, _settings, event: &SettingsSaveRequested, cx| {
                    this.apply_settings(event.0.clone(), false, cx);
                },
            );
            let rebuild = cx.subscribe(
                &settings,
                |this, _settings, event: &SettingsRebuildRequested, cx| {
                    this.request_rebuild_embedding_index(event.0, cx);
                },
            );
            self.settings_view = Some(settings);
            self._settings_subs = vec![saved, rebuild];
        }
        self.remember_current_region_focus(window, cx);
        self.active_world_canvas_view = WorldCanvasViewId::Settings;
        let focus = self
            .settings_view
            .as_ref()
            .expect("settings entity was just created")
            .read(cx)
            .focus_handle(cx);
        window.defer(cx, move |window, _cx| focus.focus(window));
        cx.notify();
    }

    pub(crate) fn show_connections(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.active_world_canvas_view = WorldCanvasViewId::Connections;
        self.graph_canvas.read(cx).focus_handle(cx).focus(window);
        cx.notify();
    }

    fn close_settings_now(&mut self, cx: &mut Context<Self>) {
        self.settings_view = None;
        self._settings_subs.clear();
        self.settings_close_after_save = false;
        self.active_world_canvas_view = WorldCanvasViewId::Connections;
        cx.notify();
    }

    pub(crate) fn request_close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(settings) = self.settings_view.clone() else {
            return;
        };
        if !settings.read(cx).is_dirty() {
            self.close_settings_now(cx);
            self.graph_canvas.read(cx).focus_handle(cx).focus(window);
            return;
        }

        let return_focus = settings.read(cx).focus_handle(cx);
        let modal = cx.new(|cx| {
            ConfirmationModal::new(
                "Save settings before closing?".to_string(),
                "The Settings tab has unsaved changes.".to_string(),
                "Save Settings".to_string(),
                return_focus,
                cx,
            )
            .with_alternative("Discard")
            .non_destructive()
        });
        let accepted = cx.subscribe(&modal, |this, _modal, _event: &ConfirmationAccepted, cx| {
            this.confirmation = None;
            this._confirmation_subs.clear();
            let Some(settings) = this.settings_view.as_ref() else {
                return;
            };
            this.settings_close_after_save = true;
            let draft = settings.read(cx).draft();
            this.apply_settings(draft, true, cx);
        });
        let discarded = cx.subscribe(
            &modal,
            |this, _modal, _event: &ConfirmationAlternative, cx| {
                this.confirmation = None;
                this._confirmation_subs.clear();
                this.close_settings_now(cx);
            },
        );
        let cancelled = cx.subscribe(
            &modal,
            |this, _modal, _event: &ConfirmationCancelled, cx| {
                this.confirmation = None;
                this._confirmation_subs.clear();
                cx.notify();
            },
        );
        self.confirmation = Some(modal);
        self._confirmation_subs = vec![accepted, discarded, cancelled];
        cx.notify();
    }

    fn apply_settings(
        &mut self,
        mut next_config: AppConfig,
        close_after_save: bool,
        cx: &mut Context<Self>,
    ) {
        next_config.source_path = self.state.app_config.source_path.clone();
        if next_config.lemonade.max_loaded_models == 0 {
            if let Some(settings) = &self.settings_view {
                settings.update(cx, |settings, cx| {
                    settings.set_error(
                        "Lemonade max_loaded_models must be at least 1.".to_string(),
                        cx,
                    )
                });
            }
            return;
        }
        if next_config.chat.response_reserve >= next_config.chat.max_context_tokens {
            if let Some(settings) = &self.settings_view {
                settings.update(cx, |settings, cx| {
                    settings.set_error(
                        "Response reserve must be smaller than the context window.".to_string(),
                        cx,
                    )
                });
            }
            return;
        }
        if let Err(error) = next_config.chat.agent.reconcile(
            next_config.chat.max_context_tokens,
            next_config.chat.response_reserve,
            next_config.chat.max_tool_turns,
        ) {
            if let Some(settings) = &self.settings_view {
                settings.update(cx, |settings, cx| settings.set_error(error.to_string(), cx));
            }
            return;
        }

        let restart_required = next_config.storage.db_path != self.state.app_config.storage.db_path
            || next_config.storage.embedding_dimensions
                != self.state.app_config.storage.embedding_dimensions
            || next_config.storage.high_quality_embedding_dimensions
                != self
                    .state
                    .app_config
                    .storage
                    .high_quality_embedding_dimensions;
        let external_lemonade_limit_changed = next_config.lemonade.max_loaded_models
            != self.state.app_config.lemonade.max_loaded_models
            && self
                .state
                .lemonade_connection
                .as_ref()
                .is_some_and(|connection| connection.ownership() == LemonadeOwnership::External);
        match self.state.app_config.persist_settings(&next_config) {
            Ok(path) => {
                self.ui_font_size = next_config.ui.font_size.clamp(10.0, 28.0);
                self.ui_interface_size = next_config.ui.interface_size.clamp(14.0, 32.0);
                next_config.ui.font_size = self.ui_font_size;
                next_config.ui.interface_size = self.ui_interface_size;
                UiTheme::set_interface_size(cx, self.ui_interface_size);
                self.node_panel.update(cx, |_panel, cx| cx.notify());
                self.search_panel.update(cx, |_panel, cx| cx.notify());
                self.node_editor.update(cx, |_panel, cx| cx.notify());
                self.chat_panel.update(cx, |_panel, cx| cx.notify());
                self.setup_panel.update(cx, |_panel, cx| cx.notify());
                self.show_advanced_controls = next_config.ui.show_advanced_controls;
                self.window_controls_left = next_config.ui.window_controls_left;
                self.state.data_file = next_config.data.import_file.clone();
                self.state.schema_dir = next_config.data.schema_dir.clone();
                let next_config = Arc::new(next_config);
                self.state.app_config = next_config.clone();
                self.search_panel
                    .update(cx, |panel, _cx| panel.set_app_config(next_config.clone()));
                self.refresh_native_menus(cx);
                let message = if external_lemonade_limit_changed {
                    format!(
                        "Settings saved to {}. The external Lemonade server was not changed; manage its max_loaded_models setting on that server.",
                        path.display()
                    )
                } else if restart_required {
                    format!(
                        "Settings saved to {}. Restart and rebuild the semantic index to apply storage changes.",
                        path.display()
                    )
                } else {
                    format!("Settings saved to {}", path.display())
                };
                self.state.data_status = Some(message.clone());
                if let Some(settings) = &self.settings_view {
                    settings.update(cx, |settings, cx| {
                        settings.mark_saved((*next_config).clone(), message, cx)
                    });
                }
                if close_after_save || self.settings_close_after_save {
                    self.close_settings_now(cx);
                }
                self.reconfigure_lemonade(cx);
            }
            Err(error) => {
                let message = format!("Could not save settings: {error}");
                self.state.data_status = Some(message.clone());
                if let Some(settings) = &self.settings_view {
                    settings.update(cx, |settings, cx| settings.set_error(message, cx));
                }
            }
        }
        cx.notify();
    }

    fn request_rebuild_embedding_index(
        &mut self,
        target: u_forge_core::EmbeddingTarget,
        cx: &mut Context<Self>,
    ) {
        let return_focus = self
            .settings_view
            .as_ref()
            .map(|settings| settings.read(cx).focus_handle(cx))
            .unwrap_or_else(|| self.graph_canvas.read(cx).focus_handle(cx));
        let lane = match target {
            u_forge_core::EmbeddingTarget::Standard => "standard",
            u_forge_core::EmbeddingTarget::HighQuality => "high-quality",
        };
        let modal = cx.new(|cx| {
            ConfirmationModal::new(
                "Rebuild semantic index?".to_string(),
                format!(
                    "This clears the regenerable {lane} vectors and rebuilds them with the active embedding model. Nodes, chunks, and keyword search are preserved."
                ),
                "Rebuild Index".to_string(),
                return_focus,
                cx,
            )
            .non_destructive()
        });
        let accepted = cx.subscribe(
            &modal,
            move |this, _modal, _event: &ConfirmationAccepted, cx| {
                this.confirmation = None;
                this._confirmation_subs.clear();
                let provider_available = match target {
                    u_forge_core::EmbeddingTarget::Standard => this
                        .state
                        .inference_queue
                        .as_ref()
                        .is_some_and(|queue| queue.has_embedding()),
                    u_forge_core::EmbeddingTarget::HighQuality => this
                        .state
                        .hq_queue
                        .as_ref()
                        .is_some_and(|queue| queue.has_embedding()),
                };
                if !provider_available {
                    this.state.embedding_status = Some(format!(
                        "Cannot rebuild the {lane} semantic index until its embedding model is available. Check Settings and the Lemonade connection."
                    ));
                    cx.notify();
                    return;
                }
                match this.state.graph.reset_embedding_space(target) {
                    Ok(()) => this.run_embedding_plan(EmbeddingPlan::embed_all(), cx),
                    Err(error) => {
                        this.state.embedding_status =
                            Some(format!("Could not reset {lane} semantic index: {error:#}"));
                        cx.notify();
                    }
                }
            },
        );
        let cancelled = cx.subscribe(
            &modal,
            |this, _modal, _event: &ConfirmationCancelled, cx| {
                this.confirmation = None;
                this._confirmation_subs.clear();
                cx.notify();
            },
        );
        self.confirmation = Some(modal);
        self._confirmation_subs = vec![accepted, cancelled];
        cx.notify();
    }

    pub(crate) fn action_context(&self, cx: &App) -> ActionContext {
        let editor = self.node_editor.read(cx);
        let active_details_dirty = editor
            .active_tab
            .and_then(|active| editor.tabs.get(active))
            .is_some_and(|tab| tab.dirty);
        let any_details_dirty = editor.has_dirty_tabs();
        let has_active_details_tab = editor.active_tab.is_some();
        let details_tab_count = editor.tabs.len();

        let snapshot = self.state.snapshot.read();
        let has_data = !snapshot.nodes.is_empty();
        drop(snapshot);

        let left_open = self
            .dock_state
            .is_open(crate::panel_contracts::DockPosition::Left);
        let active_left = self
            .dock_state
            .active_panel(crate::panel_contracts::DockPosition::Left);
        ActionContext {
            show_advanced_controls: self.show_advanced_controls,
            has_schema: self.state.schema_loaded,
            has_data,
            active_details_dirty,
            any_details_dirty,
            has_active_details_tab,
            details_tab_count,
            world_open: left_open && active_left == PanelId::World,
            search_open: left_open && active_left == PanelId::Search,
            assistant_open: self
                .dock_state
                .is_open(crate::panel_contracts::DockPosition::Right),
            details_open: self
                .dock_state
                .is_open(crate::panel_contracts::DockPosition::Bottom),
            performance_visible: self.perf_enabled,
        }
    }

    pub(crate) fn refresh_native_menus(&self, cx: &mut App) {
        cx.set_menus(native_menus(&self.action_context(cx)));
    }

    pub(crate) fn sync_dock_presentational_state(&mut self, cx: &mut Context<Self>) {
        let assistant_zoomed = self.dock_state.zoomed_panel() == Some(PanelId::Assistant);
        self.chat_panel
            .update(cx, |panel, _cx| panel.set_zoomed(assistant_zoomed));
    }

    pub(crate) fn schedule_workspace_persist(&mut self, cx: &mut Context<Self>) {
        let state = self.dock_state.clone();
        let path = self.workspace_state_path.clone();
        self.workspace_persist_task = Some(cx.spawn(async move |_this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(250))
                .await;
            if let Err(error) = state.save(&path) {
                tracing::warn!(path = %path.display(), %error, "Workspace UI state was not saved");
            }
        }));
    }

    /// Stop user work and synchronously reap the complete owned Lemonade
    /// process group. `EmbeddedLemonade::shutdown` is idempotent, and taking
    /// the handle makes repeated quit/release paths a cheap no-op.
    fn shutdown_for_exit(&mut self) {
        self.state.embedding_plan.cancel();
        if let Some(cancellation) = self.import_cancellation.take() {
            cancellation.cancel();
        }
        if let Some(task) = self.import_task.take() {
            task.detach();
        }
        if let Some(signal_task) = self._lemonade_signal_task.take() {
            signal_task.abort();
        }
        if let Some(embedded) = self.state.embedded_lemonade.take() {
            tracing::info!("Shutting down owned Lemonade process tree");
            self.state.tokio_rt.block_on(embedded.shutdown());
        }
    }

    /// GPUI window-close guard for client-owned management streams. Closing
    /// setup is safe, but exiting the process would terminate active pulls.
    pub fn should_close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.active_management_operations.is_empty() {
            return true;
        }
        if self.confirmation.is_some() {
            return false;
        }
        let count = self.active_management_operations.len();
        let return_focus = window
            .focused(cx)
            .unwrap_or_else(|| self.graph_canvas.read(cx).focus_handle(cx));
        let modal = cx.new(|cx| {
            ConfirmationModal::new(
                "Downloads are still running".to_string(),
                format!(
                    "{count} Lemonade download or backend install operation(s) are still active. Quitting now will stop them."
                ),
                "Quit Anyway".to_string(),
                return_focus,
                cx,
            )
            .with_cancel_label("Stay")
        });
        let accepted = cx.subscribe(&modal, |this, _modal, _event: &ConfirmationAccepted, cx| {
            this.confirmation = None;
            this._confirmation_subs.clear();
            cx.quit();
        });
        let cancelled = cx.subscribe(
            &modal,
            |this, _modal, _event: &ConfirmationCancelled, cx| {
                this.confirmation = None;
                this._confirmation_subs.clear();
                cx.notify();
            },
        );
        self.confirmation = Some(modal);
        self._confirmation_subs = vec![accepted, cancelled];
        cx.notify();
        false
    }

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
        Self::new_profiled(
            snapshot,
            graph,
            schema_mgr,
            data_file,
            schema_dir,
            app_config,
            tokio_rt,
            StartupTimeline::default(),
            None,
            cx,
        )
    }

    /// Production/test construction seam for sharing the launch clock and, in
    /// deterministic tests, bypassing embedded-process discovery with a fake
    /// Lemonade connection.
    #[allow(clippy::too_many_arguments)]
    pub fn new_profiled(
        snapshot: GraphSnapshot,
        graph: Arc<KnowledgeGraph>,
        schema_mgr: Arc<SchemaManager>,
        data_file: std::path::PathBuf,
        schema_dir: std::path::PathBuf,
        app_config: Arc<AppConfig>,
        tokio_rt: Arc<tokio::runtime::Runtime>,
        startup: StartupTimeline,
        initial_lemonade_connection: Option<Arc<LemonadeConnection>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let _phase = startup.phase("app_view_construct");
        let snapshot_arc = Arc::new(RwLock::new(snapshot));
        let workspace_state_path = DockState::state_path(&app_config.storage.db_path);
        let dock_state = DockState::load(&workspace_state_path);
        let ui_font_size = app_config.ui.font_size;
        let ui_interface_size = app_config.ui.interface_size;
        UiTheme::set_interface_size(cx, ui_interface_size);
        let show_advanced_controls = app_config.ui.show_advanced_controls;
        let window_controls_left = app_config.ui.window_controls_left;

        // Build child entities — clone Arc handles before they move into AppState.
        let selection = {
            let _phase = startup.phase("selection_model_construct");
            cx.new(|_cx| SelectionModel::new(snapshot_arc.clone()))
        };
        let graph_canvas = {
            let _phase = startup.phase("graph_canvas_construct");
            cx.new(|cx| {
                GraphCanvas::new(snapshot_arc.clone(), graph.clone(), selection.clone(), cx)
            })
        };
        let node_panel = {
            let _phase = startup.phase("node_panel_construct");
            cx.new(|cx| NodePanel::new(snapshot_arc.clone(), selection.clone(), cx))
        };

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
        let selection_sub = cx.observe(&selection, |this: &mut Self, selection, cx| {
            if selection.read(cx).selected_node_id.is_some() {
                this.dock_state.activate_panel(PanelId::Details);
                this.schedule_workspace_persist(cx);
                cx.notify();
            }
        });
        let search_panel = {
            let _phase = startup.phase("search_panel_construct");
            cx.new(|cx| {
                SearchPanel::new(
                    selection.clone(),
                    graph.clone(),
                    app_config.clone(),
                    tokio_rt.clone(),
                    cx,
                )
            })
        };
        let node_editor = {
            let _phase = startup.phase("node_editor_construct");
            cx.new(|cx| {
                NodeEditorPanel::new(
                    snapshot_arc.clone(),
                    selection.clone(),
                    graph.clone(),
                    schema_mgr,
                    cx,
                )
            })
        };
        let close_dirty_tab_sub = cx.subscribe(
            &node_editor,
            |this: &mut Self, _editor, event: &CloseDirtyTabRequested, cx| {
                this.open_dirty_tab_confirmation(event.0, cx);
            },
        );
        let db_path = app_config.storage.db_path.clone();
        let assistant_zoomed = dock_state.zoomed_panel() == Some(PanelId::Assistant);
        let chat_panel = {
            let _phase = startup.phase("chat_panel_construct");
            cx.new(|cx| {
                ChatPanel::new(
                    app_config.chat.system_prompt.clone(),
                    app_config.chat.max_context_tokens,
                    app_config.chat.response_reserve,
                    &db_path,
                    tokio_rt.clone(),
                    assistant_zoomed,
                    cx,
                )
            })
        };
        let connect_sub = cx.subscribe(
            &chat_panel,
            |this: &mut Self, _panel, _ev: &ConnectRequested, cx| {
                this.chat_panel.update(cx, |panel, _cx| {
                    panel.set_connecting(true);
                });
                this.do_init_lemonade(cx);
            },
        );
        let assistant_zoom_sub = cx.subscribe(
            &chat_panel,
            |this: &mut Self, _panel, _event: &ToggleAssistantZoomRequested, cx| {
                this.dock_state.toggle_zoom(PanelId::Assistant);
                this.sync_dock_presentational_state(cx);
                this.schedule_workspace_persist(cx);
                cx.notify();
            },
        );

        let setup_hq_default = if app_config
            .source_path
            .as_ref()
            .is_some_and(|path| path.exists())
        {
            app_config.embedding.high_quality_embedding
        } else {
            true
        };
        let setup_timeline = startup.clone();
        let setup_panel = cx.new(|_cx| {
            SetupPanel::new(
                LemonadeOwnership::External,
                &LemonadeServerCatalog::default(),
                app_config.chat.active_device_config().model.as_deref(),
                setup_hq_default,
                app_config.embedding.npu_enabled,
                app_config.chat.preferred_device.clone(),
                app_config.chat.reasoning_control,
            )
            .with_startup_timeline(setup_timeline)
        });
        let setup_requested = cx.subscribe(
            &setup_panel,
            |this: &mut Self, _panel, request: &SetupRequested, cx| {
                this.do_provision_lemonade(request.clone(), cx);
            },
        );
        let setup_refresh = cx.subscribe(
            &setup_panel,
            |this: &mut Self, _panel, _event: &SetupRefreshRequested, cx| {
                this.do_refresh_lemonade_setup(cx);
            },
        );
        let setup_backend_install = cx.subscribe(
            &setup_panel,
            |this: &mut Self, _panel, request: &SetupBackendInstallRequested, cx| {
                this.do_install_lemonade_backend(request.clone(), cx);
            },
        );
        let setup_download = cx.subscribe(
            &setup_panel,
            |this: &mut Self, _panel, request: &SetupDownloadRequested, cx| {
                this.do_control_lemonade_download(request.clone(), cx);
            },
        );
        let setup_closed = cx.subscribe(
            &setup_panel,
            |this: &mut Self, _panel, _event: &SetupClosed, cx| {
                this.setup_open = false;
                cx.notify();
            },
        );

        let mut state = AppState::new(
            graph,
            snapshot_arc,
            data_file,
            schema_dir,
            app_config,
            tokio_rt,
        );
        state.lemonade_connection = initial_lemonade_connection;

        // GPUI only polls asynchronous quit observers for 100 ms. Perform the
        // owned-process shutdown synchronously in the observer callback so the
        // model unload and child reap are allowed to complete. AppView::drop
        // repeats this idempotent cleanup because closing the last window can
        // release the root entity independently of this subscription.
        let app_quit_sub = cx.on_app_quit(|this, _cx| {
            this.shutdown_for_exit();
            async {}
        });

        let mut view = Self {
            state,
            graph_canvas,
            node_panel,
            search_panel,
            node_editor,
            chat_panel,
            setup_panel,
            lemonade_init_state: LemonadeInitState::Offline,
            lemonade_init_generation: 0,
            startup,
            selection,
            file_menu_open: false,
            view_menu_open: false,
            file_menu_button_focus: cx.focus_handle().tab_stop(true),
            view_menu_button_focus: cx.focus_handle().tab_stop(true),
            file_menu_focus: cx.focus_handle(),
            view_menu_focus: cx.focus_handle(),
            menu_anchors: Rc::new(Cell::new([
                point(px(0.0), px(0.0)),
                point(px(0.0), px(0.0)),
            ])),
            window_control_focus: WindowControlFocusHandles {
                minimize: cx.focus_handle(),
                maximize: cx.focus_handle(),
                close: cx.focus_handle(),
            },
            setup_open: false,
            active_world_canvas_view: WorldCanvasViewId::Connections,
            settings_view: None,
            settings_close_after_save: false,
            _settings_subs: vec![],
            ui_font_size,
            ui_interface_size,
            show_advanced_controls,
            window_controls_left,
            dock_state,
            last_region_focus: HashMap::new(),
            last_selected_panel: None,
            workspace_state_path,
            workspace_persist_task: None,
            import_cancellation: None,
            import_task: None,
            import_generation: 0,
            path_picker: None,
            _path_picker_subs: vec![],
            confirmation: None,
            pending_destructive_action: None,
            _confirmation_subs: vec![],
            _node_subs: vec![
                node_sub_create,
                node_sub_delete,
                selection_sub,
                close_dirty_tab_sub,
                connect_sub,
                assistant_zoom_sub,
                setup_requested,
                setup_refresh,
                setup_backend_install,
                setup_download,
                setup_closed,
            ],
            _app_quit_sub: app_quit_sub,
            _lemonade_signal_task: None,
            _graph_change_task: None,
            _management_event_task: None,
            active_management_operations: HashSet::new(),
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
                let mut lagged_messages = match event {
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => skipped,
                    _ => 0,
                };

                // Imports and agent tool chains can commit bursts of changes.
                // One frame-sized debounce turns them into one snapshot refresh.
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(16))
                    .await;
                loop {
                    match graph_changes.try_recv() {
                        Ok(_) => {}
                        Err(tokio::sync::broadcast::error::TryRecvError::Lagged(skipped)) => {
                            lagged_messages = lagged_messages.saturating_add(skipped);
                        }
                        Err(_) => break,
                    }
                }
                let Some(this) = this.upgrade() else { return };
                if this
                    .update(cx, |view: &mut AppView, cx| {
                        if lagged_messages > 0 {
                            view.state.graph_event_lag_events =
                                view.state.graph_event_lag_events.saturating_add(1);
                            view.state.graph_event_lagged_messages = view
                                .state
                                .graph_event_lagged_messages
                                .saturating_add(lagged_messages);
                            tracing::warn!(
                                lagged_messages,
                                lag_events = view.state.graph_event_lag_events,
                                lagged_total = view.state.graph_event_lagged_messages,
                                recovery = "full_snapshot_refresh",
                                "Graph change receiver lagged"
                            );
                        }
                        let recovered = view.refresh_snapshot(cx);
                        if lagged_messages > 0 && recovered {
                            view.state.graph_lag_recoveries =
                                view.state.graph_lag_recoveries.saturating_add(1);
                            tracing::info!(
                                lagged_messages,
                                recoveries = view.state.graph_lag_recoveries,
                                "Graph event lag recovered by full snapshot refresh"
                            );
                        }
                    })
                    .is_err()
                {
                    return;
                }
            }
        }));

        let mut management_events = view.state.management_events.subscribe();
        view._management_event_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let event = match management_events.recv().await {
                    Ok(event) => event,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "Lemonade management UI event receiver lagged");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                };
                let Some(this) = this.upgrade() else { return };
                if this
                    .update(cx, |view: &mut AppView, cx| {
                        if event.is_terminal() {
                            view.active_management_operations.remove(&event.target);
                        } else {
                            view.active_management_operations
                                .insert(event.target.clone());
                        }
                        view.setup_panel.update(cx, |panel, cx| {
                            panel.apply_management_progress(&event);
                            cx.notify();
                        });
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        }));

        view.refresh_native_menus(cx);
        view.do_init_lemonade(cx);
        view
    }

    /// Rebuild the in-memory snapshot from the graph and push it to all child views.
    ///
    /// Uses `build_snapshot_incremental` when a previous snapshot exists so
    /// legend bookkeeping can reuse the prior type set. Spatial state is always
    /// bulk-rebuilt from the newly committed node positions.
    pub(crate) fn refresh_snapshot(&mut self, cx: &mut Context<Self>) -> bool {
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
            Ok(mut snap) => {
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
                self.graph_canvas.update(cx, |canvas, _cx| {
                    canvas.reconcile_snapshot_refresh(&mut snap)
                });
                *self.state.snapshot.write() = snap;
                if !selected_still_exists {
                    self.selection
                        .update(cx, |selection, cx| selection.clear(cx));
                }
                self.node_panel
                    .update(cx, |panel, cx| panel.refresh_groups(cx));
                cx.notify();
                true
            }
            Err(e) => {
                eprintln!("Failed to rebuild snapshot: {e}");
                false
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

    pub(crate) fn request_clear_data(&mut self, window: &Window, cx: &mut Context<Self>) {
        let return_focus = window
            .focused(cx)
            .unwrap_or_else(|| self.graph_canvas.read(cx).focus_handle(cx));
        self.open_confirmation(
            DestructiveAction::ClearData,
            "Clear all data",
            "Delete every world item, relationship, text chunk, and saved canvas position? This cannot be undone.",
            "Clear Data",
            return_focus,
            cx,
        );
    }

    pub(crate) fn request_clear_schema(&mut self, window: &Window, cx: &mut Context<Self>) {
        let return_focus = window
            .focused(cx)
            .unwrap_or_else(|| self.graph_canvas.read(cx).focus_handle(cx));
        self.open_confirmation(
            DestructiveAction::ClearSchema,
            "Clear schemas",
            "Remove all imported schemas? Existing graph data is not changed, but schema validation will be unavailable.",
            "Clear Schema",
            return_focus,
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
                "Delete “{node_name}” and its connected relationships and text chunks? This cannot be undone."
            ),
            "Delete Node",
            self.node_panel.read(cx).focus_handle(cx),
            cx,
        );
    }

    fn open_confirmation(
        &mut self,
        action: DestructiveAction,
        title: &str,
        message: &str,
        confirm_label: &str,
        return_focus: FocusHandle,
        cx: &mut Context<Self>,
    ) {
        let modal = cx.new(|cx| {
            ConfirmationModal::new(
                title.to_string(),
                message.to_string(),
                confirm_label.to_string(),
                return_focus,
                cx,
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

    fn open_dirty_tab_confirmation(&mut self, index: usize, cx: &mut Context<Self>) {
        let return_focus = self.node_editor.read(cx).focus_handle(cx);
        let modal = cx.new(|cx| {
            ConfirmationModal::new(
                "Save changes before closing?".to_string(),
                "This Details tab has unsaved changes.".to_string(),
                "Save Changes".to_string(),
                return_focus,
                cx,
            )
            .with_alternative("Don't Save")
            .non_destructive()
        });
        let accepted = cx.subscribe(
            &modal,
            move |this, _modal, _event: &ConfirmationAccepted, cx| {
                this.confirmation = None;
                this._confirmation_subs.clear();
                this.save_and_close_editor_tab(index, cx);
            },
        );
        let discarded = cx.subscribe(
            &modal,
            move |this, _modal, _event: &ConfirmationAlternative, cx| {
                this.confirmation = None;
                this._confirmation_subs.clear();
                this.node_editor
                    .update(cx, |editor, cx| editor.close_tab(index, cx));
                cx.notify();
            },
        );
        let cancelled = cx.subscribe(
            &modal,
            |this, _modal, _event: &ConfirmationCancelled, cx| {
                this.confirmation = None;
                this._confirmation_subs.clear();
                cx.notify();
            },
        );
        self.pending_destructive_action = None;
        self.confirmation = Some(modal);
        self._confirmation_subs = vec![accepted, discarded, cancelled];
        cx.notify();
    }

    fn request_close_active_editor_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.node_editor.read(cx).active_tab else {
            return;
        };
        if self
            .node_editor
            .read(cx)
            .tabs
            .get(index)
            .is_some_and(|tab| tab.dirty)
        {
            self.open_dirty_tab_confirmation(index, cx);
        } else {
            self.node_editor
                .update(cx, |editor, cx| editor.close_tab(index, cx));
            self.node_editor.read(cx).focus_handle(cx).focus(window);
        }
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
        if let Some(previous) = self.import_cancellation.take() {
            previous.supersede();
        }
        if let Some(previous) = self.import_task.take() {
            // Observe token-driven termination without allowing the old import
            // to update this generation's UI state.
            previous.detach();
        }
        self.import_generation = self.import_generation.wrapping_add(1);
        let generation = self.import_generation;
        let cancellation = CancellationToken::new();
        self.import_cancellation = Some(cancellation.clone());
        tracing::info!(
            ui_action = "import_data",
            phase = "clicked",
            data_file = %data_file.display(),
            "UI action started"
        );

        self.state.data_status = Some("Importing…".to_string());
        cx.notify();

        let task = cx.spawn(async move |this, cx| {
            let import_start = std::time::Instant::now();
            let result = u_forge_core::ingest::import_data_only_with_cancellation(
                &graph,
                &data_file,
                cancellation.clone(),
            )
            .await;
            let import_duration_ms = import_start.elapsed().as_millis() as u64;
            tracing::info!(
                ui_action = "import_data",
                phase = "core_import_finished",
                duration_ms = import_duration_ms,
                success = result.is_ok(),
                "UI action phase finished"
            );

            this.update(cx, |view: &mut AppView, cx| {
                if generation != view.import_generation {
                    return;
                }
                view.import_cancellation = None;
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
                            format!(", {} relationships skipped", stats.edge_records_skipped)
                        } else {
                            String::new()
                        };
                        let diagnostics = stats
                            .diagnostics_path
                            .as_ref()
                            .map(|path| format!(", diagnostics: {}", path.display()))
                            .unwrap_or_default();
                        view.state.data_status = Some(format!(
                            "Import done — {} world items, {} relationships{}{}{}{}{}",
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
                    Err(_) if cancellation.is_cancelled() => {
                        view.state.data_status = Some("Import cancelled.".to_string());
                        cx.notify();
                    }
                    Err(e) => {
                        view.state.data_status = Some(format!("Import failed: {e}"));
                        cx.notify();
                    }
                }
            })
            .ok();
        });
        self.import_task = Some(task);
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
        let return_focus = window
            .focused(cx)
            .unwrap_or_else(|| self.graph_canvas.read(cx).focus_handle(cx));
        let modal =
            cx.new(|cx| PathPickerModal::new(mode, title, title, initial_path, return_focus, cx));

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
                    let result = mgr.save_schema(&schema_def);
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

    pub(crate) fn do_save_active(&mut self, cx: &mut Context<Self>) {
        self.do_save_editor(false, cx);
    }

    pub(crate) fn do_save_all(&mut self, cx: &mut Context<Self>) {
        self.do_save_editor(true, cx);
    }

    fn do_save_editor(&mut self, all: bool, cx: &mut Context<Self>) {
        // Save All is also the explicit full workspace flush.
        if all {
            self.graph_canvas.read(cx).save_layout();
        }

        let incomplete_relationships = if all {
            self.node_editor.read(cx).incomplete_relationship_count()
        } else {
            self.node_editor
                .read(cx)
                .active_incomplete_relationship_count()
        };
        if incomplete_relationships > 0 {
            self.state.data_status = Some(format!(
                "Cannot save: complete all {incomplete_relationships} unfinished relationship(s)."
            ));
            cx.notify();
            return;
        }

        let (saved, saved_ids, discarded_ids, skipped_edges) =
            self.node_editor.update(cx, |editor, cx| {
                if all {
                    editor.save_dirty_tabs(cx)
                } else {
                    editor.save_active_tab(cx)
                }
            });
        self.finish_editor_save((saved, saved_ids, discarded_ids, skipped_edges), cx);
    }

    fn save_and_close_editor_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        let incomplete = self
            .node_editor
            .read(cx)
            .incomplete_relationship_count_at(index);
        if incomplete > 0 {
            self.state.data_status = Some(format!(
                "Cannot save: complete all {incomplete} unfinished relationship(s)."
            ));
            cx.notify();
            return;
        }
        let node_id = self
            .node_editor
            .read(cx)
            .tabs
            .get(index)
            .map(|tab| tab.node_id);
        let result = self
            .node_editor
            .update(cx, |editor, cx| editor.save_tab(index, cx));
        self.finish_editor_save(result, cx);

        if let Some(node_id) = node_id {
            let still_dirty = self
                .node_editor
                .read(cx)
                .tabs
                .iter()
                .any(|tab| tab.node_id == node_id && tab.dirty);
            if still_dirty {
                self.state.data_status = Some(
                    "Could not close the tab because its changes did not pass validation."
                        .to_string(),
                );
            } else {
                self.node_editor.update(cx, |editor, cx| {
                    if let Some(index) = editor.tabs.iter().position(|tab| tab.node_id == node_id) {
                        editor.close_tab(index, cx);
                    }
                });
            }
        }
        cx.notify();
    }

    fn finish_editor_save(
        &mut self,
        result: (usize, Vec<ObjectId>, Vec<ObjectId>, usize),
        cx: &mut Context<Self>,
    ) {
        let (saved, saved_ids, discarded_ids, skipped_edges) = result;

        if skipped_edges > 0 {
            self.state.data_status = Some(format!(
                "{skipped_edges} incomplete relationship(s) skipped — fill both endpoints before saving."
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

    /// Create an in-memory Details draft. Storage is not touched until Save.
    fn create_node(&mut self, object_type: &str, cx: &mut Context<Self>) {
        let meta = ObjectMetadata::new(object_type.to_string(), String::new());
        self.node_editor
            .update(cx, |editor, cx| editor.open_new_draft(meta, cx));
        self.dock_state.activate_panel(PanelId::World);
        self.dock_state.activate_panel(PanelId::Details);
        self.schedule_workspace_persist(cx);
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
    /// A newer plan supersedes older UI progress and cancels queued/active child
    /// work through the previous plan's parent token.
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
        match plan.has_pending_work(
            &graph,
            queue.has_embedding(),
            hq_queue.as_ref().is_some_and(|q| q.has_embedding()),
        ) {
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
        let (generation, superseded, cancellation) = self.state.embedding_plan.start();
        self.state.embedding_status = Some(if superseded {
            format!("{} (previous work cancelled)", plan.label())
        } else {
            plan.label()
        });
        if superseded {
            tracing::info!(
                ui_action = "embedding",
                phase = "superseded",
                plan_kind,
                "Previous embedding work was cancelled and may no longer update UI status"
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
                            plan.execute_with_cancellation(
                                &graph,
                                &queue,
                                hq_queue.as_ref(),
                                cancellation,
                                move |p| *progress_write.lock() = Some(p),
                            )
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
    pub(crate) fn do_refresh_lemonade_setup(&mut self, cx: &mut Context<Self>) {
        let Some(connection) = self.state.lemonade_connection.clone() else {
            self.setup_panel.update(cx, |panel, cx| {
                panel.set_busy(
                    false,
                    "Lemonade is not connected; retry the connection first.",
                );
                cx.notify();
            });
            return;
        };
        let tokio_rt = self.state.tokio_rt.clone();
        self.setup_panel.update(cx, |panel, cx| {
            panel.set_busy(true, "Refreshing catalog and durable downloads…");
            cx.notify();
        });
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    tokio_rt.block_on(async move {
                        let catalog = LemonadeServerCatalog::discover_with_connection(
                            connection.clone(),
                        )
                        .await?;
                        let downloads = LemonadeManagement::new(connection).downloads().await;
                        Ok::<_, anyhow::Error>((catalog, downloads))
                    })
                })
                .await;
            this.update(cx, |view, cx| match result {
                Ok((catalog, downloads)) => {
                    view.state.lemonade_catalog = Some(catalog.clone());
                    let mut complete = false;
                    view.setup_panel.update(cx, |panel, cx| {
                        panel.refresh_catalog(&catalog);
                        match downloads {
                            Ok(value) => panel.set_downloads(&value),
                            Err(error) => panel.set_busy(
                                false,
                                format!(
                                    "Catalog refreshed, but durable downloads are unavailable: {error}"
                                ),
                            ),
                        }
                        complete = panel.is_complete();
                        if complete {
                            panel.set_busy(false, "Setup is complete. AI providers are ready.");
                        } else {
                            panel.set_busy(false, "Setup still has components to provision.");
                        }
                        cx.notify();
                    });
                    if complete {
                        view.do_init_lemonade(cx);
                    }
                }
                Err(error) => view.setup_panel.update(cx, |panel, cx| {
                    panel.set_busy(false, format!("Setup refresh failed: {error}"));
                    cx.notify();
                }),
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn do_install_lemonade_backend(
        &mut self,
        request: SetupBackendInstallRequested,
        cx: &mut Context<Self>,
    ) {
        let Some(connection) = self.state.lemonade_connection.clone() else {
            self.setup_panel.update(cx, |panel, cx| {
                panel.set_busy(
                    false,
                    "Lemonade is not connected; retry the connection first.",
                );
                cx.notify();
            });
            return;
        };
        let tokio_rt = self.state.tokio_rt.clone();
        let events = self.state.management_events.clone();
        let label = format!("{}:{}", request.recipe, request.backend);
        self.setup_panel.update(cx, |panel, cx| {
            panel.set_busy(true, format!("Installing backend {label}…"));
            cx.notify();
        });
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    tokio_rt.block_on(async move {
                        let manager = LemonadeManagement::new(connection.clone());
                        let receiver = manager
                            .install_backend_stream(
                                &request.recipe,
                                &request.backend,
                                request.confirmed_external,
                            )
                            .await?;
                        forward_management_events(receiver, &events).await?;
                        let catalog = LemonadeServerCatalog::discover_with_connection(
                            connection.clone(),
                        )
                        .await?;
                        let downloads = manager.downloads().await;
                        Ok::<_, anyhow::Error>((catalog, downloads))
                    })
                })
                .await;
            this.update(cx, |view, cx| {
                view.setup_panel.update(cx, |panel, cx| {
                    match result {
                        Ok((catalog, downloads)) => {
                            view.state.lemonade_catalog = Some(catalog.clone());
                            panel.refresh_catalog(&catalog);
                            if let Ok(downloads) = downloads {
                                panel.set_downloads(&downloads);
                            }
                            panel.set_busy(
                                false,
                                format!(
                                    "Backend {label} installation completed. Review its refreshed state below."
                                ),
                            );
                        }
                        Err(error) => panel.set_busy(
                            false,
                            format!("Backend {label} installation failed: {error}"),
                        ),
                    }
                    cx.notify();
                });
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn do_control_lemonade_download(
        &mut self,
        request: SetupDownloadRequested,
        cx: &mut Context<Self>,
    ) {
        let Some(connection) = self.state.lemonade_connection.clone() else {
            return;
        };
        let tokio_rt = self.state.tokio_rt.clone();
        let events = self.state.management_events.clone();
        self.setup_panel.update(cx, |panel, cx| {
            panel.set_busy(
                true,
                format!("Applying {:?} to download…", request.operation),
            );
            cx.notify();
        });
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    tokio_rt.block_on(async move {
                        let manager = LemonadeManagement::new(connection);
                        match request.operation {
                            SetupDownloadOperation::Control(action) => {
                                manager
                                    .control_download(
                                        &request.job_id,
                                        action,
                                        request.confirmed_external,
                                    )
                                    .await?;
                            }
                            SetupDownloadOperation::Retry => {
                                // Current Lemonade exposes pause/cancel/remove
                                // controls. Retry/resume is a stopped-job remove
                                // followed by the same durable pull; partial files
                                // are reused by the server downloader.
                                manager
                                    .control_download(
                                        &request.job_id,
                                        u_forge_core::lemonade::DownloadAction::Remove,
                                        request.confirmed_external,
                                    )
                                    .await?;
                                let component =
                                    initial_setup_components().into_iter().find(|component| {
                                        component.matches_model_id(&request.model_name)
                                    });
                                let (model_name, checkpoint, recipe, embedding) = component
                                    .as_ref()
                                    .map(|component| {
                                        let pull = component.pull_spec();
                                        (
                                            pull.model_name,
                                            pull.checkpoint,
                                            pull.recipe,
                                            pull.embedding,
                                        )
                                    })
                                    .unwrap_or((request.model_name.as_str(), None, None, None));
                                let receiver = manager
                                    .pull_stream(
                                        model_name,
                                        checkpoint,
                                        recipe,
                                        embedding,
                                        request.confirmed_external,
                                    )
                                    .await?;
                                forward_management_events(receiver, &events).await?;
                            }
                        }
                        manager.downloads().await
                    })
                })
                .await;
            this.update(cx, |view, cx| {
                view.setup_panel.update(cx, |panel, cx| {
                    match result {
                        Ok(downloads) => {
                            panel.set_downloads(&downloads);
                            panel.set_busy(false, "Download action accepted by Lemonade.");
                        }
                        Err(error) => {
                            panel.set_busy(false, format!("Download action failed: {error}"));
                        }
                    }
                    cx.notify();
                });
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn do_provision_lemonade(
        &mut self,
        request: SetupRequested,
        cx: &mut Context<Self>,
    ) {
        let (Some(connection), Some(catalog)) = (
            self.state.lemonade_connection.clone(),
            self.state.lemonade_catalog.clone(),
        ) else {
            self.setup_panel.update(cx, |panel, cx| {
                panel.set_busy(
                    false,
                    "A live Lemonade catalog is required before provisioning.",
                );
                cx.notify();
            });
            return;
        };

        let mut next_config = (*self.state.app_config).clone();
        if let Err(error) = next_config.persist_lemonade_setup(
            request.high_quality_embedding,
            request.preferred_device.clone(),
            &request.chat_model,
            request.reasoning_control,
        ) {
            self.setup_panel.update(cx, |panel, cx| {
                panel.set_busy(false, format!("Could not save setup choices: {error}"));
                cx.notify();
            });
            return;
        }
        next_config.embedding.high_quality_embedding = request.high_quality_embedding;
        next_config.chat.preferred_device = request.preferred_device.clone();
        next_config.chat.reasoning_control = request.reasoning_control;
        match request.preferred_device {
            u_forge_core::ChatDevice::Auto | u_forge_core::ChatDevice::Gpu => {
                next_config.chat.gpu.model = Some(request.chat_model.clone())
            }
            u_forge_core::ChatDevice::Npu => {
                next_config.chat.npu.model = Some(request.chat_model.clone())
            }
            u_forge_core::ChatDevice::Cpu => {
                next_config.chat.cpu.model = Some(request.chat_model.clone())
            }
        }
        self.state.app_config = Arc::new(next_config.clone());

        let tokio_rt = self.state.tokio_rt.clone();
        let events = self.state.management_events.clone();
        self.setup_panel.update(cx, |panel, cx| {
            panel.set_busy(true, "Starting server-owned provisioning jobs…");
            cx.notify();
        });
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    tokio_rt.block_on(provision_lemonade(
                        connection,
                        catalog,
                        next_config,
                        request,
                        events,
                    ))
                })
                .await;
            this.update(cx, |view, cx| {
                view.setup_panel.update(cx, |panel, cx| {
                    match result {
                        Ok((catalog, downloads, message)) => {
                            view.state.lemonade_catalog = Some(catalog.clone());
                            panel.refresh_catalog(&catalog);
                            panel.set_downloads(&downloads);
                            panel.set_busy(false, message);
                        }
                        Err(error) => {
                            panel.set_busy(false, format!("Provisioning failed: {error}"));
                        }
                    }
                    cx.notify();
                });
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn do_init_lemonade(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.lemonade_init_state,
            LemonadeInitState::Discovering | LemonadeInitState::CapabilitiesLoading
        ) {
            tracing::debug!("Lemonade initialization is already in flight");
            return;
        }
        self.lemonade_init_generation = self.lemonade_init_generation.wrapping_add(1);
        let generation = self.lemonade_init_generation;
        self.lemonade_init_state = LemonadeInitState::Discovering;
        self.chat_panel.update(cx, |panel, _cx| {
            panel.set_connecting(true);
        });
        cx.notify();

        let app_config = self.state.app_config.clone();
        let max_loaded_models = app_config.lemonade.max_loaded_models;
        let tokio_rt = self.state.tokio_rt.clone();
        let existing_connection = self.state.lemonade_connection.clone();
        let existing_embedded = self.state.embedded_lemonade.clone();
        let startup = self.startup.clone();

        cx.spawn(async move |this, cx| {
            let metadata_timeline = startup.clone();
            let metadata_runtime = tokio_rt.clone();
            let metadata_result = cx
                .background_executor()
                .spawn(
                    async move {
                        metadata_runtime.block_on(discover_lemonade_metadata(
                            existing_connection,
                            existing_embedded,
                            max_loaded_models,
                            metadata_timeline,
                        ))
                    }
                    .instrument(tracing::info_span!("lemonade_metadata_init")),
                )
                .await;
            let metadata = match metadata_result {
                Ok(metadata) => metadata,
                Err(error) => {
                    this.update(cx, |view: &mut AppView, cx| {
                        if view.lemonade_init_generation != generation {
                            return;
                        }
                        eprintln!("Lemonade init skipped: {error}");
                        view.lemonade_init_state = LemonadeInitState::Failed;
                        view.chat_panel.update(cx, |panel, _cx| {
                            panel.set_connect_failed(&error.to_string());
                        });
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };

            let metadata_for_ui = metadata.clone();
            if this
                .update(cx, move |view: &mut AppView, cx| {
                    if view.lemonade_init_generation != generation {
                        return;
                    }
                    view.apply_lemonade_metadata(metadata_for_ui, cx);
                    view.lemonade_init_state = LemonadeInitState::CapabilitiesLoading;
                })
                .is_err()
            {
                return;
            }
            if startup.should_exit_after(StartupMilestone::LemonadeMetadataReady) {
                return;
            }

            let activation_runtime = tokio_rt;
            let activation_config = app_config;
            let activation_timeline = startup.clone();
            let activation_connection = metadata.connection.clone();
            let activation_catalog = metadata.catalog.clone();
            let activation_result = cx
                .background_executor()
                .spawn(
                    async move {
                        activation_runtime.block_on(activate_lemonade_capabilities(
                            activation_connection,
                            activation_catalog,
                            activation_config,
                            activation_timeline,
                        ))
                    }
                    .instrument(tracing::info_span!("lemonade_capability_activation")),
                )
                .await;

            this.update(cx, move |view: &mut AppView, cx| {
                if view.lemonade_init_generation != generation {
                    return;
                }
                match activation_result {
                    Ok(activation) => {
                        view.apply_lemonade_activation(activation, cx);
                        view.lemonade_init_state = LemonadeInitState::Ready;
                    }
                    Err(error) => {
                        eprintln!("Lemonade capability activation failed: {error}");
                        view.lemonade_init_state = LemonadeInitState::Degraded;
                        view.state.embedding_status = Some(format!(
                            "Assistant connected; retrieval AI is unavailable: {error}"
                        ));
                        view.chat_panel.update(cx, |panel, cx| {
                            panel.finish_capability_initialization(cx);
                        });
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Supersede any activation based on old settings and immediately discover
    /// capabilities again with the newly persisted configuration.
    fn reconfigure_lemonade(&mut self, cx: &mut Context<Self>) {
        self.lemonade_init_generation = self.lemonade_init_generation.wrapping_add(1);
        self.lemonade_init_state = LemonadeInitState::Offline;
        self.do_init_lemonade(cx);
    }

    fn start_managed_baseline_provisioning(
        &mut self,
        connection: Arc<LemonadeConnection>,
        catalog: LemonadeServerCatalog,
        cx: &mut Context<Self>,
    ) {
        if connection.ownership() != LemonadeOwnership::Embedded {
            return;
        }
        let tokio_rt = self.state.tokio_rt.clone();
        let events = self.state.management_events.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    tokio_rt.block_on(provision_managed_baseline(connection, catalog, events))
                })
                .await;
            this.update(cx, |view: &mut AppView, cx| match result {
                Ok(true) => {
                    // Re-discover the completed artifacts and supersede any
                    // activation based on the old catalog snapshot.
                    view.lemonade_init_state = LemonadeInitState::Offline;
                    view.do_init_lemonade(cx);
                }
                Ok(false) => {}
                Err(error) => {
                    view.state.embedding_status = Some(format!(
                        "Default retrieval model provisioning failed: {error}"
                    ));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn apply_lemonade_metadata(&mut self, metadata: LemonadeMetadata, cx: &mut Context<Self>) {
        let _phase = self.startup.phase("lemonade_metadata_ui_apply");
        self.state.embedded_lemonade = metadata.embedded;
        if self._lemonade_signal_task.is_none()
            && let Some(embedded) = self.state.embedded_lemonade.clone()
        {
            self._lemonade_signal_task = Some(self.state.tokio_rt.spawn(async move {
                match tokio::signal::ctrl_c().await {
                    Ok(()) => {
                        tracing::info!("Ctrl-C received; shutting down embedded Lemonade");
                        embedded.shutdown().await;
                        std::process::exit(130);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "Could not install the Ctrl-C shutdown handler");
                    }
                }
            }));
        }
        self.state.lemonade_connection = Some(metadata.connection.clone());
        self.state.lemonade_catalog = Some(metadata.catalog.clone());
        let chat = prepare_lemonade_chat(
            metadata.connection.clone(),
            &metadata.catalog,
            &self.state.app_config,
        );
        if let Some(provider) = chat.provider {
            self.chat_panel.update(cx, |panel, _cx| {
                panel.set_provider(
                    provider,
                    chat.models,
                    chat.preferred_idx,
                    chat.runtime,
                    self.state.app_config.chat.reasoning_control,
                );
                panel.begin_capability_initialization();
            });
        } else {
            self.chat_panel.update(cx, |panel, _cx| {
                panel.set_connect_failed("No downloaded LLM models available");
            });
        }
        let setup_hq_default = if self
            .state
            .app_config
            .source_path
            .as_ref()
            .is_some_and(|path| path.exists())
        {
            self.state.app_config.embedding.high_quality_embedding
        } else {
            true
        };
        let mut setup = SetupPanel::new(
            metadata.connection.ownership(),
            &metadata.catalog,
            self.state
                .app_config
                .chat
                .active_device_config()
                .model
                .as_deref(),
            setup_hq_default,
            self.state.app_config.embedding.npu_enabled,
            self.state.app_config.chat.preferred_device.clone(),
            self.state.app_config.chat.reasoning_control,
        )
        .with_startup_timeline(self.startup.clone());
        match metadata.downloads {
            Ok(downloads) => {
                setup.set_downloads(&downloads);
                let degraded = [
                    metadata.catalog.diagnostics.health.as_deref(),
                    metadata.catalog.diagnostics.system_info.as_deref(),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
                if !degraded.is_empty() {
                    setup.set_busy(
                        false,
                        format!("Discovery is degraded: {}", degraded.join("; ")),
                    );
                }
            }
            Err(error) => setup.set_busy(
                false,
                format!("Inference is available, but managed downloads are unavailable: {error}"),
            ),
        }
        let setup_incomplete = !setup.is_complete();
        self.setup_panel.update(cx, |panel, cx| {
            *panel = setup;
            cx.notify();
        });
        self.setup_open |= setup_incomplete;
        self.start_managed_baseline_provisioning(
            metadata.connection.clone(),
            metadata.catalog.clone(),
            cx,
        );
        eprintln!("{LEMONADE_METADATA_READY_MESSAGE}");
        self.startup
            .milestone(StartupMilestone::LemonadeMetadataReady);
        cx.notify();
        if self
            .startup
            .should_exit_after(StartupMilestone::LemonadeMetadataReady)
        {
            cx.quit();
        }
    }

    fn apply_lemonade_activation(
        &mut self,
        activation: LemonadeActivation,
        cx: &mut Context<Self>,
    ) {
        let _phase = self.startup.phase("lemonade_activation_ui_apply");
        let LemonadeActivation {
            queue,
            hq_queue,
            chat_provider,
            llm_models,
            preferred_idx,
            runtime,
            effective_limits,
        } = activation;
        let has_embedding =
            queue.has_embedding() || hq_queue.as_ref().is_some_and(|queue| queue.has_embedding());
        let has_chat = chat_provider.is_some();
        self.state.inference_queue = Some(queue.clone());
        self.state.hq_queue = hq_queue.clone();
        let hq_arc = hq_queue.clone().map(Arc::new);
        self.search_panel.update(cx, |panel, _cx| {
            panel.set_queues(Some(queue.clone()), hq_queue);
        });

        let agent_gpu = chat_provider
            .as_ref()
            .and_then(|provider| provider.gpu.clone());
        let developer = llm_models
            .get(preferred_idx)
            .map(|model| &model.sampling)
            .unwrap_or_else(|| self.state.app_config.chat.active_device_config());
        let agent_params = AgentParams {
            temperature: developer.temperature.map(f64::from),
            max_tokens: effective_limits
                .as_ref()
                .map(|limits| limits.agent_generation as u64)
                .or_else(|| developer.max_tokens.map(u64::from)),
            top_p: developer.top_p.map(f64::from),
            top_k: developer.top_k,
            min_p: developer.min_p.map(f64::from),
            frequency_penalty: developer.frequency_penalty.map(f64::from),
            presence_penalty: developer.presence_penalty.map(f64::from),
            repetition_penalty: developer.repetition_penalty.map(f64::from),
            seed: developer.seed,
            stop: developer.stop.clone(),
            max_tool_turns: self.state.app_config.chat.max_tool_turns,
            budget: llm_models
                .get(preferred_idx)
                .map(|model| model.agent_budget.clone())
                .unwrap_or_default(),
        };
        if has_chat {
            match GraphAgent::new_with_connection_and_gpu(
                runtime.connection().clone(),
                self.state.graph.clone(),
                Arc::new(queue),
                hq_arc,
                self.state.app_config.chat.system_prompt.clone(),
                agent_params,
                agent_gpu,
            ) {
                Ok(agent) => self
                    .chat_panel
                    .update(cx, |panel, _cx| panel.set_agent(agent)),
                Err(error) => eprintln!("GraphAgent init failed: {error}"),
            }
        }

        if let Some(provider) = chat_provider {
            self.chat_panel.update(cx, |panel, _cx| {
                panel.set_provider(
                    provider,
                    llm_models,
                    preferred_idx,
                    runtime,
                    self.state.app_config.chat.reasoning_control,
                );
            });
        } else {
            self.chat_panel.update(cx, |panel, _cx| {
                panel.set_connect_failed("No downloaded LLM models available");
            });
        }
        self.chat_panel.update(cx, |panel, cx| {
            panel.finish_capability_initialization(cx);
        });
        if has_embedding {
            self.run_embedding_plan(EmbeddingPlan::embed_all(), cx);
        }
        self.startup.milestone(StartupMilestone::StartupReady);
        cx.notify();
        if self
            .startup
            .should_exit_after(StartupMilestone::StartupReady)
        {
            cx.quit();
        }
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
                            "Exported {node_count} world items, {edge_count} relationships → {filename}"
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

impl Drop for AppView {
    fn drop(&mut self) {
        // On Linux the default GPUI quit mode is LastWindowClosed. The root
        // entity can therefore be released as part of closing the final
        // window; do not rely exclusively on its app-quit subscription still
        // being registered at that point.
        self.shutdown_for_exit();
    }
}

fn select_preferred_llm_index(
    models: &[u_forge_core::lemonade::SelectedModel],
    preferred_device: u_forge_core::ChatDevice,
    explicit_model: Option<&str>,
) -> (usize, Option<String>) {
    if let Some(explicit_model) = explicit_model
        && let Some(index) = models
            .iter()
            .position(|model| model.model_id == explicit_model)
    {
        return (index, None);
    }

    let requested = match preferred_device {
        u_forge_core::ChatDevice::Auto => None,
        u_forge_core::ChatDevice::Gpu => Some("gpu"),
        u_forge_core::ChatDevice::Npu => Some("npu"),
        u_forge_core::ChatDevice::Cpu => Some("cpu"),
    };
    let index = requested
        .and_then(|device| {
            models
                .iter()
                .position(|model| selected_model_device(model) == device)
        })
        .or_else(|| {
            ["gpu", "npu", "cpu"].into_iter().find_map(|device| {
                models
                    .iter()
                    .position(|model| selected_model_device(model) == device)
            })
        })
        .unwrap_or(0);
    let selected_device = models.get(index).map(selected_model_device);
    let diagnostic = if let Some(explicit) = explicit_model {
        Some(format!(
            "configured model {explicit} is unavailable; rebuilt the complete profile for {}",
            selected_device.unwrap_or("the available device")
        ))
    } else if let (Some(requested), Some(selected)) = (requested, selected_device) {
        (requested != selected).then(|| {
            format!(
                "preferred device {requested} is unavailable; rebuilt the complete profile for {selected}"
            )
        })
    } else {
        None
    };
    (index, diagnostic)
}

fn selected_model_device(model: &u_forge_core::lemonade::SelectedModel) -> &'static str {
    match model.recipe.as_str() {
        "flm" => "npu",
        "llamacpp" if matches!(model.backend.as_deref(), Some("rocm" | "vulkan" | "metal")) => {
            "gpu"
        }
        _ => "cpu",
    }
}

fn chat_device_config_for_model<'a>(
    chat: &'a u_forge_core::ChatConfig,
    model: &u_forge_core::lemonade::SelectedModel,
) -> &'a u_forge_core::ChatDeviceConfig {
    match selected_model_device(model) {
        "gpu" => &chat.gpu,
        "npu" => &chat.npu,
        _ => &chat.cpu,
    }
}

async fn provision_lemonade(
    connection: Arc<u_forge_core::lemonade::LemonadeConnection>,
    catalog: LemonadeServerCatalog,
    config: AppConfig,
    request: SetupRequested,
    events: tokio::sync::broadcast::Sender<ManagementProgressEvent>,
) -> anyhow::Result<(LemonadeServerCatalog, serde_json::Value, String)> {
    let manager = LemonadeManagement::new(connection.clone());
    // Verify that the management plane is available before mutating an
    // external or embedded runtime. Each mutation below remains subscribed to
    // its SSE stream until a terminal event arrives.
    manager.downloads().await?;
    if let Some(error) = &catalog.diagnostics.system_info {
        anyhow::bail!(
            "backend discovery is unavailable, so managed setup cannot safely install a compatible backend: {error}"
        );
    }

    let mut installed = HashSet::new();
    let mut jobs_started = 0usize;
    let components = initial_setup_components();
    for component in components.iter().filter(|component| {
        component.required
            || (component.role == u_forge_core::lemonade::SetupRole::NpuEmbedding
                && config.embedding.npu_enabled)
            || (component.role == u_forge_core::lemonade::SetupRole::HighQualityEmbedding
                && request.high_quality_embedding)
    }) {
        let state = component_state(&catalog, component);
        if let u_forge_core::lemonade::SetupComponentState::Conflict(message) = &state {
            anyhow::bail!(message.clone());
        }
        let recipe = component.recipe.or_else(|| {
            catalog
                .models
                .iter()
                .find(|model| component.matches_model_id(&model.id))
                .map(|model| model.recipe.as_str())
        });
        if let Some(recipe) = recipe.filter(|recipe| !recipe.is_empty()) {
            let choice =
                select_setup_backend(&catalog, recipe, &config.models.llamacpp_backend_preference)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "no installed or installable {recipe} backend was reported for {}",
                            component.model_id
                        )
                    })?;
            if choice.needs_install()
                && installed.insert((choice.recipe.clone(), choice.backend.clone()))
            {
                let receiver = manager
                    .install_backend_stream(
                        &choice.recipe,
                        &choice.backend,
                        request.confirmed_external,
                    )
                    .await?;
                forward_management_events(receiver, &events).await?;
            }
        }
        if state.needs_pull() {
            let pull = component.pull_spec();
            let receiver = manager
                .pull_stream(
                    pull.model_name,
                    pull.checkpoint,
                    pull.recipe,
                    pull.embedding,
                    request.confirmed_external,
                )
                .await?;
            forward_management_events(receiver, &events).await?;
            jobs_started += 1;
        }
    }

    let chat_state = chat_component_state(&catalog, &request.chat_model);
    if let u_forge_core::lemonade::SetupComponentState::Conflict(message) = &chat_state {
        anyhow::bail!(message.clone());
    }
    let chat_model = catalog
        .models
        .iter()
        .find(|model| model.id == request.chat_model)
        .ok_or_else(|| anyhow::anyhow!("selected chat model is no longer in the live catalog"))?;
    let choice = select_setup_backend(
        &catalog,
        &chat_model.recipe,
        &config.models.llamacpp_backend_preference,
    )
    .ok_or_else(|| {
        anyhow::anyhow!(
            "no installed or installable {} backend was reported for {}",
            chat_model.recipe,
            request.chat_model
        )
    })?;
    if choice.needs_install() && installed.insert((choice.recipe.clone(), choice.backend.clone())) {
        let receiver = manager
            .install_backend_stream(&choice.recipe, &choice.backend, request.confirmed_external)
            .await?;
        forward_management_events(receiver, &events).await?;
    }
    if chat_state.needs_pull() {
        let receiver = manager
            .pull_stream(
                &request.chat_model,
                None,
                None,
                None,
                request.confirmed_external,
            )
            .await?;
        forward_management_events(receiver, &events).await?;
        jobs_started += 1;
    }

    let downloads = manager.downloads().await?;
    let refreshed = LemonadeServerCatalog::discover_with_connection(connection).await?;
    let message = if jobs_started == 0 {
        "Selections saved; all selected models are already downloaded.".to_string()
    } else {
        format!("Completed {jobs_started} model download(s). The setup catalog is current.")
    };
    Ok((refreshed, downloads, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_focus_cycle_wraps_in_both_directions() {
        assert_eq!(next_focus_index(None, 4, false), Some(0));
        assert_eq!(next_focus_index(Some(3), 4, false), Some(0));
        assert_eq!(next_focus_index(None, 4, true), Some(3));
        assert_eq!(next_focus_index(Some(0), 4, true), Some(3));
        assert_eq!(next_focus_index(None, 0, false), None);
    }

    fn selected(
        id: &str,
        recipe: &str,
        backend: Option<&str>,
    ) -> u_forge_core::lemonade::SelectedModel {
        u_forge_core::lemonade::SelectedModel {
            model_id: id.to_string(),
            recipe: recipe.to_string(),
            backend: backend.map(ToString::to_string),
            load_opts: u_forge_core::ModelLoadOptions::default(),
            quality_tier: u_forge_core::lemonade::QualityTier::NotApplicable,
            checkpoint: id.to_string(),
            max_context_window: None,
            tool_capable: true,
            reasoning_capable: false,
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn preferred_device_selects_a_coherent_profile_and_reports_fallback() {
        let models = vec![
            selected("gpu", "llamacpp", Some("vulkan")),
            selected("npu", "flm", None),
            selected("cpu", "llamacpp", Some("cpu")),
        ];
        assert_eq!(
            select_preferred_llm_index(&models, u_forge_core::ChatDevice::Npu, None).0,
            1
        );
        let (index, diagnostic) =
            select_preferred_llm_index(&models[..1], u_forge_core::ChatDevice::Npu, None);
        assert_eq!(index, 0);
        assert!(diagnostic.unwrap().contains("rebuilt the complete profile"));
    }

    #[test]
    fn model_picker_profiles_use_their_own_device_sampling() {
        let mut chat = u_forge_core::ChatConfig::default();
        chat.gpu.temperature = Some(0.1);
        chat.npu.temperature = Some(0.2);
        chat.cpu.temperature = Some(0.3);
        assert_eq!(
            chat_device_config_for_model(&chat, &selected("gpu", "llamacpp", Some("vulkan")))
                .temperature,
            Some(0.1)
        );
        assert_eq!(
            chat_device_config_for_model(&chat, &selected("npu", "flm", None)).temperature,
            Some(0.2)
        );
        assert_eq!(
            chat_device_config_for_model(&chat, &selected("cpu", "llamacpp", Some("cpu")))
                .temperature,
            Some(0.3)
        );
    }

    #[test]
    fn hq_selection_never_suppresses_the_standard_embedding_provider() {
        let mut standard = selected("standard", "llamacpp", Some("vulkan"));
        standard.quality_tier = QualityTier::Standard;
        let mut hq = selected("hq", "llamacpp", Some("vulkan"));
        hq.quality_tier = QualityTier::High;

        let models = [hq, standard];
        let selected = standard_embedding_models(&models)
            .map(|model| model.model_id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(selected, ["standard"]);
    }
}
