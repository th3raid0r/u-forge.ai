mod history;
mod render;
mod run;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    App, ClipboardItem, Context, Corner, Entity, EntityId, EventEmitter, FocusHandle, Focusable,
    ListAlignment, ListState, MouseButton, MouseDownEvent, Pixels, Point, Window, anchored,
    deferred, div, linear_color_stop, linear_gradient, list, prelude::*, px, relative, rgb, rgba,
};
use u_forge_agent::{AgentParams, GraphAgent, HistoryMessage, select_history_window};
use u_forge_core::{
    ChatDeviceConfig, ChatMessage, ChatRequest, EffectiveAgentBudget, LemonadeRuntime,
    LemonadeRuntimeLease, LemonadeRuntimeProfile, ModelLoadOptions, ReasoningPolicy, StreamToken,
    config::ReasoningControl,
    lemonade::{EffectiveChatLimits, LemonadeChatProvider, SelectedModel},
    queue::CancellationToken,
};

use crate::chat_history::{ChatHistoryStore, ChatSessionSummary, StoredChatMessage};
use crate::chat_message::{ChatMessageRole, ChatMessageView};
use crate::text_field::{TextFieldView, TextSubmit};
use crate::ui::components::Tooltip;
use crate::ui::icons::{Icon, IconName, IconSize};
use crate::ui::theme::UiTheme;

use run::{ChatRunEvent, ChatRunReducer, ChatRunTerminal};

// ── Events ────────────────────────────────────────────────────────────────────

pub(crate) struct ConnectRequested;
impl EventEmitter<ConnectRequested> for ChatPanel {}
pub(crate) struct ToggleAssistantZoomRequested;
impl EventEmitter<ToggleAssistantZoomRequested> for ChatPanel {}

// ── ChatPanel ───────────────────────────────────────────────────────────────

struct ContextMenuState {
    position: Point<Pixels>,
    text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum ProfileReloadState {
    #[default]
    Ready,
    Reloading,
    Failed(String),
}

impl ProfileReloadState {
    fn is_reloading(&self) -> bool {
        matches!(self, Self::Reloading)
    }

    fn error(&self) -> Option<&str> {
        match self {
            Self::Failed(error) => Some(error),
            Self::Ready | Self::Reloading => None,
        }
    }
}

fn assistant_controls_locked(
    streaming: bool,
    _connecting: bool,
    _reload_state: &ProfileReloadState,
) -> bool {
    streaming
}

struct QueuedSend {
    text: String,
    history: Vec<HistoryMessage>,
    cancellation: CancellationToken,
}

pub(crate) struct ChatPanel {
    focus: FocusHandle,
    zoomed: bool,
    /// The text input field for composing messages.
    input_field: Entity<TextFieldView>,
    /// When true, pressing Enter submits; Shift+Enter inserts a newline.
    /// When false, Enter inserts a newline; Shift+Enter (or button) submits.
    enter_to_submit: bool,
    /// Chat message entities. Each message owns its own rendering state so
    /// streaming token deltas only invalidate the target entity, not the panel.
    messages: Vec<Entity<ChatMessageView>>,
    /// Whether a response is currently streaming.
    streaming: bool,
    /// Handle to the active stream task. Dropping it cancels the outer async
    /// consumer, which closes the mpsc::Receiver and causes the next tx.send()
    /// inside prompt_stream to return Err — breaking the stream loop.
    stream_task: Option<gpui::Task<()>>,
    /// Explicit owner for runtime loading, provider streams, and agent tools.
    stream_cancellation: Option<CancellationToken>,
    /// Normalizes both transports and admits exactly one terminal transition.
    run_reducer: ChatRunReducer,
    /// During streaming: handle to the Thinking message entity being appended to
    /// (lazily created on the first ReasoningDelta / Thinking token).
    streaming_thinking: Option<Entity<ChatMessageView>>,
    /// During streaming: handle to the current Assistant message entity being
    /// appended to (lazily created on the first TextDelta / Content token; reset
    /// after each tool call so the next text creates a new message).
    streaming_assistant: Option<Entity<ChatMessageView>>,
    /// Assistant row shown immediately while model/capability readiness is
    /// being awaited. The first semantic event reuses or removes this row.
    pending_assistant: Option<Entity<ChatMessageView>>,
    /// Drives the lightweight dot animation in the pending Assistant row.
    pending_animation_task: Option<gpui::Task<()>>,
    /// During streaming: tool-call entities indexed by their internal_id, so
    /// `ToolResult` events can target the right entry directly.
    streaming_tool_calls: HashMap<String, Entity<ChatMessageView>>,
    /// Available LLM models (populated after Lemonade init).
    available_models: Vec<AvailableModel>,
    /// Index into `available_models` for the currently selected model.
    selected_model_idx: usize,
    /// Whether the model selector dropdown is open.
    model_dropdown_open: bool,
    /// Whether the compact Assistant toolbar overflow menu is open.
    toolbar_menu_open: bool,
    /// The active chat provider for direct streaming (None until Lemonade is discovered).
    chat_provider: Option<LemonadeChatProvider>,
    /// Serializes model/reasoning profile reloads before inference.
    runtime: Option<Arc<LemonadeRuntime>>,
    /// Lemonade applies this mode only after a full profile reload.
    reasoning_enabled: bool,
    reasoning_control: ReasoningControl,
    /// User-visible lifecycle for explicit model/reasoning profile activation.
    profile_reload_state: ProfileReloadState,
    /// Retains the GPUI task so activation is not cancelled when the method returns.
    profile_reload_task: Option<gpui::Task<()>>,
    /// True while a do_init_lemonade call is in flight (after ConnectRequested emitted).
    connecting: bool,
    /// Brief error string shown under the button when the last connect attempt failed.
    connect_error: Option<String>,
    /// Rig agent with graph search tools (None until Lemonade + graph are wired up).
    /// When present, messages are routed through the agent loop instead of direct streaming.
    agent: Option<Arc<GraphAgent>>,
    /// Metadata makes the chrome usable immediately, but sends wait here until
    /// retrieval and reranking finish activating so tool-capable models do not
    /// accidentally fall back to direct chat during startup.
    capabilities_loading: bool,
    queued_send: Option<QueuedSend>,
    /// System prompt from config.
    system_prompt: String,
    /// Tokio runtime for async chat calls.
    tokio_rt: Arc<tokio::runtime::Runtime>,
    /// Subscription for the Enter-submit event from the input field.
    #[allow(dead_code)]
    submit_sub: gpui::Subscription,
    /// Virtualized list state for the message area (only renders visible items).
    list_state: ListState,
    /// Virtualized list state for the session history dropdown. Prevents
    /// O(N) row allocation per frame when the dropdown is open and the user
    /// has accumulated many sessions.
    history_list_state: ListState,
    /// Chat history persistence store (None if DB couldn't be opened).
    history_store: Option<ChatHistoryStore>,
    /// ID of the currently active chat session.
    current_session_id: Option<String>,
    /// Cached list of session summaries for the dropdown.
    session_list: Vec<ChatSessionSummary>,
    /// Whether the history selector dropdown is open.
    history_dropdown_open: bool,
    /// Index of the history row currently under the pointer, for gradient sync.
    hovered_history_ix: Option<usize>,
    /// CPU time (µs) spent building the element tree in the last render call.
    pub(crate) last_render_us: u64,
    /// Active right-click context menu (position + text to copy).
    context_menu: Option<ContextMenuState>,
}

/// A simplified model entry for the UI dropdown.
#[derive(Debug, Clone)]
pub(crate) struct AvailableModel {
    pub(crate) model_id: String,
    pub(crate) checkpoint: String,
    pub(crate) recipe: String,
    pub(crate) backend: Option<String>,
    pub(crate) load_options: ModelLoadOptions,
    pub(crate) tool_capable: bool,
    pub(crate) reasoning_capable: bool,
    pub(crate) sampling: ChatDeviceConfig,
    pub(crate) effective_limits: Option<EffectiveChatLimits>,
    pub(crate) max_tool_turns: usize,
    pub(crate) agent_budget: EffectiveAgentBudget,
}

impl From<&SelectedModel> for AvailableModel {
    fn from(sel: &SelectedModel) -> Self {
        let mut load_options = sel.load_opts.clone();
        if sel.recipe == "llamacpp" {
            load_options.llamacpp_backend = sel.backend.clone();
        }
        Self {
            model_id: sel.model_id.clone(),
            checkpoint: sel.checkpoint.clone(),
            recipe: sel.recipe.clone(),
            backend: sel.backend.clone(),
            load_options,
            tool_capable: sel.tool_capable,
            reasoning_capable: sel.reasoning_capable,
            sampling: ChatDeviceConfig::default(),
            effective_limits: None,
            max_tool_turns: 5,
            agent_budget: EffectiveAgentBudget::default(),
        }
    }
}

impl AvailableModel {
    pub(crate) fn with_chat_profile(
        mut self,
        sampling: ChatDeviceConfig,
        effective_limits: Option<EffectiveChatLimits>,
        max_tool_turns: usize,
        agent_budget: EffectiveAgentBudget,
    ) -> Self {
        self.sampling = sampling;
        self.effective_limits = effective_limits;
        self.max_tool_turns = max_tool_turns;
        self.agent_budget = agent_budget;
        self
    }

