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
    ChatDeviceConfig, ChatMessage, ChatRequest, LemonadeRuntime, LemonadeRuntimeLease,
    LemonadeRuntimeProfile, ModelLoadOptions, ReasoningPolicy, StreamToken,
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
    connecting: bool,
    reload_state: &ProfileReloadState,
) -> bool {
    streaming || connecting || reload_state.is_reloading()
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
    /// During streaming: handle to the Thinking message entity being appended to
    /// (lazily created on the first ReasoningDelta / Thinking token).
    streaming_thinking: Option<Entity<ChatMessageView>>,
    /// During streaming: handle to the current Assistant message entity being
    /// appended to (lazily created on the first TextDelta / Content token; reset
    /// after each tool call so the next text creates a new message).
    streaming_assistant: Option<Entity<ChatMessageView>>,
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
    /// System prompt from config.
    system_prompt: String,
    /// Total context-window token budget (from `[chat] max_context_tokens`).
    max_context_tokens: usize,
    /// Tokens reserved for the model's response (from `[chat] response_reserve`).
    response_reserve: usize,
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
}

impl From<&SelectedModel> for AvailableModel {
    fn from(sel: &SelectedModel) -> Self {
        let mut load_options = sel.load_opts.clone();
        if sel.recipe == "llamacpp" {
            load_options.llamacpp_backend = sel.backend.clone();
        }
        if let (Some(configured), Some(maximum)) = (load_options.ctx_size, sel.max_context_window) {
            load_options.ctx_size = Some(configured.min(maximum));
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
        }
    }
}

