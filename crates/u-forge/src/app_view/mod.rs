mod lemonade;
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
use u_forge_agent::GraphAgent;
use u_forge_core::{
    AppConfig, EmbeddingOutcome, EmbeddingPlan, EmbeddingProgress, KnowledgeGraph, ObjectMetadata,
    SchemaManager,
    ingest::build_hq_embed_queue_with_connection,
    lemonade::{
        Capability, EmbeddedLemonade, GpuResourceManager, LemonadeChatProvider, LemonadeConnection,
        LemonadeManagement, LemonadeOwnership, LemonadeRuntime, LemonadeServerCatalog,
        ManagementEventKind, ManagementProgressEvent, ModelSelector, ProviderFactory, QualityTier,
        SetupRole, chat_component_state, component_state, initial_setup_components,
        resolve_runtime_connection, select_setup_backend,
    },
    queue::{CancellationToken, InferenceQueueBuilder},
    types::ObjectId,
};
use u_forge_graph_view::GraphSnapshot;

use lemonade::LemonadeInitState;
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
use crate::node_editor::{CloseDirtyTabRequested, EditorSaveResult, NodeEditorPanel};
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
    SetupPanel, SetupRequested,
};
use crate::startup::{LEMONADE_METADATA_READY_MESSAGE, StartupMilestone, StartupTimeline};
use crate::ui::theme::UiTheme;
use crate::window_chrome::WindowControlFocusHandles;
use crate::world_setup::{WorldCreateRequested, WorldSetupClosed, WorldSetupModal};

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
    pub(crate) world_setup: Option<Entity<WorldSetupModal>>,
    _world_setup_subs: Vec<Subscription>,
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
        let schema_loaded = graph
            .get_schema_manager()
            .list_schemas()
            .map(|names| names.iter().any(|name| name != "default"))
            .unwrap_or(false);
        let setup_timeline = startup.clone();
        let setup_panel = cx.new(|_cx| {
            SetupPanel::new(
                LemonadeOwnership::External,
                &LemonadeServerCatalog::default(),
                app_config.chat.active_device_config().model.as_deref(),
                setup_hq_default,
                app_config.embedding.standard.npu_enabled,
                app_config.chat.preferred_device.clone(),
                app_config.chat.reasoning_control,
            )
            .with_schema_loaded(schema_loaded)
            .with_startup_timeline(setup_timeline)
        });
        let setup_requested = cx.subscribe(
            &setup_panel,
            |this: &mut Self, _panel, request: &SetupRequested, cx| {
                this.do_provision_lemonade(request.clone(), cx);
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
                if !this.state.schema_loaded {
                    this.open_world_setup(cx);
                }
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
            world_setup: None,
            _world_setup_subs: vec![],
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
                        let progress = event
                            .progress_percent
                            .map(|percent| format!(" · {percent:.0}%"))
                            .unwrap_or_default();
                        view.state.management_status = Some(match event.kind {
                            ManagementEventKind::Progress => {
                                format!("Preparing {}{progress}", event.target)
                            }
                            ManagementEventKind::Complete => format!("{} is ready", event.target),
                            ManagementEventKind::Failed => {
                                format!("{} failed", event.target)
                            }
                        });
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

    fn embedding_prerequisites_ready(&self) -> bool {
        self.state
            .inference_queue
            .as_ref()
            .is_some_and(|queue| queue.has_embedding())
            && (!self.state.app_config.embedding.high_quality_embedding
                || self
                    .state
                    .hq_queue
                    .as_ref()
                    .is_some_and(|queue| queue.has_embedding()))
    }

    fn sync_world_setup_readiness(&mut self, cx: &mut Context<Self>) {
        let ready = self.embedding_prerequisites_ready();
        if let Some(modal) = &self.world_setup {
            modal.update(cx, |modal, cx| modal.set_embedding_ready(ready, cx));
        }
    }

    fn open_world_setup(&mut self, cx: &mut Context<Self>) {
        if self.state.schema_loaded || self.world_setup.is_some() {
            return;
        }
        self.setup_open = false;
        let schema_dir = self.state.schema_dir.to_string_lossy().into_owned();
        let data_file = self.state.data_file.to_string_lossy().into_owned();
        let ready = self.embedding_prerequisites_ready();
        let modal = cx.new(|cx| WorldSetupModal::new(&schema_dir, &data_file, ready, cx));
        let create = cx.subscribe(
            &modal,
            |this: &mut Self, _modal, request: &WorldCreateRequested, cx| {
                this.do_create_world(request.clone(), cx);
            },
        );
        let close = cx.subscribe(
            &modal,
            |this: &mut Self, _modal, _event: &WorldSetupClosed, cx| {
                this.world_setup = None;
                this._world_setup_subs.clear();
                cx.notify();
            },
        );
        self.world_setup = Some(modal);
        self._world_setup_subs = vec![create, close];
        cx.notify();
    }

    fn do_create_world(&mut self, request: WorldCreateRequested, cx: &mut Context<Self>) {
        if request.data_file.is_some() && !self.embedding_prerequisites_ready() {
            self.sync_world_setup_readiness(cx);
            return;
        }
        let Some(modal) = self.world_setup.clone() else {
            return;
        };

        let mut next_config = (*self.state.app_config).clone();
        next_config.data.schema_dir = request.schema_dir.clone();
        if let Some(data_file) = &request.data_file {
            next_config.data.import_file = data_file.clone();
        }
        if let Err(error) = self.state.app_config.persist_settings(&next_config) {
            modal.update(cx, |modal, cx| {
                modal.set_busy(false, format!("Could not save world paths: {error}"), cx)
            });
            return;
        }
        self.state.schema_dir = request.schema_dir.clone();
        if let Some(data_file) = &request.data_file {
            self.state.data_file = data_file.clone();
        }
        self.state.app_config = Arc::new(next_config);

        if let Some(previous) = self.import_cancellation.take() {
            previous.supersede();
        }
        if let Some(previous) = self.import_task.take() {
            previous.detach();
        }
        self.import_generation = self.import_generation.wrapping_add(1);
        let generation = self.import_generation;
        let cancellation = CancellationToken::new();
        self.import_cancellation = Some(cancellation.clone());
        let graph = self.state.graph.clone();
        let schema_dir = request.schema_dir;
        let data_file = request.data_file;
        modal.update(cx, |modal, cx| modal.set_busy(true, "Creating world…", cx));

        let task = cx.spawn(async move |this, cx| {
            let result: anyhow::Result<Option<u_forge_core::SetupResult>> = if let Some(data_file) =
                data_file.as_ref()
            {
                u_forge_core::import_schemas_and_data_with_cancellation(
                    &graph,
                    &schema_dir,
                    data_file,
                    cancellation.clone(),
                )
                .await
                .map(Some)
            } else {
                (|| -> anyhow::Result<Option<u_forge_core::SetupResult>> {
                    cancellation.check_cancelled()?;
                    let definition = u_forge_core::SchemaIngestion::load_schemas_from_directory(
                        &schema_dir,
                        "imported_schemas",
                        "1.0.0",
                    )?;
                    let manager = graph.get_schema_manager();
                    let _ = manager.delete_schema("default");
                    cancellation.check_cancelled()?;
                    manager.save_schema(&definition)?;
                    Ok(None)
                })()
            };
            this.update(cx, |view: &mut AppView, cx| {
                if generation != view.import_generation {
                    return;
                }
                view.import_cancellation = None;
                match result {
                    Ok(stats) => {
                        let imported_data = stats.is_some();
                        view.state.schema_loaded = true;
                        view.setup_panel
                            .update(cx, |panel, _cx| panel.set_schema_loaded(true));
                        view.world_setup = None;
                        view._world_setup_subs.clear();
                        view.refresh_snapshot(cx);
                        view.state.data_status = Some(match stats {
                            Some(stats) => format!(
                                "World created: {} items, {} relationships",
                                stats.objects_created, stats.relationships_created
                            ),
                            None => "World created with an empty schema-backed graph".to_string(),
                        });
                        if imported_data {
                            view.run_embedding_plan(EmbeddingPlan::embed_all(), cx);
                        }
                    }
                    Err(error) => {
                        if let Some(modal) = &view.world_setup {
                            modal.update(cx, |modal, cx| {
                                modal.set_busy(false, format!("World creation failed: {error}"), cx)
                            });
                        }
                    }
                }
                cx.notify();
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

        let result = self.node_editor.update(cx, |editor, cx| {
            if all {
                editor.save_dirty_tabs(cx)
            } else {
                editor.save_active_tab(cx)
            }
        });
        self.finish_editor_save(result, cx);
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
        let had_detailed_failure = result.has_failures();
        self.finish_editor_save(result, cx);

        if let Some(node_id) = node_id {
            let still_dirty = self
                .node_editor
                .read(cx)
                .tabs
                .iter()
                .any(|tab| tab.node_id == node_id && tab.dirty);
            if still_dirty {
                if !had_detailed_failure {
                    self.state.data_status = Some(
                        "Could not close the tab because its changes did not pass validation."
                            .to_string(),
                    );
                }
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

    fn finish_editor_save(&mut self, result: EditorSaveResult, cx: &mut Context<Self>) {
        if let Some(diagnostic) = result.user_diagnostic() {
            self.state.data_status = Some(diagnostic);
        } else if result.skipped_edges > 0 {
            self.state.data_status = Some(format!(
                "{} incomplete relationship(s) skipped — fill both endpoints before saving.",
                result.skipped_edges
            ));
        }

        // If any nodes were discarded, refresh the full snapshot.
        if !result.discarded_ids.is_empty() {
            eprintln!(
                "Discarded {} empty new node(s).",
                result.discarded_ids.len()
            );
            self.refresh_snapshot(cx);
        }

        if result.saved > 0 {
            eprintln!("Saved {} node(s).", result.saved);

            // Refresh snapshot fully when edges may have changed.
            self.refresh_snapshot(cx);

            // 3. Re-chunk and embed every saved node so semantic search stays current.
            if !result.saved_ids.is_empty() {
                self.run_embedding_plan(EmbeddingPlan::rechunk(result.saved_ids), cx);
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
}