    fn agent_params(&self) -> AgentParams {
        AgentParams {
            temperature: self.sampling.temperature.map(f64::from),
            max_tokens: self
                .effective_limits
                .as_ref()
                .map(|limits| limits.agent_generation as u64)
                .or_else(|| self.sampling.max_tokens.map(u64::from)),
            top_p: self.sampling.top_p.map(f64::from),
            top_k: self.sampling.top_k,
            min_p: self.sampling.min_p.map(f64::from),
            frequency_penalty: self.sampling.frequency_penalty.map(f64::from),
            presence_penalty: self.sampling.presence_penalty.map(f64::from),
            repetition_penalty: self.sampling.repetition_penalty.map(f64::from),
            seed: self.sampling.seed,
            stop: self.sampling.stop.clone(),
            max_tool_turns: self.max_tool_turns,
            budget: self.agent_budget.clone(),
        }
    }

    fn uses_gpu(&self) -> bool {
        self.recipe == "llamacpp"
            && u_forge_core::lemonade::selector::is_gpu_backend(self.backend.as_deref())
    }

    /// Keep registry identifiers recognizable while removing packaging noise
    /// that is not useful to a non-technical model picker.
    fn display_name(&self) -> String {
        let basename = self.model_id.rsplit('/').next().unwrap_or(&self.model_id);
        let without_extension = basename
            .strip_suffix(".gguf")
            .or_else(|| basename.strip_suffix(".GGUF"))
            .unwrap_or(basename);
        let without_packaging = without_extension
            .strip_suffix("-GGUF")
            .unwrap_or(without_extension);
        without_packaging.replace(['-', '_'], " ")
    }

    fn device_label(&self) -> &'static str {
        match self.recipe.as_str() {
            "flm" => "NPU",
            "llamacpp" => match self.backend.as_deref() {
                Some("cuda" | "rocm" | "vulkan" | "metal") => "GPU",
                _ => "CPU",
            },
            _ => "",
        }
    }

    fn picker_label(&self) -> String {
        let name = self.display_name();
        let device = self.device_label();
        if device.is_empty() {
            name
        } else {
            format!("{name} ({device})")
        }
    }
}

/// Provider metadata is rebuilt during settings changes and reconnects. Keep
/// the feature-scoped runtime when the underlying connection is unchanged so
/// it retains ownership of the currently active chat model and can release it
/// before the replacement model is loaded.
fn runtime_for_provider_refresh(
    current: Option<&Arc<LemonadeRuntime>>,
    replacement: Arc<LemonadeRuntime>,
) -> Arc<LemonadeRuntime> {
    current
        .filter(|runtime| Arc::ptr_eq(runtime.connection(), replacement.connection()))
        .cloned()
        .unwrap_or(replacement)
}