impl AvailableModel {
    pub(crate) fn with_chat_profile(
        mut self,
        sampling: ChatDeviceConfig,
        effective_limits: Option<EffectiveChatLimits>,
        max_tool_turns: usize,
    ) -> Self {
        self.sampling = sampling;
        self.effective_limits = effective_limits;
        self.max_tool_turns = max_tool_turns;
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
        }
    }

    fn uses_gpu(&self) -> bool {
        self.recipe == "llamacpp"
            && matches!(self.backend.as_deref(), Some("rocm" | "vulkan" | "metal"))
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
                Some("rocm" | "vulkan" | "metal") => "GPU",
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

impl ChatPanel {
    pub(crate) fn new(
        system_prompt: String,
        max_context_tokens: usize,
        response_reserve: usize,
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
            streaming_thinking: None,
            streaming_assistant: None,
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
            system_prompt,
            max_context_tokens,
            response_reserve,
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
        self.runtime = Some(runtime);
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
            self.max_context_tokens = limits.context;
            self.response_reserve = limits.response_reserve;
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
                Some("rocm" | "vulkan" | "metal") => "gpu".to_string(),
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

    /// Activate the currently selected model/reasoning profile immediately.
    /// This makes a selector change observable at the point of interaction,
    /// rather than deferring an activation failure until the user sends text.
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

    /// Finalize streaming state and persist the session.
    fn finalize_stream(&mut self, cx: &mut Context<Self>) {
        self.streaming = false;
        self.stream_task = None;
        self.stream_cancellation = None;
        self.streaming_thinking = None;
        self.streaming_assistant = None;
        self.streaming_tool_calls.clear();
        self.save_current_session(cx);
        cx.notify();
    }

    fn stop_stream(&mut self, cx: &mut Context<Self>) {
        if let Some(cancellation) = self.stream_cancellation.take() {
            cancellation.cancel();
        }
        self.stream_task.take();
        if let Some(msg) = self.streaming_assistant.clone() {
            msg.update(cx, |m, cx| m.append_text("\n[Cancelled]", cx));
        }
        self.finalize_stream(cx);
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
        }
        let cancellation = CancellationToken::new();
        self.stream_cancellation = Some(cancellation.clone());
        let Some(runtime) = self.runtime.clone() else {
            self.start_send_with_text(text, None, cancellation, cx);
            return;
        };
        let Some(profile) = self.selected_runtime_profile() else {
            self.start_send_with_text(text, None, cancellation, cx);
            return;
        };
        let tokio_rt = self.tokio_rt.clone();
        self.streaming = true;
        cx.notify();
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
                Some(Ok(lease)) => view.start_send_with_text(text, Some(lease), cancellation, cx),
                Some(Err(error)) => {
                    view.profile_reload_state = ProfileReloadState::Failed(format!(
                        "Could not load the selected Assistant model: {error:#}"
                    ));
                    view.push_text_message(ChatMessageRole::User, text, cx);
                    view.push_text_message(
                        ChatMessageRole::Assistant,
                        format!("Runtime profile reload failed: {error:#}"),
                        cx,
                    );
                    view.finalize_stream(cx);
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
        runtime_lease: Option<LemonadeRuntimeLease>,
        cancellation: CancellationToken,
        cx: &mut Context<Self>,
    ) {
        // Build token-windowed history from prior User/Assistant turns before
        // recording the current prompt. GraphAgent receives the current prompt
        // separately, so including it here would send it twice.
        // each message entity once up-front so we don't need cx access later.
        let raw_history: Vec<HistoryMessage> = self
            .messages
            .iter()
            .filter_map(|h| {
                let m = h.read(cx);
                match m.role {
                    ChatMessageRole::User => Some(HistoryMessage {
                        role: "user".to_string(),
                        content: m.text().to_string(),
                    }),
                    ChatMessageRole::Assistant => Some(HistoryMessage {
                        role: "assistant".to_string(),
                        content: m.text().to_string(),
                    }),
                    _ => None,
                }
            })
            .collect();
        let history = select_history_window(
            &raw_history,
            &self.system_prompt,
            &text,
            self.max_context_tokens,
            self.response_reserve,
        );

        // Record the user message immediately after history has been captured.
        self.push_text_message(ChatMessageRole::User, text.clone(), cx);
        self.streaming = true;
        cx.notify();

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
                            // Channel closed without Done — treat as finished.
                            this.update(cx, |view: &mut ChatPanel, cx| {
                                view.finalize_stream(cx);
                            })
                            .ok();
                            break;
                        }
                        Some(AgentStreamEvent::ReasoningDelta(delta)) => {
                            this.update(cx, |view: &mut ChatPanel, cx| {
                                let msg = view.streaming_thinking.clone().unwrap_or_else(|| {
                                    let m = view.push_text_message(
                                        ChatMessageRole::Thinking,
                                        String::new(),
                                        cx,
                                    );
                                    view.streaming_thinking = Some(m.clone());
                                    m
                                });
                                msg.update(cx, |m, cx| m.append_text(&delta, cx));
                            })
                            .ok();
                        }
                        Some(AgentStreamEvent::TextDelta(delta)) => {
                            this.update(cx, |view: &mut ChatPanel, cx| {
                                let msg = view.streaming_assistant.clone().unwrap_or_else(|| {
                                    let m = view.push_text_message(
                                        ChatMessageRole::Assistant,
                                        String::new(),
                                        cx,
                                    );
                                    view.streaming_assistant = Some(m.clone());
                                    m
                                });
                                msg.update(cx, |m, cx| m.append_text(&delta, cx));
                            })
                            .ok();
                        }
                        Some(AgentStreamEvent::ToolCallStart {
                            internal_id,
                            name,
                            args_display,
                        }) => {
                            this.update(cx, |view: &mut ChatPanel, cx| {
                                let msg = view.push_tool_call_message(
                                    internal_id.clone(),
                                    name,
                                    args_display,
                                    cx,
                                );
                                view.streaming_tool_calls.insert(internal_id, msg);
                                // Reset the current assistant so the next text
                                // delta creates a fresh message after the tool.
                                view.streaming_assistant = None;
                                cx.notify();
                            })
                            .ok();
                        }
                        Some(AgentStreamEvent::ToolResult {
                            internal_id,
                            content,
                        }) => {
                            this.update(cx, |view: &mut ChatPanel, _cx| {
                                if let Some(msg) =
                                    view.streaming_tool_calls.get(&internal_id).cloned()
                                {
                                    msg.update(_cx, |m, cx| m.set_tool_result(content, cx));
                                }
                            })
                            .ok();
                        }
                        Some(AgentStreamEvent::Usage(_)) => {}
                        Some(AgentStreamEvent::Finished { .. }) => {
                            this.update(cx, |view: &mut ChatPanel, cx| {
                                view.finalize_stream(cx);
                            })
                            .ok();
                            break;
                        }
                        Some(AgentStreamEvent::FatalError(e)) => {
                            this.update(cx, |view: &mut ChatPanel, cx| {
                                let msg = view.streaming_assistant.clone().unwrap_or_else(|| {
                                    let m = view.push_text_message(
                                        ChatMessageRole::Assistant,
                                        String::new(),
                                        cx,
                                    );
                                    view.streaming_assistant = Some(m.clone());
                                    m
                                });
                                msg.update(cx, |m, cx| {
                                    m.append_error(&format!("\n[Agent error: {e}]"), cx)
                                });
                                view.finalize_stream(cx);
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
                self.push_text_message(
                    ChatMessageRole::Assistant,
                    "Chat unavailable — Lemonade Server not connected.".to_string(),
                    cx,
                );
                self.streaming = false;
                cx.notify();
                return;
            }
        };

        // Build the message list for the API (system prompt + windowed history).
        let mut api_messages = Vec::new();
        if !self.system_prompt.is_empty() {
            api_messages.push(ChatMessage::system(&self.system_prompt));
        }
        for msg in &history {
            match msg.role.as_str() {
                "user" => api_messages.push(ChatMessage::user(&msg.content)),
                _ => api_messages.push(ChatMessage::assistant(&msg.content)),
            }
        }
        api_messages.push(ChatMessage::user(&text));

        // Determine model override if user selected a different model.
        let selected_model = self.available_models.get(self.selected_model_idx).cloned();

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
                            let msg = view.streaming_assistant.clone().unwrap_or_else(|| {
                                let m = view.push_text_message(
                                    ChatMessageRole::Assistant,
                                    String::new(),
                                    cx,
                                );
                                view.streaming_assistant = Some(m.clone());
                                m
                            });
                            msg.update(cx, |m, cx| m.append_text(&text, cx));
                        })
                        .ok();
                    }
                    Some(Ok(StreamToken::Thinking(text))) => {
                        this.update(cx, |view: &mut ChatPanel, cx| {
                            let msg = view.streaming_thinking.clone().unwrap_or_else(|| {
                                let m = view.push_text_message(
                                    ChatMessageRole::Thinking,
                                    String::new(),
                                    cx,
                                );
                                view.streaming_thinking = Some(m.clone());
                                m
                            });
                            msg.update(cx, |m, cx| m.append_text(&text, cx));
                        })
                        .ok();
                    }
                    Some(Ok(StreamToken::FinishReason(_reason))) => {}
                    Some(Ok(StreamToken::Usage(_usage))) => {}
                    Some(Err(e)) => {
                        this.update(cx, |view: &mut ChatPanel, cx| {
                            let msg = view.streaming_assistant.clone().unwrap_or_else(|| {
                                let m = view.push_text_message(
                                    ChatMessageRole::Assistant,
                                    String::new(),
                                    cx,
                                );
                                view.streaming_assistant = Some(m.clone());
                                m
                            });
                            msg.update(cx, |m, cx| m.append_error(&format!("\n[Error: {e}]"), cx));
                            view.finalize_stream(cx);
                        })
                        .ok();
                        break;
                    }
                    None => {
                        // Stream finished.
                        this.update(cx, |view: &mut ChatPanel, cx| {
                            view.finalize_stream(cx);
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

    // ── Chat history methods ────────────────────────────────────────────────

    /// Title for the current session (for the header dropdown button).
    fn session_title(&self) -> String {
        if let Some(sid) = &self.current_session_id {
            self.session_list
                .iter()
                .find(|s| s.id == *sid)
                .map(|s| s.title.clone())
                .unwrap_or_else(|| "Assistant".to_string())
        } else {
            "New Chat".to_string()
        }
    }

    /// Save the current messages to the active session (creating one if needed).
    fn save_current_session(&mut self, cx: &mut Context<Self>) {
        let store = match &self.history_store {
            Some(s) => s.clone(),
            None => return,
        };

        // Derive a title from the first user message.
        let title = self
            .messages
            .iter()
            .find_map(|h| {
                let m = h.read(cx);
                if matches!(m.role, ChatMessageRole::User) {
                    Some(m.text().to_string())
                } else {
                    None
                }
            })
            .map(|t| {
                let t = t.trim();
                if t.len() > 60 {
                    format!("{}…", &t[..60])
                } else {
                    t.to_string()
                }
            })
            .unwrap_or_else(|| "New Chat".to_string());

        // Ensure we have a session ID.
        let session_id = match &self.current_session_id {
            Some(id) => id.clone(),
            None => match store.create_session(&title) {
                Ok(id) => {
                    self.current_session_id = Some(id.clone());
                    id
                }
                Err(e) => {
                    eprintln!("Failed to create chat session: {e}");
                    return;
                }
            },
        };

        let stored: Vec<StoredChatMessage> = self
            .messages
            .iter()
            .map(|h| h.read(cx).to_stored())
            .collect();
        if let Err(e) = store.save_session(&session_id, &title, &stored) {
            eprintln!("Failed to save chat session: {e}");
        }

        // Refresh the cached session list.
        self.session_list = store.list_sessions().unwrap_or_default();
        self.history_list_state.reset(self.session_list.len());
    }

    /// Start a new empty chat session.
    ///
    /// No-op while streaming: swapping `self.messages` mid-stream would cause
    /// in-flight `TextDelta` / `ReasoningDelta` events to append to the new
    /// session and `finalize_stream` to save the polluted list under the
    /// wrong session_id. The Send button is already gated on `streaming`.
    fn new_session(&mut self, cx: &mut Context<Self>) {
        if self.controls_locked() {
            tracing::debug!("new_session suppressed: Assistant is busy");
            return;
        }
        // Save current session before switching.
        if !self.messages.is_empty() {
            self.save_current_session(cx);
        }

        self.messages.clear();
        self.current_session_id = None;
        self.history_dropdown_open = false;
        self.toolbar_menu_open = false;
        self.reset_list_state();
        cx.notify();
    }

    /// Switch to an existing session by ID.
    ///
    /// No-op while streaming — see `new_session` for the race it prevents.
    fn load_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        if self.controls_locked() {
            tracing::debug!(%session_id, "load_session suppressed: Assistant is busy");
            return;
        }
        // Save current session before switching.
        if !self.messages.is_empty() {
            self.save_current_session(cx);
        }

        let store = match &self.history_store {
            Some(s) => s.clone(),
            None => return,
        };

        match store.load_messages(session_id) {
            Ok(msgs) => {
                self.messages = msgs
                    .into_iter()
                    .map(|m| cx.new(|cx| ChatMessageView::from_stored(m, cx)))
                    .collect();
                self.current_session_id = Some(session_id.to_string());
                self.history_dropdown_open = false;
                self.reset_list_state();
                cx.notify();
            }
            Err(e) => {
                eprintln!("Failed to load chat session: {e}");
            }
        }
    }

    /// Delete the message at `ix` from the current session.
    ///
    /// No-op while streaming — the last message during streaming is the
    /// in-flight assistant response; allowing delete mid-stream would race
    /// with `append_text`. The button is also not rendered when streaming.
    fn delete_message_at(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.streaming {
            tracing::debug!(ix, "delete_message_at suppressed: stream in progress");
            return;
        }
        if ix >= self.messages.len() {
            return;
        }
        self.messages.remove(ix);
        self.list_state.reset(self.messages.len());
        self.save_current_session(cx);
        cx.notify();
    }

    /// Delete a session from history. If it's the current session, clear the chat.
    ///
    /// Deleting the **active** session while streaming is suppressed (it would
    /// clear `self.messages` out from under the in-flight stream). Deleting a
    /// different session is always safe.
    fn delete_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        if self.streaming && self.current_session_id.as_deref() == Some(session_id) {
            tracing::debug!(%session_id, "delete_session suppressed: active session is streaming");
            return;
        }
        let store = match &self.history_store {
            Some(s) => s.clone(),
            None => return,
        };

        if let Err(e) = store.delete_session(session_id) {
            eprintln!("Failed to delete chat session: {e}");
            return;
        }

        // If we just deleted the active session, clear the chat.
        if self.current_session_id.as_deref() == Some(session_id) {
            self.messages.clear();
            self.current_session_id = None;
            self.reset_list_state();
        }

        // Refresh the cached session list.
        self.session_list = store.list_sessions().unwrap_or_default();
        self.history_list_state.reset(self.session_list.len());
        cx.notify();
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

impl Render for ChatPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let render_start = Instant::now();
        let theme = *UiTheme::get(cx);
        let panel_focused = self.focus.contains_focused(window, cx);
        let enter_to_submit = self.enter_to_submit;
        let streaming = self.streaming;
        let connecting = self.connecting;
        let profile_reloading = self.profile_reload_state.is_reloading();
        let controls_locked = self.controls_locked();
        let profile_reload_error = self.profile_reload_state.error().map(ToString::to_string);
        let connect_error = self.connect_error.clone();
        let model_dropdown_open = self.model_dropdown_open;
        let toolbar_menu_open = self.toolbar_menu_open;
        let reasoning_capable = self
            .available_models
            .get(self.selected_model_idx)
            .is_some_and(|model| model.reasoning_capable);
        let reasoning_enabled = self.reasoning_enabled && reasoning_capable;
        let has_provider = self.chat_provider.is_some() || self.agent.is_some();
        let model_label = self.selected_model_label();
        let history_dropdown_open = self.history_dropdown_open;
        let history_label = self.session_title();

        // Virtualized history dropdown list — only renders visible rows so the
        // dropdown stays cheap even with hundreds of accumulated sessions.
        let history_list_entity = cx.entity().clone();
        let history_list_el = list(
            self.history_list_state.clone(),
            move |ix, _window, cx: &mut App| {
                let panel = history_list_entity.read(cx);
                let Some(session) = panel.session_list.get(ix).cloned() else {
                    return div().into_any_element();
                };
                let is_current = panel.current_session_id.as_deref() == Some(&session.id);
                let is_hovered = panel.hovered_history_ix == Some(ix);
                let entity_load = history_list_entity.clone();
                let sid_load = session.id.clone();
                let entity_del = history_list_entity.clone();
                let sid_del = session.id.clone();
                let entity_hover = history_list_entity.clone();
                let title = session.title.clone();

                // Both gradient stops share the same hue (only alpha differs)
                // so the gradient is invisible over empty space and only masks
                // text that actually overflows. Colours are pre-composited
                // equivalents of the semi-transparent row backgrounds:
                //   selected: rgba(0x45475a88) over #313244 ≈ #3C3D50
                //   hovered:  rgba(0x45475a66) over #313244 ≈ #393A4D
                let (gradient_start, gradient_end) = if is_current {
                    (rgba(0x3c3d5000), rgba(0x3c3d50ff))
                } else if is_hovered {
                    (rgba(0x393a4d00), rgba(0x393a4dff))
                } else {
                    (rgba(0x31324400), rgba(0x313244ff))
                };

                div()
                    .id(("hist", ix))
                    .relative()
                    .w_full()
                    .overflow_x_hidden()
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(24.0))
                    .px_2()
                    .when(is_current, |el| el.bg(rgb(0x3c3d50)))
                    .hover(|s| s.bg(rgb(0x393a4d)))
                    .on_hover(move |is_hov, _window, cx| {
                        entity_hover.update(cx, |this, cx| {
                            this.hovered_history_ix = if *is_hov { Some(ix) } else { None };
                            cx.notify();
                        });
                    })
                    .child({
                        // No `relative()` here — keeping the title static avoids
                        // a separate stacking context that confuses the row's
                        // on_hover hit-testing when the cursor descends from above.
                        let mut title_el = div()
                            .id(("hist-title", ix))
                            .flex()
                            .items_center()
                            .min_w_0()
                            .text_sm()
                            .text_color(if is_current {
                                rgba(0xcdd6f4ff)
                            } else {
                                rgba(0xa6adc8ff)
                            })
                            .cursor_pointer()
                            .overflow_x_hidden()
                            .on_mouse_down(
                                MouseButton::Left,
                                move |_: &MouseDownEvent, _window, cx: &mut App| {
                                    entity_load.update(cx, |this, cx| {
                                        this.load_session(&sid_load, cx);
                                    });
                                },
                            )
                            .child(title);
                        title_el.style().flex_grow = Some(1.0);
                        title_el.style().flex_shrink = Some(1.0);
                        title_el
                    })
                    // Gradient is a row-level absolute child so it doesn't
                    // interfere with the title's hit-box. right(26) aligns its
                    // right edge with the delete button's left edge
                    // (8 px padding + 18 px button = 26 px from outer right).
                    .child(
                        div()
                            .absolute()
                            .right(px(26.0))
                            .top_0()
                            .h_full()
                            .w(px(28.0))
                            .bg(linear_gradient(
                                90.,
                                linear_color_stop(gradient_start, 0.0),
                                linear_color_stop(gradient_end, 1.0),
                            )),
                    )
                    .child(
                        div()
                            .id(("hist-del", ix))
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .w(px(18.0))
                            .h(px(18.0))
                            .rounded(px(2.0))
                            .text_sm()
                            .text_color(rgba(0xf38ba8aa))
                            .cursor_pointer()
                            .hover(|s| s.text_color(rgba(0xf38ba8ff)).bg(rgba(0x45475a66)))
                            .on_mouse_down(
                                MouseButton::Left,
                                move |_: &MouseDownEvent, _window, cx: &mut App| {
                                    entity_del.update(cx, |this, cx| {
                                        this.delete_session(&sid_del, cx);
                                    });
                                },
                            )
                            .tooltip(Tooltip::text("Delete conversation"))
                            .child(Icon::new(
                                IconName::Trash,
                                IconSize::Small,
                                rgba(0xf38ba8ff),
                            )),
                    )
                    .into_any_element()
            },
        );

        // Build the virtualized message list. Only visible items (+ overdraw
        // buffer) are rendered, so long chat histories don't slow down layout.
        // Each item is its own `Entity<ChatMessageView>` — streaming token
        // deltas only invalidate the target entity, not this panel.
        let list_entity = cx.entity().clone();
        let list_el = list(self.list_state.clone(), move |ix, _window, cx: &mut App| {
            let _span = tracing::trace_span!("chat_panel::list_item", ix).entered();
            let panel = list_entity.read(cx);
            let Some(msg) = panel.messages.get(ix).cloned() else {
                return div().into_any_element();
            };
            let is_last = ix + 1 == panel.messages.len();
            let is_streaming = panel.streaming;
            let controls_locked = panel.controls_locked();
            let role = msg.read(cx).role;
            let can_retry = !controls_locked
                && matches!(role, ChatMessageRole::User | ChatMessageRole::Assistant);
            let show_delete = is_last && !is_streaming;

            let mut row = div()
                .id(("msg-row", ix))
                .flex()
                .flex_col()
                .w_full()
                .child(msg.clone());

            // Right-click anywhere on the row to open the context menu.
            // Resolve copy text at click time (not render time) so an active
            // drag-selection in the body is captured — selection changes in
            // the body's TextFieldView only notify that entity, not the panel,
            // so a render-time snapshot would be stale.
            let ctx_entity = list_entity.clone();
            let msg_for_ctx = msg.clone();
            row = row.on_mouse_down(
                MouseButton::Right,
                move |event: &MouseDownEvent, _window, cx: &mut App| {
                    let pos = event.position;
                    let text = msg_for_ctx.read(cx).copy_text_for_context(cx);
                    ctx_entity.update(cx, |panel, cx| {
                        panel.context_menu = Some(ContextMenuState {
                            position: pos,
                            text,
                        });
                        cx.notify();
                    });
                },
            );

            if can_retry || show_delete {
                let del_entity = list_entity.clone();
                let retry_entity = list_entity.clone();
                let copy_entity = list_entity.clone();
                let msg_entity_id = panel.messages[ix].entity_id();
                let msg_for_copy = msg.clone();

                let bubble_bg = match role {
                    ChatMessageRole::User => rgb(0x313244),
                    ChatMessageRole::Thinking => rgb(0x181825),
                    ChatMessageRole::Assistant | ChatMessageRole::ToolCall => rgb(0x1e1e2e),
                };

                let mut action_bar = div()
                    .id(("action-bar", ix))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap_1()
                    .px_2()
                    .py(px(2.0))
                    .bg(bubble_bg);

                if can_retry {
                    action_bar = action_bar.child(
                        div()
                            .id(("retry", ix))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(theme.metrics.control_height_small)
                            .text_base()
                            .text_color(rgba(0x6c708688))
                            .cursor_pointer()
                            .hover(|s| s.text_color(rgba(0xcdd6f4ff)))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx: &mut App| {
                                retry_entity.update(cx, |this, cx| {
                                    this.retry_message(msg_entity_id, cx);
                                });
                            })
                            .tooltip(Tooltip::text("Try this response again"))
                            .child(Icon::new(
                                IconName::Refresh,
                                IconSize::Small,
                                rgba(0xa6adc8ff),
                            )),
                    );
                }

                // Copy button — always shown in the action bar.
                action_bar = action_bar.child(
                    div()
                        .id(("copy", ix))
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(theme.metrics.control_height_small)
                        .text_base()
                        .text_color(rgba(0x6c708688))
                        .cursor_pointer()
                        .hover(|s| s.text_color(rgba(0xcdd6f4ff)))
                        .on_mouse_down(MouseButton::Left, move |_, _, cx: &mut App| {
                            let text = msg_for_copy.read(cx).copy_text_for_context(cx);
                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                            copy_entity.update(cx, |panel, cx| {
                                panel.context_menu = None;
                                cx.notify();
                            });
                        })
                        .tooltip(Tooltip::text("Copy message"))
                        .child(Icon::new(IconName::Copy, IconSize::Small, rgba(0xa6adc8ff))),
                );

                if show_delete {
                    action_bar = action_bar.child(
                        div()
                            .id(("del", ix))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(theme.metrics.control_height_small)
                            .rounded(px(2.0))
                            .text_base()
                            .text_color(rgba(0xf38ba8aa))
                            .cursor_pointer()
                            .hover(|s| s.text_color(rgba(0xf38ba8ff)).bg(rgba(0x45475a66)))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx: &mut App| {
                                del_entity.update(cx, |this, cx| {
                                    let last = this.messages.len().saturating_sub(1);
                                    this.delete_message_at(last, cx);
                                });
                            })
                            .tooltip(Tooltip::text("Delete message"))
                            .child(Icon::new(
                                IconName::Trash,
                                IconSize::Small,
                                rgba(0xf38ba8ff),
                            )),
                    );
                }

                row = row.child(action_bar);
            }
            row.into_any_element()
        });

        // Model dropdown items.
        let model_items: Vec<_> = if model_dropdown_open {
            self.available_models
                .iter()
                .enumerate()
                .map(|(idx, m)| {
                    let is_selected = idx == self.selected_model_idx;
                    let label = m.picker_label();
                    div()
                        .id(("model", idx))
                        .flex()
                        .items_center()
                        .h(theme.metrics.control_height)
                        .px_2()
                        .text_sm()
                        .text_color(if is_selected {
                            rgba(0xcdd6f4ff)
                        } else {
                            rgba(0xa6adc8ff)
                        })
                        .when(is_selected, |el| el.bg(rgba(0x45475a88)))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgba(0x45475a66)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                                let changed =
                                    !this.controls_locked() && this.selected_model_idx != idx;
                                if changed {
                                    this.selected_model_idx = idx;
                                    this.apply_selected_chat_profile();
                                }
                                this.model_dropdown_open = false;
                                if changed {
                                    this.reload_selected_profile(cx);
                                } else {
                                    cx.notify();
                                }
                            }),
                        )
                        .child(label)
                })
                .collect()
        } else {
            Vec::new()
        };

        // ── Context menu state (captured before root building) ────────────────
        let ctx_pos = self.context_menu.as_ref().map(|m| m.position);
        let ctx_text = self
            .context_menu
            .as_ref()
            .map(|m| m.text.clone())
            .unwrap_or_default();
        let input_field_paste = self.input_field.clone();
        let ctx_copy_listener = cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(ctx_text.clone()));
            this.context_menu = None;
            cx.notify();
        });
        let ctx_paste_listener = cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
            if let Some(clip) = cx.read_from_clipboard()
                && let Some(text) = clip.text()
            {
                input_field_paste.update(cx, |tf, cx| {
                    tf.insert_at_cursor(&text, cx);
                });
            }
            this.context_menu = None;
            cx.notify();
        });

        let root = div()
            .id("chat-panel")
            .key_context("AssistantPanel")
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .w_full()
            .h_full()
            .min_h_0()
            .bg(rgb(0x181825))
            .border_l_1()
            .border_color(rgb(0x313244))
            .when(panel_focused, |panel| {
                panel.border_1().border_color(rgba(0xb4befeff))
            })
            // ── Header: chat history selector ────────────────────────────────
            .child(
                div()
                    .id("chat-header")
                    .flex()
                    .flex_col()
                    .flex_none()
                    .w_full()
                    .border_b_1()
                    .border_color(rgb(0x313244))
                    .child(
                        div()
                            .id("history-selector-row")
                            .flex()
                            .flex_row()
                            .flex_none()
                            .items_center()
                            .h(theme.metrics.panel_header_height)
                            .px_3()
                            .gap(px(theme.metrics.space_2))
                            .child(
                                div()
                                    .id("history-selector-btn")
                                    .flex()
                                    .items_center()
                                    .px_2()
                                    .h(theme.metrics.control_height_small)
                                    .bg(rgb(0x313244))
                                    .border_1()
                                    .border_color(rgb(0x45475a))
                                    .rounded(px(3.0))
                                    .text_sm()
                                    .text_color(if controls_locked {
                                        rgba(0x6c7086ff)
                                    } else {
                                        rgba(0xcdd6f4ff)
                                    })
                                    .overflow_x_hidden()
                                    .when(!controls_locked, |button| {
                                        button.cursor_pointer().on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                                this.history_dropdown_open =
                                                    !this.history_dropdown_open;
                                                this.model_dropdown_open = false;
                                                this.toolbar_menu_open = false;
                                                cx.notify();
                                            }),
                                        )
                                    })
                                    .child(history_label),
                            )
                            .child({
                                let mut spacer = div();
                                spacer.style().flex_grow = Some(1.0);
                                spacer
                            })
                            .child(
                                div()
                                    .id("assistant-zoom-btn")
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .justify_center()
                                    .px_2()
                                    .h(theme.metrics.control_height_small)
                                    .rounded(px(3.0))
                                    .text_sm()
                                    .text_color(rgba(0xa6adc8ff))
                                    .cursor_pointer()
                                    .tooltip(Tooltip::text(if self.zoomed {
                                        "Restore Assistant panel"
                                    } else {
                                        "Maximize Assistant panel"
                                    }))
                                    .hover(|style| style.bg(rgba(0x45475a88)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|_this, _: &MouseDownEvent, _window, cx| {
                                            cx.emit(ToggleAssistantZoomRequested);
                                        }),
                                    )
                                    .child(Icon::new(
                                        IconName::Maximize,
                                        IconSize::Medium,
                                        rgba(0xa6adc8ff),
                                    )),
                            )
                            .child(
                                div()
                                    .id("assistant-options-btn")
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .justify_center()
                                    .px_2()
                                    .h(theme.metrics.control_height_small)
                                    .rounded(px(3.0))
                                    .text_color(rgba(0xa6adc8ff))
                                    .cursor_pointer()
                                    .tooltip(Tooltip::text("More Assistant actions"))
                                    .hover(|style| style.bg(rgba(0x45475a88)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                            this.toolbar_menu_open = !this.toolbar_menu_open;
                                            this.history_dropdown_open = false;
                                            this.model_dropdown_open = false;
                                            cx.notify();
                                        }),
                                    )
                                    .child(Icon::new(
                                        IconName::MoreHorizontal,
                                        IconSize::Medium,
                                        rgba(0xa6adc8ff),
                                    )),
                            )
                            .child(
                                div()
                                    .id("new-chat-btn")
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .justify_center()
                                    .px_2()
                                    .h(theme.metrics.control_height_small)
                                    .bg(rgb(0xa6e3a1))
                                    .rounded(px(3.0))
                                    .text_sm()
                                    .text_color(if controls_locked {
                                        rgba(0x6c7086ff)
                                    } else {
                                        rgba(0x1e1e2eff)
                                    })
                                    .tooltip(Tooltip::text("Start a new conversation"))
                                    .when(!controls_locked, |button| {
                                        button.cursor_pointer().on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                                this.new_session(cx);
                                            }),
                                        )
                                    })
                                    .child(Icon::new(
                                        IconName::Plus,
                                        IconSize::Medium,
                                        if controls_locked {
                                            rgba(0x6c7086ff)
                                        } else {
                                            rgba(0x1e1e2eff)
                                        },
                                    )),
                            ),
                    )
                    .when(history_dropdown_open, |header| {
                        header.child(
                            div()
                                .id("history-dropdown")
                                .flex()
                                .flex_col()
                                .flex_none()
                                .w_full()
                                .bg(rgb(0x313244))
                                .border_1()
                                .border_color(rgb(0x45475a))
                                .rounded(px(3.0))
                                .mt_1()
                                .h(px(200.0))
                                .overflow_hidden()
                                .child({
                                    let mut el = history_list_el;
                                    el.style().flex_grow = Some(1.0);
                                    el.style().flex_shrink = Some(1.0);
                                    el.style().flex_basis = Some(relative(0.).into());
                                    el
                                }),
                        )
                    })
                    .when(toolbar_menu_open, |header| {
                        header.child(
                            div()
                                .id("assistant-options-menu")
                                .flex()
                                .flex_col()
                                .w_full()
                                .bg(rgb(0x313244))
                                .border_1()
                                .border_color(rgb(0x45475a))
                                .rounded(px(3.0))
                                .mt_1()
                                .child(
                                    div()
                                        .id("assistant-reload-model")
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .h(theme.metrics.control_height)
                                        .px_2()
                                        .text_sm()
                                        .text_color(if controls_locked || !has_provider {
                                            rgba(0x6c7086ff)
                                        } else {
                                            rgba(0xcdd6f4ff)
                                        })
                                        .when(!controls_locked && has_provider, |item| {
                                            item.cursor_pointer()
                                                .hover(|style| style.bg(rgba(0x45475a88)))
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(
                                                        |this, _: &MouseDownEvent, _window, cx| {
                                                            this.reload_selected_profile(cx);
                                                        },
                                                    ),
                                                )
                                        })
                                        .child(Icon::new(
                                            IconName::Refresh,
                                            IconSize::Medium,
                                            if controls_locked || !has_provider {
                                                rgba(0x6c7086ff)
                                            } else {
                                                rgba(0xa6adc8ff)
                                            },
                                        ))
                                        .child(if profile_reloading {
                                            "Reloading model…"
                                        } else {
                                            "Reload selected model"
                                        }),
                                )
                                .child(
                                    div()
                                        .id("assistant-options-zoom")
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .h(theme.metrics.control_height)
                                        .px_2()
                                        .text_sm()
                                        .text_color(rgba(0xcdd6f4ff))
                                        .cursor_pointer()
                                        .hover(|style| style.bg(rgba(0x45475a88)))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                                this.toolbar_menu_open = false;
                                                cx.emit(ToggleAssistantZoomRequested);
                                                cx.notify();
                                            }),
                                        )
                                        .child(Icon::new(
                                            IconName::Maximize,
                                            IconSize::Medium,
                                            rgba(0xa6adc8ff),
                                        ))
                                        .child(if self.zoomed {
                                            "Restore Assistant"
                                        } else {
                                            "Maximize Assistant"
                                        }),
                                ),
                        )
                    }),
            )
            // ── Message area (virtualized list) ──────────────────────────────
            .child({
                let mut msg_area = div()
                    .id("chat-messages")
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                            let mut changed = false;
                            if this.history_dropdown_open
                                || this.model_dropdown_open
                                || this.toolbar_menu_open
                            {
                                this.history_dropdown_open = false;
                                this.model_dropdown_open = false;
                                this.toolbar_menu_open = false;
                                changed = true;
                            }
                            if this.context_menu.is_some() {
                                this.context_menu = None;
                                changed = true;
                            }
                            if changed {
                                cx.notify();
                            }
                        }),
                    )
                    .child({
                        let mut el = list_el;
                        el.style().flex_grow = Some(1.0);
                        el.style().flex_shrink = Some(1.0);
                        el.style().flex_basis = Some(relative(0.).into());
                        el
                    });
                if streaming {
                    msg_area = msg_area.child(
                        div()
                            .id("streaming-indicator")
                            .flex_none()
                            .px_2()
                            .py_1()
                            .text_sm()
                            .text_color(rgba(0xf9e2afff))
                            .child("Generating…"),
                    );
                }
                msg_area.style().flex_grow = Some(1.0);
                msg_area.style().flex_shrink = Some(1.0);
                msg_area.style().flex_basis = Some(relative(0.).into());
                msg_area
            })
            // ── Input area ───────────────────────────────────────────────────
            .child(
                div()
                    .id("chat-input-area")
                    .flex()
                    .flex_col()
                    .flex_none()
                    .w_full()
                    .border_t_1()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                            let mut changed = false;
                            if this.history_dropdown_open
                                || this.model_dropdown_open
                                || this.toolbar_menu_open
                            {
                                this.history_dropdown_open = false;
                                this.model_dropdown_open = false;
                                this.toolbar_menu_open = false;
                                changed = true;
                            }
                            if this.context_menu.is_some() {
                                this.context_menu = None;
                                changed = true;
                            }
                            if changed {
                                cx.notify();
                            }
                        }),
                    )
                    // Right-click anywhere in the input area opens the context
                    // menu. Copy text = current selection in the input field (or
                    // empty, leaving only Paste useful). Message rows install
                    // their own Right handler that captures per-row text.
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                            let pos = event.position;
                            let text = this
                                .input_field
                                .read(cx)
                                .selected_text()
                                .unwrap_or_default();
                            this.context_menu = Some(ContextMenuState {
                                position: pos,
                                text,
                            });
                            cx.notify();
                        }),
                    )
                    .border_color(rgb(0x313244))
                    .px_2()
                    .py_1()
                    .gap(px(4.0))
                    .child(
                        div()
                            .id("submit-toggle-row")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .id("enter-toggle")
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .w(px(14.0))
                                    .h(px(14.0))
                                    .border_1()
                                    .border_color(rgb(0x45475a))
                                    .rounded(px(2.0))
                                    .cursor_pointer()
                                    .when(enter_to_submit, |el| el.bg(rgba(0x89b4faff)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                            this.enter_to_submit = !this.enter_to_submit;
                                            this.input_field.update(cx, |field, _cx| {
                                                field.submit_on_enter = this.enter_to_submit;
                                            });
                                            cx.notify();
                                        }),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgba(0x6c7086ff))
                                    .child("Enter to submit"),
                            ),
                    )
                    .child(
                        div()
                            .id("input-row")
                            .flex()
                            .flex_row()
                            .items_end()
                            .gap(px(4.0))
                            .child({
                                let mut field_container = div()
                                    .flex()
                                    .flex_col()
                                    .min_w_0()
                                    .child(self.input_field.clone());
                                field_container.style().flex_grow = Some(1.0);
                                field_container.style().flex_shrink = Some(1.0);
                                field_container.style().flex_basis = Some(relative(0.).into());
                                field_container
                            })
                            .child({
                                #[derive(Clone, Copy)]
                                enum BtnState {
                                    Connect,
                                    Connecting,
                                    Reloading,
                                    Send,
                                    Stop,
                                }
                                let btn_state = if streaming {
                                    BtnState::Stop
                                } else if profile_reloading {
                                    BtnState::Reloading
                                } else if connecting {
                                    BtnState::Connecting
                                } else if !has_provider {
                                    BtnState::Connect
                                } else {
                                    BtnState::Send
                                };
                                let (label, bg, fg) = match btn_state {
                                    BtnState::Connect => {
                                        ("Connect", rgb(0xf9e2af_u32), rgba(0x1e1e2eff_u32))
                                    }
                                    BtnState::Connecting => {
                                        ("Connecting…", rgb(0x45475a_u32), rgba(0x6c7086ff_u32))
                                    }
                                    BtnState::Reloading => {
                                        ("Loading…", rgb(0x45475a_u32), rgba(0x6c7086ff_u32))
                                    }
                                    BtnState::Send => {
                                        ("Send", rgb(0x89b4fa_u32), rgba(0x1e1e2eff_u32))
                                    }
                                    BtnState::Stop => {
                                        ("Stop", rgb(0xf38ba8_u32), rgba(0x1e1e2eff_u32))
                                    }
                                };
                                let button_child = if matches!(btn_state, BtnState::Send) {
                                    Icon::new(IconName::Send, IconSize::Large, fg)
                                        .rotate_degrees(90.0)
                                        .into_any_element()
                                } else {
                                    div().child(label).into_any_element()
                                };
                                div()
                                    .id("send-btn")
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .justify_center()
                                    .w(px(88.0))
                                    .h(theme.metrics.control_height)
                                    .bg(bg)
                                    .rounded(px(3.0))
                                    .text_base()
                                    .text_color(fg)
                                    .cursor_pointer()
                                    .tooltip(Tooltip::text(match btn_state {
                                        BtnState::Connect => "Connect the Assistant",
                                        BtnState::Connecting => "Connecting the Assistant",
                                        BtnState::Reloading => "Loading the selected model",
                                        BtnState::Send => "Send message",
                                        BtnState::Stop => "Stop response",
                                    }))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(
                                            move |this, _: &MouseDownEvent, _window, cx| {
                                                match btn_state {
                                                    BtnState::Connect => cx.emit(ConnectRequested),
                                                    BtnState::Connecting | BtnState::Reloading => {}
                                                    BtnState::Send => this.do_send(cx),
                                                    BtnState::Stop => this.stop_stream(cx),
                                                }
                                            },
                                        ),
                                    )
                                    .child(button_child)
                            }),
                    )
                    .when_some(connect_error, |el, err| {
                        el.child(
                            div()
                                .id("connect-error")
                                .text_base()
                                .text_color(rgba(0xf38ba8ff))
                                .child(err),
                        )
                    })
                    .when(profile_reloading, |el| {
                        el.child(
                            div()
                                .id("profile-reloading")
                                .text_sm()
                                .text_color(rgba(0xf9e2afff))
                                .child("Loading the selected Assistant model…"),
                        )
                    })
                    .when_some(profile_reload_error, |el, err| {
                        el.child(
                            div()
                                .id("profile-reload-error")
                                .text_sm()
                                .text_color(rgba(0xf38ba8ff))
                                .child(err),
                        )
                    })
                    .child(
                        div()
                            .id("model-selector-row")
                            .flex()
                            .flex_col()
                            .w_full()
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(4.0))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(rgba(0x6c7086ff))
                                            .child("Model:"),
                                    )
                                    .child(
                                        div()
                                            .id("model-selector-btn")
                                            .debug_selector(|| "model-selector-btn".to_string())
                                            .flex()
                                            .items_center()
                                            .px_2()
                                            .h(theme.metrics.control_height)
                                            .bg(rgb(0x313244))
                                            .border_1()
                                            .border_color(rgb(0x45475a))
                                            .rounded(px(3.0))
                                            .text_sm()
                                            .text_color(if has_provider && !controls_locked {
                                                rgba(0xcdd6f4ff)
                                            } else {
                                                rgba(0x6c7086ff)
                                            })
                                            .when(!controls_locked && has_provider, |toggle| {
                                                toggle.cursor_pointer().on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(
                                                        |this, _: &MouseDownEvent, _window, cx| {
                                                            // This control lives inside the input
                                                            // area's general menu-dismissal handler.
                                                            // Keep the opening press from bubbling
                                                            // up and immediately closing the menu.
                                                            cx.stop_propagation();
                                                            this.model_dropdown_open =
                                                                !this.model_dropdown_open;
                                                            this.history_dropdown_open = false;
                                                            this.toolbar_menu_open = false;
                                                            cx.notify();
                                                        },
                                                    ),
                                                )
                                            })
                                            .overflow_x_hidden()
                                            .child(model_label),
                                    )
                                    .child(
                                        div()
                                            .id("reasoning-toggle")
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .size(theme.metrics.control_height)
                                            .bg(if reasoning_enabled {
                                                rgb(0x45475a)
                                            } else {
                                                rgb(0x313244)
                                            })
                                            .border_1()
                                            .border_color(rgb(0x45475a))
                                            .rounded(px(3.0))
                                            .text_sm()
                                            .text_color(if reasoning_enabled {
                                                rgba(0xcdd6f4ff)
                                            } else {
                                                rgba(0x6c7086ff)
                                            })
                                            .tooltip(Tooltip::text(if !reasoning_capable {
                                                "Thinking follows this model's default"
                                            } else if reasoning_enabled {
                                                "Disable thinking"
                                            } else {
                                                "Enable thinking"
                                            }))
                                            .when(!controls_locked && reasoning_capable, |toggle| {
                                                toggle.cursor_pointer().on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(
                                                        |this, _: &MouseDownEvent, _window, cx| {
                                                            this.reasoning_enabled =
                                                                !this.reasoning_enabled;
                                                            if this.reasoning_control
                                                                == ReasoningControl::Reload
                                                            {
                                                                this.reload_selected_profile(cx);
                                                            } else {
                                                                this.profile_reload_state =
                                                                    ProfileReloadState::Ready;
                                                                cx.notify();
                                                            }
                                                        },
                                                    ),
                                                )
                                            })
                                            .child(Icon::new(
                                                IconName::Thinking,
                                                IconSize::Medium,
                                                if !reasoning_capable {
                                                    rgba(0xa6adc8ff)
                                                } else if reasoning_enabled {
                                                    rgba(0xa6e3a1ff)
                                                } else {
                                                    rgba(0xf38ba8ff)
                                                },
                                            )),
                                    ),
                            )
                            .when(model_dropdown_open, |container| {
                                container.child(
                                    div()
                                        .id("model-dropdown")
                                        .debug_selector(|| "model-dropdown".to_string())
                                        .flex()
                                        .flex_col()
                                        .w_full()
                                        .bg(rgb(0x313244))
                                        .border_1()
                                        .border_color(rgb(0x45475a))
                                        .rounded(px(3.0))
                                        .mt_1()
                                        .max_h(px(200.0))
                                        .overflow_y_scroll()
                                        .children(model_items),
                                )
                            }),
                    ),
            );
        // ── Right-click context menu overlay ─────────────────────────────────
        let root = root.when_some(ctx_pos, |root, pos| {
            root.child(deferred(
                anchored().position(pos).anchor(Corner::TopLeft).child(
                    div()
                        .id("ctx-menu")
                        .w(px(140.0))
                        .bg(rgb(0x313244))
                        .border_1()
                        .border_color(rgb(0x45475a))
                        .rounded(px(3.0))
                        .child(
                            div()
                                .id("ctx-copy")
                                .flex()
                                .items_center()
                                .h(px(24.0))
                                .px_3()
                                .text_sm()
                                .text_color(rgba(0xcdd6f4ff))
                                .cursor_pointer()
                                .hover(|s| s.bg(rgba(0x45475a88)))
                                .on_mouse_down(MouseButton::Left, ctx_copy_listener)
                                .child("Copy"),
                        )
                        .child(div().h(px(1.0)).w_full().bg(rgb(0x45475a)))
                        .child(
                            div()
                                .id("ctx-paste")
                                .flex()
                                .items_center()
                                .h(px(24.0))
                                .px_3()
                                .text_sm()
                                .text_color(rgba(0xcdd6f4ff))
                                .cursor_pointer()
                                .hover(|s| s.bg(rgba(0x45475a88)))
                                .on_mouse_down(MouseButton::Left, ctx_paste_listener)
                                .child("Paste"),
                        ),
                ),
            ))
        });

        self.last_render_us = render_start.elapsed().as_micros() as u64;
        root
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
        }
    }

    #[gpui::test]
    fn model_selector_press_is_not_closed_by_the_input_area(cx: &mut TestAppContext) {
        cx.update(UiTheme::init);
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().to_path_buf();
        let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
        let (panel, cx) = cx.add_window_view(move |_window, cx| {
            let mut panel = ChatPanel::new(
                "Test assistant".into(),
                4096,
                512,
                &db_path,
                runtime,
                false,
                cx,
            );
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
                response_reserve: 512,
                direct_generation: 400,
                agent_generation: 300,
                history_budget: 3584,
                diagnostics: Vec::new(),
            }),
            max_tool_turns: 7,
        };
        let params = model.agent_params();
        assert_eq!(params.max_tokens, Some(300));
        assert_eq!(params.temperature, Some(0.25));
        assert!((params.top_p.unwrap() - 0.8).abs() < 1e-6);
        assert!((params.repetition_penalty.unwrap() - 1.1).abs() < 1e-6);
        assert_eq!(params.max_tool_turns, 7);
        assert!(model.uses_gpu());
    }

    #[test]
    fn assistant_controls_lock_during_connection_streaming_and_profile_reload() {
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
        assert!(assistant_controls_locked(
            false,
            true,
            &ProfileReloadState::Ready
        ));
        assert!(assistant_controls_locked(
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