impl ChatPanel {
    pub(crate) fn new(
        system_prompt: String,
        db_path: &Path,
        tokio_rt: Arc<tokio::runtime::Runtime>,
        zoomed: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let input_field = cx.new(|cx| {
            let mut field = TextFieldView::new(true, "Type a message...", cx);
            field.submit_on_enter = true;
            field
        });

        let submit_sub = cx.subscribe(&input_field, |this: &mut Self, _, _ev: &TextSubmit, cx| {
            this.do_send(cx);
        });

        // Open chat history store (non-fatal if it fails).
        let history_store = match ChatHistoryStore::open(db_path) {
            Ok(store) => Some(store),
            Err(e) => {
                eprintln!("Warning: chat history unavailable: {e}");
                None
            }
        };

        // Load the session list and resume the most recent session if one exists.
        let session_list = history_store
            .as_ref()
            .and_then(|s| s.list_sessions().ok())
            .unwrap_or_default();

        let (current_session_id, messages): (Option<String>, Vec<Entity<ChatMessageView>>) =
            if let Some(first) = session_list.first() {
                let msgs = history_store
                    .as_ref()
                    .and_then(|s| s.load_messages(&first.id).ok())
                    .unwrap_or_default();
                let entities = msgs
                    .into_iter()
                    .map(|m| cx.new(|cx| ChatMessageView::from_stored(m, cx)))
                    .collect();
                (Some(first.id.clone()), entities)
            } else {
                (None, Vec::new())
            };
        let msg_count = messages.len();

        Self {
            focus: cx.focus_handle(),
            zoomed,
            input_field,
            enter_to_submit: true,
            messages,
            streaming: false,
            stream_task: None,
            stream_cancellation: None,
            run_reducer: ChatRunReducer::default(),
            streaming_thinking: None,
            streaming_assistant: None,
            pending_assistant: None,
            pending_animation_task: None,
            streaming_tool_calls: HashMap::new(),
            available_models: Vec::new(),
            selected_model_idx: 0,
            model_dropdown_open: false,
            toolbar_menu_open: false,
            chat_provider: None,
            runtime: None,
            reasoning_enabled: true,
            reasoning_control: ReasoningControl::Request,
            profile_reload_state: ProfileReloadState::Ready,
            profile_reload_task: None,
            connecting: false,
            connect_error: None,
            agent: None,
            capabilities_loading: false,
            queued_send: None,
            system_prompt,
            tokio_rt,
            submit_sub,
            list_state: ListState::new(msg_count, ListAlignment::Bottom, px(200.0)),
            history_list_state: ListState::new(session_list.len(), ListAlignment::Top, px(200.0)),
            history_store,
            current_session_id,
            session_list,
            history_dropdown_open: false,
            hovered_history_ix: None,
            last_render_us: 0,
            context_menu: None,
        }
    }

    pub(crate) fn set_zoomed(&mut self, zoomed: bool) {
        self.zoomed = zoomed;
    }

    /// Called from AppView after Lemonade init discovers LLM models.
    pub(crate) fn set_provider(
        &mut self,
        provider: LemonadeChatProvider,
        models: Vec<AvailableModel>,
        preferred_idx: usize,
        runtime: Arc<LemonadeRuntime>,
        reasoning_control: ReasoningControl,
    ) {
        self.available_models = models;
        self.selected_model_idx = preferred_idx;
        self.runtime = Some(runtime_for_provider_refresh(self.runtime.as_ref(), runtime));
        self.reasoning_control = reasoning_control;
        self.chat_provider = Some(provider);
        self.apply_selected_chat_profile();
        self.connecting = false;
        self.profile_reload_state = ProfileReloadState::Ready;
        self.profile_reload_task = None;
    }

    fn apply_selected_chat_profile(&mut self) {
        let Some(model) = self.available_models.get(self.selected_model_idx) else {
            return;
        };
        let tool_capable = model.tool_capable;
        let limits = model.effective_limits.clone();
        self.connect_error = (!tool_capable).then(|| {
            "Selected model does not advertise tool calling; using direct chat.".to_string()
        });
        if let Some(limits) = limits {
            if let Some(provider) = &mut self.chat_provider {
                provider.default_max_tokens =
                    limits.direct_generation.min(u32::MAX as usize) as u32;
            }
            if !limits.diagnostics.is_empty() {
                let diagnostics = limits.diagnostics.join("; ");
                self.connect_error = Some(match self.connect_error.take() {
                    Some(existing) => format!("{existing} {diagnostics}"),
                    None => diagnostics,
                });
            }
        }
    }

    fn selected_runtime_profile(&self) -> Option<LemonadeRuntimeProfile> {
        let model = self.available_models.get(self.selected_model_idx)?;
        let reasoning_enabled = self.reasoning_enabled && model.reasoning_capable;
        let device = match model.recipe.as_str() {
            "flm" => Some("npu".to_string()),
            "llamacpp" => Some(match model.backend.as_deref() {
                Some("cuda" | "rocm" | "vulkan" | "metal") => "gpu".to_string(),
                _ => "cpu".to_string(),
            }),
            _ => None,
        };
        Some(
            LemonadeRuntimeProfile::new(
                model.model_id.clone(),
                reasoning_enabled,
                model.load_options.clone(),
            )
            .with_checkpoint(model.checkpoint.clone())
            .with_backend_profile(model.recipe.clone(), model.backend.clone(), device)
            .with_reasoning(
                if reasoning_enabled {
                    ReasoningPolicy::Enabled
                } else {
                    ReasoningPolicy::Disabled
                },
                self.reasoning_control,
                model.reasoning_capable,
            ),
        )
    }

    fn selected_reasoning_enabled(&self) -> bool {
        self.reasoning_enabled
            && self
                .available_models
                .get(self.selected_model_idx)
                .is_some_and(|model| model.reasoning_capable)
    }

    fn controls_locked(&self) -> bool {
        assistant_controls_locked(self.streaming, self.connecting, &self.profile_reload_state)
    }

    /// Activate the currently selected model/reasoning profile. Model-picker
    /// changes call this eagerly so loading overlaps the user's think time;
    /// the send path still acquires the profile authoritatively before use.
    fn reload_selected_profile(&mut self, cx: &mut Context<Self>) {
        if self.controls_locked() {
            return;
        }
        let Some(runtime) = self.runtime.clone() else {
            self.profile_reload_state = ProfileReloadState::Failed(
                "The Assistant runtime is not available. Reconnect and try again.".to_string(),
            );
            cx.notify();
            return;
        };
        let Some(profile) = self.selected_runtime_profile() else {
            self.profile_reload_state = ProfileReloadState::Failed(
                "No Assistant model is selected. Run Setup and choose a model.".to_string(),
            );
            cx.notify();
            return;
        };

        self.model_dropdown_open = false;
        self.toolbar_menu_open = false;
        self.profile_reload_state = ProfileReloadState::Reloading;
        let tokio_rt = self.tokio_rt.clone();
        cx.notify();

        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { tokio_rt.block_on(runtime.activate(&profile)) })
                .await;
            this.update(cx, |view: &mut ChatPanel, cx| {
                view.profile_reload_task = None;
                view.profile_reload_state = match result {
                    Ok(_) => ProfileReloadState::Ready,
                    Err(error) => ProfileReloadState::Failed(format!(
                        "Could not load the selected Assistant model: {error:#}"
                    )),
                };
                cx.notify();
            })
            .ok();
        });
        self.profile_reload_task = Some(task);
    }

    /// Called from AppView once the graph, inference queue, and Lemonade URL
    /// are all available. Enables the agent tool-calling path.
    pub(crate) fn set_agent(&mut self, agent: GraphAgent) {
        self.agent = Some(Arc::new(agent));
    }

    pub(crate) fn begin_capability_initialization(&mut self) {
        self.capabilities_loading = true;
    }

    pub(crate) fn finish_capability_initialization(&mut self, cx: &mut Context<Self>) {
        self.capabilities_loading = false;
        let Some(queued) = self.queued_send.take() else {
            return;
        };
        if queued.cancellation.is_cancelled() {
            return;
        }
        self.start_profile_acquisition(queued.text, queued.history, queued.cancellation, cx);
    }

    pub(crate) fn set_connecting(&mut self, b: bool) {
        self.connecting = b;
        if b {
            self.connect_error = None;
        }
    }

    pub(crate) fn set_connect_failed(&mut self, msg: &str) {
        self.connecting = false;
        self.connect_error = Some(msg.to_string());
    }

    /// Full rebuild of the virtualized list state — invalidates all cached
    /// item measurements. Use only when messages are replaced wholesale
    /// (session switch, session delete, initial load).
    fn reset_list_state(&self) {
        self.list_state.reset(self.messages.len());
    }

    /// Append-only splice at the end of the list. Unlike `reset`, this only
    /// invalidates the measurement of the newly appended item — prior items
    /// keep their cached heights. Call after pushing one message onto
    /// `self.messages`.
    ///
    /// This is the critical difference between the pre-14d full-panel
    /// re-render pattern and a per-message cache: `reset(len)` blows away
    /// every prior item's measurement, forcing every visible message to
    /// re-render + re-lay-out on the next paint. `splice(end..end, 1)`
    /// preserves prior measurements, which is what actually keeps streaming
    /// and message-boundary transitions smooth.
    fn splice_appended(&self, prev_len: usize) {
        self.list_state.splice(prev_len..prev_len, 1);
    }

    /// Push a plain text message (User/Assistant/Thinking) and return its handle.
    fn push_text_message(
        &mut self,
        role: ChatMessageRole,
        text: String,
        cx: &mut Context<Self>,
    ) -> Entity<ChatMessageView> {
        let msg = cx.new(|cx| ChatMessageView::new_text(role, text, cx));
        let prev_len = self.messages.len();
        self.messages.push(msg.clone());
        self.splice_appended(prev_len);
        msg
    }

    /// Push a tool-call message and return its handle.
    fn push_tool_call_message(
        &mut self,
        internal_id: String,
        name: String,
        args: String,
        cx: &mut Context<Self>,
    ) -> Entity<ChatMessageView> {
        let msg = cx.new(|cx| ChatMessageView::new_tool_call(internal_id, name, args, cx));
        let prev_len = self.messages.len();
        self.messages.push(msg.clone());
        self.splice_appended(prev_len);
        msg
    }

    fn stop_stream(&mut self, cx: &mut Context<Self>) {
        if let Some(cancellation) = self.stream_cancellation.take() {
            cancellation.cancel();
        }
        self.stream_task.take();
        self.apply_run_event(ChatRunEvent::Terminal(ChatRunTerminal::Cancelled), cx);
    }

    fn start_pending_assistant(&mut self, cx: &mut Context<Self>) {
        let message = self.push_text_message(
            ChatMessageRole::Assistant,
            "Preparing response…".to_string(),
            cx,
        );
        let pending_id = message.entity_id();
        self.pending_assistant = Some(message.clone());
        self.pending_animation_task = Some(cx.spawn(async move |this, cx| {
            let frames = [
                "Preparing response",
                "Preparing response.",
                "Preparing response..",
                "Preparing response…",
            ];
            let mut frame = 0usize;
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(350))
                    .await;
                let Some(this) = this.upgrade() else { return };
                let keep_running = this
                    .update(cx, |view: &mut ChatPanel, cx| {
                        let Some(pending) = view.pending_assistant.as_ref() else {
                            return false;
                        };
                        if pending.entity_id() != pending_id {
                            return false;
                        }
                        frame = (frame + 1) % frames.len();
                        message.update(cx, |message, cx| {
                            message.replace_text(frames[frame], cx);
                        });
                        true
                    })
                    .unwrap_or(false);
                if !keep_running {
                    return;
                }
            }
        }));
    }

    /// Re-run the user turn at or before `msg_entity_id`.
    /// If the clicked message is itself a User message, replays it directly.
    /// Otherwise walks backwards to find the nearest preceding User message.
    fn retry_message(&mut self, msg_entity_id: EntityId, cx: &mut Context<Self>) {
        if self.controls_locked() {
            tracing::debug!("retry_message suppressed: Assistant is busy");
            return;
        }

        let msg_idx = match self
            .messages
            .iter()
            .position(|m| m.entity_id() == msg_entity_id)
        {
            Some(idx) => idx,
            None => return,
        };

        let user_idx = if self.messages[msg_idx].read(cx).role == ChatMessageRole::User {
            msg_idx
        } else {
            match (0..msg_idx)
                .rev()
                .find(|&i| self.messages[i].read(cx).role == ChatMessageRole::User)
            {
                Some(idx) => idx,
                None => return,
            }
        };

        let user_text = self.messages[user_idx].read(cx).text().to_string();

        // Truncate from the user message onward (inclusive); send_with_text re-pushes it.
        self.messages.truncate(user_idx);
        self.list_state.reset(self.messages.len());

        self.send_with_text(user_text, cx);
    }

    /// Send the current input. Routes to the agent loop when an agent is
    /// available, otherwise falls back to direct LLM streaming.
    fn do_send(&mut self, cx: &mut Context<Self>) {
        if self.profile_reload_state.is_reloading() {
            return;
        }
        if self.streaming {
            return;
        }
        let has_provider = self.chat_provider.is_some() || self.agent.is_some();
        if !has_provider {
            if !self.connecting {
                cx.emit(ConnectRequested);
            }
            return;
        }
        let text = self.input_field.read(cx).content.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.input_field.update(cx, |field, cx| {
            field.set_content("", cx);
        });
        self.send_with_text(text, cx);
    }

    /// Core send path. Pushes the user message, sets streaming state, and
    /// spawns the agent or direct stream. Used by both `do_send` and
    /// `retry_message`.
    fn send_with_text(&mut self, text: String, cx: &mut Context<Self>) {
        self.model_dropdown_open = false;
        self.history_dropdown_open = false;
        self.toolbar_menu_open = false;
        if let Some(previous) = self.stream_cancellation.take() {
            previous.supersede();
            self.stream_task.take();
            self.apply_run_event(ChatRunEvent::Terminal(ChatRunTerminal::Superseded), cx);
        }
        self.run_reducer.begin();
        let cancellation = CancellationToken::new();
        self.stream_cancellation = Some(cancellation.clone());

        // Capture prior history before inserting the current turn, then update
        // the UI synchronously. Model/profile acquisition happens afterwards.
        let raw_history: Vec<HistoryMessage> = self
            .messages
            .iter()
            .filter_map(|message| {
                let message = message.read(cx);
                match message.role {
                    ChatMessageRole::User => Some(HistoryMessage {
                        role: "user".to_string(),
                        content: message.text().to_string(),
                    }),
                    ChatMessageRole::Assistant => Some(HistoryMessage {
                        role: "assistant".to_string(),
                        content: message.text().to_string(),
                    }),
                    _ => None,
                }
            })
            .collect();
        let history = raw_history;
        self.push_text_message(ChatMessageRole::User, text.clone(), cx);
        self.start_pending_assistant(cx);
        self.streaming = true;
        cx.notify();

        if self.capabilities_loading {
            self.queued_send = Some(QueuedSend {
                text,
                history,
                cancellation,
            });
            return;
        }

        self.start_profile_acquisition(text, history, cancellation, cx);
    }

    fn start_profile_acquisition(
        &mut self,
        text: String,
        history: Vec<HistoryMessage>,
        cancellation: CancellationToken,
        cx: &mut Context<Self>,
    ) {
        let Some(runtime) = self.runtime.clone() else {
            self.start_send_with_text(text, history, None, cancellation, cx);
            return;
        };
        let Some(profile) = self.selected_runtime_profile() else {
            self.start_send_with_text(text, history, None, cancellation, cx);
            return;
        };
        let tokio_rt = self.tokio_rt.clone();
        let task = cx.spawn(async move |this, cx| {
            let acquire_cancellation = cancellation.clone();
            let lease = cx
                .background_executor()
                .spawn(async move {
                    tokio_rt.block_on(async move {
                        tokio::select! {
                            _ = acquire_cancellation.cancelled() => None,
                            result = runtime.acquire_with_cancellation(
                                &profile,
                                &acquire_cancellation,
                            ) => Some(result),
                        }
                    })
                })
                .await;
            this.update(cx, |view: &mut ChatPanel, cx| match lease {
                Some(Ok(lease)) => {
                    view.start_send_with_text(text, history, Some(lease), cancellation, cx)
                }
                Some(Err(error)) => {
                    view.profile_reload_state = ProfileReloadState::Failed(format!(
                        "Could not load the selected Assistant model: {error:#}"
                    ));
                    view.apply_run_event(
                        ChatRunEvent::Terminal(ChatRunTerminal::RuntimeFailure(format!(
                            "{error:#}"
                        ))),
                        cx,
                    );
                }
                None => {}
            })
            .ok();
        });
        self.stream_task = Some(task);
    }

    fn start_send_with_text(
        &mut self,
        text: String,
        history: Vec<HistoryMessage>,
        runtime_lease: Option<LemonadeRuntimeLease>,
        cancellation: CancellationToken,
        cx: &mut Context<Self>,
    ) {
        // ── Agent path ────────────────────────────────────────────────────────
        // When a GraphAgent is wired up, route through the tool-calling loop
        // with streaming output. Tool calls appear as collapsible entries.
        let selected_supports_tools = self
            .available_models
            .get(self.selected_model_idx)
            .is_some_and(|model| model.tool_capable);
        if let Some(agent) = self.agent.clone().filter(|_| selected_supports_tools) {
            let selected_model = self.available_models[self.selected_model_idx].clone();
            let model_id = selected_model.model_id.clone();
            let agent_params = selected_model.agent_params();
            let uses_gpu = selected_model.uses_gpu();
            let reasoning_enabled = self.selected_reasoning_enabled();
            let tokio_rt = self.tokio_rt.clone();
            let stream_cancellation = cancellation.clone();

            let task = cx.spawn(async move |this, cx| {
                // Get the mpsc::Receiver on the background executor.
                let mut rx = cx
                    .background_executor()
                    .spawn(async move {
                        tokio_rt.block_on(agent.prompt_stream_with_profile_and_cancellation(
                            &model_id,
                            &text,
                            &history,
                            if reasoning_enabled {
                                ReasoningPolicy::Enabled
                            } else {
                                ReasoningPolicy::Disabled
                            },
                            agent_params,
                            runtime_lease,
                            uses_gpu,
                            cancellation,
                        ))
                    })
                    .await;

                use u_forge_agent::AgentStreamEvent;
                loop {
                    let event = cx
                        .background_executor()
                        .spawn(async move {
                            let e = rx.recv().await;
                            (rx, e)
                        })
                        .await;
                    rx = event.0;
                    match event.1 {
                        None => {
                            let terminal =
                                ChatRunTerminal::for_closed_stream(&stream_cancellation, "Agent");
                            this.update(cx, |view: &mut ChatPanel, cx| {
                                view.apply_run_event(ChatRunEvent::Terminal(terminal), cx);
                            })
                            .ok();
                            break;
                        }
                        Some(AgentStreamEvent::ReasoningDelta(delta)) => {
                            this.update(cx, |view: &mut ChatPanel, cx| {
                                view.apply_run_event(ChatRunEvent::ReasoningDelta(delta), cx);
                            })
                            .ok();
                        }
                        Some(AgentStreamEvent::TextDelta(delta)) => {
                            this.update(cx, |view: &mut ChatPanel, cx| {
                                view.apply_run_event(ChatRunEvent::TextDelta(delta), cx);
                            })
                            .ok();
                        }
                        Some(AgentStreamEvent::ToolCallStart {
                            internal_id,
                            name,
                            args_display,
                        }) => {
                            this.update(cx, |view: &mut ChatPanel, cx| {
                                view.apply_run_event(
                                    ChatRunEvent::ToolCallStart {
                                        internal_id,
                                        name,
                                        args_display,
                                    },
                                    cx,
                                );
                            })
                            .ok();
                        }
                        Some(AgentStreamEvent::ToolResult {
                            internal_id,
                            content,
                        }) => {
                            this.update(cx, |view: &mut ChatPanel, cx| {
                                view.apply_run_event(
                                    ChatRunEvent::ToolResult {
                                        internal_id,
                                        content,
                                    },
                                    cx,
                                );
                            })
                            .ok();
                        }
                        Some(AgentStreamEvent::Usage(_))
                        | Some(AgentStreamEvent::AgentDiagnostics(_)) => {}
                        Some(AgentStreamEvent::BudgetTerminated {
                            reason,
                            diagnostics,
                        }) => {
                            this.update(cx, |view: &mut ChatPanel, cx| {
                                view.apply_run_event(
                                    ChatRunEvent::Terminal(ChatRunTerminal::BudgetStop {
                                        reason,
                                        model_calls: diagnostics.model_calls,
                                        request_tokens: diagnostics.request_tokens,
                                        tool_output_tokens: diagnostics.tool_output_tokens,
                                    }),
                                    cx,
                                );
                            })
                            .ok();
                            break;
                        }
                        Some(AgentStreamEvent::RepeatTerminated {
                            reason,
                            diagnostics,
                        }) => {
                            this.update(cx, |view: &mut ChatPanel, cx| {
                                view.apply_run_event(
                                    ChatRunEvent::Terminal(ChatRunTerminal::RepeatStop {
                                        reason,
                                        model_calls: diagnostics.model_calls,
                                    }),
                                    cx,
                                );
                            })
                            .ok();
                            break;
                        }
                        Some(AgentStreamEvent::Finished { full_text, .. }) => {
                            this.update(cx, |view: &mut ChatPanel, cx| {
                                view.apply_run_event(
                                    ChatRunEvent::Terminal(ChatRunTerminal::Success { full_text }),
                                    cx,
                                );
                            })
                            .ok();
                            break;
                        }
                        Some(AgentStreamEvent::FatalError(e)) => {
                            this.update(cx, |view: &mut ChatPanel, cx| {
                                view.apply_run_event(
                                    ChatRunEvent::Terminal(ChatRunTerminal::AgentFailure(e)),
                                    cx,
                                );
                            })
                            .ok();
                            break;
                        }
                    }
                }
            });
            self.stream_task = Some(task);
            return;
        }

        // ── Direct streaming path (fallback when no agent is configured) ──────
        let provider = match &self.chat_provider {
            Some(p) => p.clone(),
            None => {
                self.apply_run_event(ChatRunEvent::Terminal(ChatRunTerminal::Unavailable), cx);
                return;
            }
        };

        let selected_model = self.available_models.get(self.selected_model_idx).cloned();
        let active_context = selected_model
            .as_ref()
            .and_then(|model| model.effective_limits.as_ref())
            .map_or(usize::MAX, |limits| limits.context);
        let history = select_history_window(&history, &self.system_prompt, &text, active_context);

        // Build the message list for the API (system prompt + windowed history).
        let mut api_messages = Vec::new();
        if !self.system_prompt.is_empty() {
            api_messages.push(ChatMessage::system(&self.system_prompt));
        }
        for msg in &history {
            match msg.role.as_str() {
                "user" => api_messages.push(ChatMessage::user(&msg.content)),
                "system" => api_messages.push(ChatMessage::system(&msg.content)),
                _ => api_messages.push(ChatMessage::assistant(&msg.content)),
            }
        }
        api_messages.push(ChatMessage::user(&text));

        // Determine model override if user selected a different model.
        let mut req =
            ChatRequest::new(api_messages).with_thinking(self.selected_reasoning_enabled());
        if let Some(model) = selected_model {
            req = req
                .with_model(model.model_id)
                .with_sampling(&model.sampling);
            if let Some(limits) = model.effective_limits {
                req = req.with_max_tokens(limits.direct_generation.min(u32::MAX as usize) as u32);
            }
        }

        let tokio_rt = self.tokio_rt.clone();
        let provider_cancellation = cancellation.clone();
        let stream_cancellation = cancellation;

        // Spawn a background task to drive the stream.
        let task = cx.spawn(async move |this, cx| {
            let rx = cx
                .background_executor()
                .spawn(async move {
                    tokio_rt.block_on(async {
                        match runtime_lease {
                            Some(lease) => provider.complete_stream_with_lease_and_cancellation(
                                req,
                                lease,
                                provider_cancellation,
                            ),
                            None => provider
                                .complete_stream_with_cancellation(req, provider_cancellation),
                        }
                    })
                })
                .await;

            // Consume tokens from the stream.
            let mut rx = rx;
            let mut finished = false;
            loop {
                let token = cx
                    .background_executor()
                    .spawn(async move {
                        let result = rx.recv().await;
                        (rx, result)
                    })
                    .await;
                rx = token.0;
                let result = token.1;

                match result {
                    Some(Ok(StreamToken::Content(text))) => {
                        this.update(cx, |view: &mut ChatPanel, cx| {
                            view.apply_run_event(ChatRunEvent::TextDelta(text), cx);
                        })
                        .ok();
                    }
                    Some(Ok(StreamToken::Thinking(text))) => {
                        this.update(cx, |view: &mut ChatPanel, cx| {
                            view.apply_run_event(ChatRunEvent::ReasoningDelta(text), cx);
                        })
                        .ok();
                    }
                    Some(Ok(StreamToken::FinishReason(_reason))) => finished = true,
                    Some(Ok(StreamToken::Usage(_usage))) => {}
                    Some(Err(e)) => {
                        let terminal = ChatRunTerminal::for_stream_error(&stream_cancellation, &e);
                        this.update(cx, |view: &mut ChatPanel, cx| {
                            view.apply_run_event(ChatRunEvent::Terminal(terminal), cx);
                        })
                        .ok();
                        break;
                    }
                    None => {
                        let terminal = if finished {
                            ChatRunTerminal::Success { full_text: None }
                        } else {
                            ChatRunTerminal::for_closed_stream(
                                &stream_cancellation,
                                "Direct provider",
                            )
                        };
                        this.update(cx, |view: &mut ChatPanel, cx| {
                            view.apply_run_event(ChatRunEvent::Terminal(terminal), cx);
                        })
                        .ok();
                        break;
                    }
                }
            }
        });
        self.stream_task = Some(task);
    }

    /// Label for the currently selected model (or a placeholder).
    fn selected_model_label(&self) -> String {
        if self.available_models.is_empty() {
            return "No models".to_string();
        }
        self.available_models[self.selected_model_idx].picker_label()
    }
}

impl Drop for ChatPanel {
    fn drop(&mut self) {
        if let Some(cancellation) = self.stream_cancellation.take() {
            cancellation.cancel();
        }
        self.stream_task.take();
    }
}

impl Focusable for ChatPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Modifiers, TestAppContext};

    use super::*;

    fn picker_test_model(model_id: &str) -> AvailableModel {
        AvailableModel {
            model_id: model_id.into(),
            checkpoint: "checkpoint".into(),
            recipe: "llamacpp".into(),
            backend: Some("vulkan".into()),
            load_options: ModelLoadOptions::default(),
            tool_capable: true,
            reasoning_capable: true,
            sampling: ChatDeviceConfig::default(),
            effective_limits: None,
            max_tool_turns: 5,
            agent_budget: EffectiveAgentBudget::default(),
        }
    }

    #[test]
    fn provider_refresh_retains_runtime_for_the_same_connection() {
        let connection = Arc::new(
            u_forge_core::lemonade::LemonadeConnection::external("http://127.0.0.1:1/v1").unwrap(),
        );
        let current = Arc::new(LemonadeRuntime::from_connection(connection.clone()));
        let replacement = Arc::new(LemonadeRuntime::from_connection(connection));

        let retained = runtime_for_provider_refresh(Some(&current), replacement);

        assert!(Arc::ptr_eq(&retained, &current));

        let other_connection = Arc::new(
            u_forge_core::lemonade::LemonadeConnection::external("http://127.0.0.1:2/v1").unwrap(),
        );
        let other = Arc::new(LemonadeRuntime::from_connection(other_connection));
        let replaced = runtime_for_provider_refresh(Some(&current), other.clone());

        assert!(Arc::ptr_eq(&replaced, &other));
    }

    #[gpui::test]
    fn model_selector_press_is_not_closed_by_the_input_area(cx: &mut TestAppContext) {
        cx.update(UiTheme::init);
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().to_path_buf();
        let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
        let (panel, cx) = cx.add_window_view(move |_window, cx| {
            let mut panel = ChatPanel::new("Test assistant".into(), &db_path, runtime, false, cx);
            panel.available_models = vec![picker_test_model("publisher/Gemma-4-E4B-it-GGUF")];
            panel.chat_provider = Some(LemonadeChatProvider::new(
                "http://127.0.0.1:1/v1",
                "Gemma-4-E4B-it-GGUF",
                None,
            ));
            panel
        });
        cx.update(|window, _app| window.refresh());
        cx.run_until_parked();

        let selector = cx.debug_bounds("model-selector-btn").unwrap();
        cx.simulate_mouse_down(selector.center(), MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        assert!(cx.update(|_window, app| panel.read(app).model_dropdown_open));
        assert!(cx.debug_bounds("model-dropdown").is_some());
    }

    #[gpui::test]
    fn model_option_click_eagerly_starts_profile_activation(cx: &mut TestAppContext) {
        cx.update(UiTheme::init);
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().to_path_buf();
        let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
        let (panel, cx) = cx.add_window_view(move |_window, cx| {
            let mut panel = ChatPanel::new("Test assistant".into(), &db_path, runtime, false, cx);
            panel.available_models = vec![
                picker_test_model("publisher/old-GGUF"),
                picker_test_model("publisher/replacement-GGUF"),
            ];
            panel.chat_provider = Some(LemonadeChatProvider::new(
                "http://127.0.0.1:1/v1",
                "old-GGUF",
                None,
            ));
            panel
        });
        cx.update(|window, _app| window.refresh());
        cx.run_until_parked();

        let selector = cx.debug_bounds("model-selector-btn").unwrap();
        cx.simulate_mouse_down(selector.center(), MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        let replacement = cx.debug_bounds("model-option-1").unwrap();
        cx.simulate_mouse_down(replacement.center(), MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        cx.update(|_window, app| {
            let panel = panel.read(app);
            assert_eq!(panel.selected_model_idx, 1);
            assert!(!panel.model_dropdown_open);
            assert!(matches!(
                panel.profile_reload_state,
                ProfileReloadState::Failed(ref error)
                    if error.contains("runtime is not available")
            ));
        });
    }

    #[gpui::test]
    fn send_renders_immediately_while_capabilities_finish_loading(cx: &mut TestAppContext) {
        cx.update(UiTheme::init);
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().to_path_buf();
        let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
        let (panel, cx) = cx.add_window_view(move |_window, cx| {
            let mut panel = ChatPanel::new("Test assistant".into(), &db_path, runtime, false, cx);
            panel.begin_capability_initialization();
            panel
        });

        cx.update(|_window, app| {
            panel.update(app, |panel, cx| {
                panel.send_with_text("hello".into(), cx);
                assert!(panel.streaming);
                assert!(panel.queued_send.is_some());
                assert_eq!(panel.messages.len(), 2);
                assert_eq!(panel.messages[0].read(cx).role, ChatMessageRole::User);
                assert_eq!(panel.messages[1].read(cx).role, ChatMessageRole::Assistant);
                panel.stop_stream(cx);
            });
        });
    }

    #[gpui::test]
    fn terminal_agent_text_replaces_pending_row_when_no_deltas_arrive(cx: &mut TestAppContext) {
        cx.update(UiTheme::init);
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().to_path_buf();
        let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
        let (panel, cx) = cx.add_window_view(move |_window, cx| {
            ChatPanel::new("Test assistant".into(), &db_path, runtime, false, cx)
        });

        cx.update(|_window, app| {
            panel.update(app, |panel, cx| {
                panel.streaming = true;
                panel.start_pending_assistant(cx);
                panel.apply_run_event(
                    ChatRunEvent::Terminal(ChatRunTerminal::Success {
                        full_text: Some("CUDA answer".into()),
                    }),
                    cx,
                );

                assert!(!panel.streaming);
                assert_eq!(panel.messages.len(), 1);
                assert_eq!(panel.messages[0].read(cx).text(), "CUDA answer");
            });
        });
    }

    #[gpui::test]
    fn terminal_agent_text_does_not_duplicate_streamed_deltas(cx: &mut TestAppContext) {
        cx.update(UiTheme::init);
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().to_path_buf();
        let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
        let (panel, cx) = cx.add_window_view(move |_window, cx| {
            ChatPanel::new("Test assistant".into(), &db_path, runtime, false, cx)
        });

        cx.update(|_window, app| {
            panel.update(app, |panel, cx| {
                panel.streaming = true;
                let message = panel.push_text_message(
                    ChatMessageRole::Assistant,
                    "streamed answer".into(),
                    cx,
                );
                panel.streaming_assistant = Some(message);
                panel.apply_run_event(
                    ChatRunEvent::Terminal(ChatRunTerminal::Success {
                        full_text: Some("streamed answer".into()),
                    }),
                    cx,
                );

                assert_eq!(panel.messages.len(), 1);
                assert_eq!(panel.messages[0].read(cx).text(), "streamed answer");
            });
        });
    }

    #[gpui::test]
    fn normalized_run_events_preserve_tool_order_and_finalize_once(cx: &mut TestAppContext) {
        cx.update(UiTheme::init);
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().to_path_buf();
        let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
        let (panel, cx) = cx.add_window_view(move |_window, cx| {
            ChatPanel::new("Test assistant".into(), &db_path, runtime, false, cx)
        });

        cx.update(|_window, app| {
            panel.update(app, |panel, cx| {
                panel.run_reducer.begin();
                panel.streaming = true;
                panel.start_pending_assistant(cx);
                panel.apply_run_event(ChatRunEvent::TextDelta("before".into()), cx);
                panel.apply_run_event(
                    ChatRunEvent::ToolCallStart {
                        internal_id: "call-1".into(),
                        name: "search_graph".into(),
                        args_display: "{}".into(),
                    },
                    cx,
                );
                panel.apply_run_event(
                    ChatRunEvent::ToolResult {
                        internal_id: "call-1".into(),
                        content: "found".into(),
                    },
                    cx,
                );
                panel.apply_run_event(ChatRunEvent::TextDelta("after".into()), cx);
                panel.apply_run_event(
                    ChatRunEvent::Terminal(ChatRunTerminal::Success { full_text: None }),
                    cx,
                );

                assert!(!panel.streaming);
                assert_eq!(panel.messages.len(), 3);
                assert_eq!(panel.messages[0].read(cx).role, ChatMessageRole::Assistant);
                assert_eq!(panel.messages[0].read(cx).text(), "before");
                assert_eq!(panel.messages[1].read(cx).role, ChatMessageRole::ToolCall);
                assert_eq!(panel.messages[2].read(cx).role, ChatMessageRole::Assistant);
                assert_eq!(panel.messages[2].read(cx).text(), "after");
                let session_id = panel.current_session_id.clone();
                let session_count = panel.session_list.len();

                panel.apply_run_event(
                    ChatRunEvent::Terminal(ChatRunTerminal::ProviderFailure("late".into())),
                    cx,
                );
                assert_eq!(panel.current_session_id, session_id);
                assert_eq!(panel.session_list.len(), session_count);
                assert_eq!(panel.messages.len(), 3);
            });
        });
    }

    #[test]
    fn available_model_maps_one_profile_to_rig_parameters() {
        let model = AvailableModel {
            model_id: "model".into(),
            checkpoint: "checkpoint".into(),
            recipe: "llamacpp".into(),
            backend: Some("vulkan".into()),
            load_options: ModelLoadOptions::default(),
            tool_capable: true,
            reasoning_capable: true,
            sampling: ChatDeviceConfig {
                temperature: Some(0.25),
                top_p: Some(0.8),
                repetition_penalty: Some(1.1),
                max_tokens: Some(999),
                ..ChatDeviceConfig::default()
            },
            effective_limits: Some(EffectiveChatLimits {
                load_context: Some(4096),
                context: 4096,
                direct_generation: 400,
                agent_generation: 300,
                diagnostics: Vec::new(),
            }),
            max_tool_turns: 7,
            agent_budget: u_forge_core::AgentBudgetConfig::default()
                .reconcile(4096, 7)
                .unwrap(),
        };
        let params = model.agent_params();
        assert_eq!(params.max_tokens, Some(300));
        assert_eq!(params.temperature, Some(0.25));
        assert!((params.top_p.unwrap() - 0.8).abs() < 1e-6);
        assert!((params.repetition_penalty.unwrap() - 1.1).abs() < 1e-6);
        assert_eq!(params.max_tool_turns, 7);
        assert_eq!(params.budget.context_tokens, 4096);
        assert!(model.uses_gpu());
    }

    #[test]
    fn assistant_controls_only_lock_during_an_active_response() {
        assert!(!assistant_controls_locked(
            false,
            false,
            &ProfileReloadState::Ready
        ));
        assert!(assistant_controls_locked(
            true,
            false,
            &ProfileReloadState::Ready
        ));
        assert!(!assistant_controls_locked(
            false,
            true,
            &ProfileReloadState::Ready
        ));
        assert!(!assistant_controls_locked(
            false,
            false,
            &ProfileReloadState::Reloading
        ));
        assert!(!assistant_controls_locked(
            false,
            false,
            &ProfileReloadState::Failed("load failed".into())
        ));
    }

    #[test]
    fn model_picker_removes_registry_packaging_noise() {
        assert_eq!(
            picker_test_model("publisher/Gemma-4-26B-A4B-it-GGUF").picker_label(),
            "Gemma 4 26B A4B it (GPU)"
        );
    }
}
