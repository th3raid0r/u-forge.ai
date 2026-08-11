//! Reopenable Lemonade provisioning dialog.

use std::collections::BTreeMap;

use gpui::{
    Context, EventEmitter, MouseButton, MouseDownEvent, Render, Window, canvas, deferred, div,
    prelude::*, px, rgb, rgba,
};
use u_forge_core::{
    ChatDevice, ReasoningControl,
    lemonade::{
        DownloadAction, LemonadeOwnership, LemonadeServerCatalog, ManagementEventKind,
        ManagementProgressEvent, SetupComponentState, component_state, initial_setup_components,
        setup_chat_models,
    },
};

use crate::startup::{StartupMilestone, StartupTimeline};
use crate::ui::components::Tooltip;
use crate::ui::icons::{Icon, IconName, IconSize};
use crate::ui::theme::UiTheme;

#[derive(Debug, Clone)]
pub(crate) struct SetupRequested {
    pub(crate) chat_model: String,
    pub(crate) high_quality_embedding: bool,
    pub(crate) preferred_device: ChatDevice,
    pub(crate) reasoning_control: ReasoningControl,
    pub(crate) confirmed_external: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct SetupDownloadRequested {
    pub(crate) job_id: String,
    pub(crate) model_name: String,
    pub(crate) operation: SetupDownloadOperation,
    pub(crate) confirmed_external: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct SetupBackendInstallRequested {
    pub(crate) recipe: String,
    pub(crate) backend: String,
    pub(crate) confirmed_external: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum SetupDownloadOperation {
    Control(DownloadAction),
    Retry,
}

pub(crate) struct SetupRefreshRequested;
pub(crate) struct SetupClosed;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetupPage {
    Backends,
    Configuration,
}

#[derive(Debug, Clone)]
struct BackendRow {
    recipe: String,
    backend: String,
    state: String,
    message: String,
    action: String,
    version: Option<String>,
    devices: Vec<String>,
}

impl BackendRow {
    fn can_install(&self) -> bool {
        matches!(self.state.as_str(), "installable" | "update_required")
    }
}

#[derive(Debug, Clone)]
struct BackendGroup {
    recipe: String,
    rows: Vec<BackendRow>,
}

#[derive(Debug, Clone, Copy)]
enum BackendTagTone {
    Success,
    Info,
    Warning,
    Neutral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendCapability {
    Chat,
    Embeddings,
    Reranking,
    SpeechToText,
    TextToSpeech,
    ImageGeneration,
    MusicGeneration,
    SoundEffects,
    Routing,
    ThreeDGeneration,
    ConcurrentServing,
}

impl BackendCapability {
    fn label(self) -> &'static str {
        match self {
            Self::Chat => "Chat",
            Self::Embeddings => "Embeddings",
            Self::Reranking => "Reranking",
            Self::SpeechToText => "Speech to text",
            Self::TextToSpeech => "Text to speech",
            Self::ImageGeneration => "Image generation",
            Self::MusicGeneration => "Music generation",
            Self::SoundEffects => "Sound effects",
            Self::Routing => "Routing / classification",
            Self::ThreeDGeneration => "3D generation",
            Self::ConcurrentServing => "Multi-session throughput",
        }
    }

    fn is_currently_relevant(self) -> bool {
        matches!(self, Self::Chat | Self::Embeddings | Self::Reranking)
    }
}

#[derive(Debug, Clone)]
struct BackendTag {
    label: String,
    tone: BackendTagTone,
}

#[derive(Debug, Clone)]
struct ComponentRow {
    label: String,
    state: SetupComponentState,
    status: String,
    backend_ready: bool,
}

#[derive(Debug, Clone)]
struct ChatChoice {
    id: String,
    recipe: String,
    downloaded: bool,
    tools: bool,
    reasoning: bool,
}

impl ChatChoice {
    fn is_recommended(&self) -> bool {
        self.tools && self.reasoning
    }
}

#[derive(Debug, Clone)]
struct DownloadJob {
    id: String,
    label: String,
    status: String,
    progress: Option<f64>,
    file: Option<String>,
    bytes_downloaded: Option<u64>,
    bytes_total: Option<u64>,
    error: Option<String>,
}

pub(crate) struct SetupPanel {
    ownership: LemonadeOwnership,
    catalog: LemonadeServerCatalog,
    page: SetupPage,
    backend_rows: Vec<BackendRow>,
    show_all_backends: bool,
    component_rows: Vec<ComponentRow>,
    chat_models: Vec<ChatChoice>,
    selected_chat: usize,
    show_advanced_models: bool,
    chat_dropdown_open: bool,
    high_quality_embedding: bool,
    npu_embedding_enabled: bool,
    preferred_device: ChatDevice,
    reasoning_control: ReasoningControl,
    downloads: Vec<DownloadJob>,
    status: String,
    busy: bool,
    external_confirmation_armed: bool,
    startup: Option<StartupTimeline>,
}

impl EventEmitter<SetupRequested> for SetupPanel {}
impl EventEmitter<SetupBackendInstallRequested> for SetupPanel {}
impl EventEmitter<SetupDownloadRequested> for SetupPanel {}
impl EventEmitter<SetupRefreshRequested> for SetupPanel {}
impl EventEmitter<SetupClosed> for SetupPanel {}

impl SetupPanel {
    pub(crate) fn new(
        ownership: LemonadeOwnership,
        catalog: &LemonadeServerCatalog,
        selected_chat_model: Option<&str>,
        high_quality_embedding: bool,
        npu_embedding_enabled: bool,
        preferred_device: ChatDevice,
        reasoning_control: ReasoningControl,
    ) -> Self {
        let chat_models: Vec<_> = setup_chat_models(catalog)
            .into_iter()
            .map(|model| ChatChoice {
                id: model.id.clone(),
                recipe: model.recipe.clone(),
                downloaded: model.downloaded,
                tools: model.supports_tool_calling(),
                reasoning: model.supports_reasoning(),
            })
            .collect();
        let saved_selection = selected_chat_model
            .and_then(|selected| chat_models.iter().position(|model| model.id == selected));
        let selected_chat = saved_selection
            .filter(|index| chat_models[*index].is_recommended())
            .or_else(|| {
                chat_models
                    .iter()
                    .position(|model| model.is_recommended() && model.downloaded)
            })
            .or_else(|| chat_models.iter().position(ChatChoice::is_recommended))
            .unwrap_or(0);
        let show_advanced_models = !chat_models.iter().any(ChatChoice::is_recommended);
        let component_rows = component_rows(catalog, high_quality_embedding, npu_embedding_enabled);
        Self {
            ownership,
            catalog: catalog.clone(),
            page: SetupPage::Backends,
            backend_rows: backend_rows(catalog),
            show_all_backends: false,
            component_rows,
            chat_models,
            selected_chat,
            show_advanced_models,
            chat_dropdown_open: false,
            high_quality_embedding,
            npu_embedding_enabled,
            preferred_device,
            reasoning_control,
            downloads: Vec::new(),
            status: "Install the backends you want Lemonade to use, then continue to model configuration."
                .to_string(),
            busy: false,
            external_confirmation_armed: false,
            startup: None,
        }
    }

    pub(crate) fn with_startup_timeline(mut self, startup: StartupTimeline) -> Self {
        self.startup = Some(startup);
        self
    }

    pub(crate) fn is_complete(&self) -> bool {
        !self.chat_models.is_empty()
            && self
                .component_rows
                .iter()
                .all(|row| row.state == SetupComponentState::Ready && row.backend_ready)
            && self.chat_models[self.selected_chat].downloaded
            && recipe_has_installed_backend(
                &self.catalog,
                &self.chat_models[self.selected_chat].recipe,
            )
    }

    pub(crate) fn refresh_catalog(&mut self, catalog: &LemonadeServerCatalog) {
        self.catalog = catalog.clone();
        self.backend_rows = backend_rows(catalog);
        let selected = self
            .chat_models
            .get(self.selected_chat)
            .map(|model| model.id.clone());
        self.chat_models = setup_chat_models(catalog)
            .into_iter()
            .map(|model| ChatChoice {
                id: model.id.clone(),
                recipe: model.recipe.clone(),
                downloaded: model.downloaded,
                tools: model.supports_tool_calling(),
                reasoning: model.supports_reasoning(),
            })
            .collect();
        self.selected_chat = selected
            .as_deref()
            .and_then(|id| self.chat_models.iter().position(|model| model.id == id))
            .filter(|index| self.show_advanced_models || self.chat_models[*index].is_recommended())
            .or_else(|| {
                self.chat_models
                    .iter()
                    .position(|model| model.is_recommended() && model.downloaded)
            })
            .or_else(|| self.chat_models.iter().position(ChatChoice::is_recommended))
            .unwrap_or(0);
        if !self.chat_models.iter().any(ChatChoice::is_recommended) {
            self.show_advanced_models = true;
        }
        self.component_rows = component_rows(
            catalog,
            self.high_quality_embedding,
            self.npu_embedding_enabled,
        );
    }

    pub(crate) fn set_downloads(&mut self, value: &serde_json::Value) {
        self.downloads = parse_download_jobs(value);
    }

    pub(crate) fn set_busy(&mut self, busy: bool, status: impl Into<String>) {
        self.busy = busy;
        self.status = status.into();
    }

    pub(crate) fn apply_management_progress(&mut self, event: &ManagementProgressEvent) {
        let progress = event
            .progress_percent
            .map(|percent| format!(" {percent:.0}%"))
            .unwrap_or_default();
        let detail = event
            .message
            .as_deref()
            .map(|message| format!(": {message}"))
            .unwrap_or_default();
        self.busy = !event.is_terminal();
        self.status = match event.kind {
            ManagementEventKind::Progress => {
                format!("Preparing {}{progress}{detail}", event.target)
            }
            ManagementEventKind::Complete => format!("{} is ready{detail}", event.target),
            ManagementEventKind::Failed => format!("{} failed{detail}", event.target),
        };
    }

    fn cycle_device(&mut self, cx: &mut Context<Self>) {
        self.preferred_device = match self.preferred_device {
            ChatDevice::Auto => ChatDevice::Gpu,
            ChatDevice::Gpu => ChatDevice::Npu,
            ChatDevice::Npu => ChatDevice::Cpu,
            ChatDevice::Cpu => ChatDevice::Auto,
        };
        let preferred_recipe = match self.preferred_device {
            ChatDevice::Npu => Some("flm"),
            ChatDevice::Gpu | ChatDevice::Cpu => Some("llamacpp"),
            ChatDevice::Auto => None,
        };
        if let Some(recipe) = preferred_recipe
            && let Some(index) = self
                .chat_models
                .iter()
                .position(|model| model.recipe == recipe)
        {
            self.selected_chat = index;
        }
        self.external_confirmation_armed = false;
        cx.notify();
    }

    fn request_setup(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(model) = self.chat_models.get(self.selected_chat) else {
            self.status = "No compatible chat models were reported by Lemonade.".to_string();
            cx.notify();
            return;
        };
        if self.ownership == LemonadeOwnership::External && !self.external_confirmation_armed {
            self.external_confirmation_armed = true;
            self.status = "External management will install backends and download models. Click again to confirm this action.".to_string();
            cx.notify();
            return;
        }
        cx.emit(SetupRequested {
            chat_model: model.id.clone(),
            high_quality_embedding: self.high_quality_embedding,
            preferred_device: self.preferred_device.clone(),
            reasoning_control: self.reasoning_control,
            confirmed_external: self.ownership == LemonadeOwnership::Embedded
                || self.external_confirmation_armed,
        });
    }

    fn request_backend_install(&mut self, row: BackendRow, cx: &mut Context<Self>) {
        if self.busy || !row.can_install() {
            return;
        }
        if self.ownership == LemonadeOwnership::External && !self.external_confirmation_armed {
            self.external_confirmation_armed = true;
            self.status = format!(
                "Installing {}:{} changes an external Lemonade server. Click Install again to confirm.",
                row.recipe, row.backend
            );
            cx.notify();
            return;
        }
        cx.emit(SetupBackendInstallRequested {
            recipe: row.recipe,
            backend: row.backend,
            confirmed_external: self.ownership == LemonadeOwnership::Embedded
                || self.external_confirmation_armed,
        });
    }

    fn request_download_operation(
        &mut self,
        job_id: String,
        model_name: String,
        operation: SetupDownloadOperation,
        cx: &mut Context<Self>,
    ) {
        if self.ownership == LemonadeOwnership::External && !self.external_confirmation_armed {
            self.external_confirmation_armed = true;
            self.status = format!(
                "This will apply {operation:?} to a job on an external server. Click the control again to confirm."
            );
            cx.notify();
            return;
        }
        cx.emit(SetupDownloadRequested {
            job_id,
            model_name,
            operation,
            confirmed_external: self.ownership == LemonadeOwnership::Embedded
                || self.external_confirmation_armed,
        });
    }
}

fn backend_rows(catalog: &LemonadeServerCatalog) -> Vec<BackendRow> {
    let mut rows: Vec<_> = catalog
        .backends
        .iter()
        .filter(|backend| backend.state != "unsupported")
        .map(|backend| BackendRow {
            recipe: backend.recipe.clone(),
            backend: backend.backend.clone(),
            state: backend.state.clone(),
            message: backend.message.clone(),
            action: backend.action.clone(),
            version: backend.version.clone(),
            devices: backend.devices.clone(),
        })
        .collect();
    rows.sort_by(|left, right| {
        left.recipe
            .cmp(&right.recipe)
            .then_with(|| left.backend.cmp(&right.backend))
    });
    rows
}

fn backend_groups(rows: &[BackendRow], show_all: bool) -> Vec<BackendGroup> {
    let mut by_recipe = BTreeMap::<String, Vec<BackendRow>>::new();
    for row in rows.iter().filter(|row| {
        show_all
            || recipe_capabilities(&row.recipe)
                .iter()
                .any(|capability| capability.is_currently_relevant())
    }) {
        by_recipe
            .entry(row.recipe.clone())
            .or_default()
            .push(row.clone());
    }
    let mut groups: Vec<_> = by_recipe
        .into_iter()
        .map(|(recipe, rows)| BackendGroup { recipe, rows })
        .collect();
    groups.sort_by_key(|group| match group.recipe.as_str() {
        "llamacpp" => 0,
        "flm" => 1,
        "vllm" => 2,
        "whispercpp" | "moonshine" => 3,
        "kokoro" | "openmoss" => 4,
        "acestep" | "ace-step" | "ace_step" => 5,
        "thinksound" => 6,
        "sd-cpp" => 7,
        "trellis" => 8,
        "onnxruntime" => 9,
        _ => 10,
    });
    groups
}

fn recipe_capabilities(recipe: &str) -> &'static [BackendCapability] {
    match recipe {
        "llamacpp" => &[
            BackendCapability::Chat,
            BackendCapability::Embeddings,
            BackendCapability::Reranking,
        ],
        "flm" => &[
            BackendCapability::Chat,
            BackendCapability::Embeddings,
            BackendCapability::SpeechToText,
        ],
        "vllm" => &[
            BackendCapability::Chat,
            BackendCapability::ConcurrentServing,
        ],
        "whispercpp" | "moonshine" => &[BackendCapability::SpeechToText],
        "kokoro" | "openmoss" => &[BackendCapability::TextToSpeech],
        "sd-cpp" => &[BackendCapability::ImageGeneration],
        "acestep" | "ace-step" | "ace_step" => &[BackendCapability::MusicGeneration],
        "thinksound" => &[BackendCapability::SoundEffects],
        "onnxruntime" => &[BackendCapability::Routing],
        "trellis" => &[BackendCapability::ThreeDGeneration],
        _ => &[],
    }
}

fn recipe_display_name(recipe: &str) -> String {
    match recipe {
        "llamacpp" => "llama.cpp".to_string(),
        "flm" => "FLM".to_string(),
        "vllm" => "vLLM".to_string(),
        "whispercpp" => "Whisper.cpp".to_string(),
        "moonshine" => "Moonshine".to_string(),
        "kokoro" => "Kokoro".to_string(),
        "openmoss" => "OpenMOSS".to_string(),
        "sd-cpp" => "Stable Diffusion.cpp".to_string(),
        "acestep" | "ace-step" | "ace_step" => "ACE-Step".to_string(),
        "thinksound" => "ThinkSound".to_string(),
        "onnxruntime" => "ONNX Runtime".to_string(),
        "trellis" => "TRELLIS".to_string(),
        recipe => recipe.to_string(),
    }
}

fn recipe_description(recipe: &str) -> &'static str {
    match recipe {
        "llamacpp" => "Runs GGUF chat, embedding, and reranking models on CPU or GPU hardware.",
        "flm" => "Runs optimized language, embedding, and audio models on AMD NPUs.",
        "vllm" => "Runs chat models with high throughput for multiple concurrent sessions.",
        "whispercpp" => "Runs Whisper speech recognition models on CPU or GPU hardware.",
        "moonshine" => "Runs Moonshine as an alternative local speech-to-text engine.",
        "kokoro" => "Runs local Kokoro text-to-speech models.",
        "openmoss" => "Runs OpenMOSS as an alternative local text-to-speech engine.",
        "sd-cpp" => "Runs local image-generation models through Stable Diffusion.cpp.",
        "acestep" | "ace-step" | "ace_step" => "Generates music locally with ACE-Step.",
        "thinksound" => "Generates sound effects and other non-speech audio.",
        "onnxruntime" => {
            "Runs routing and classification models; this is not currently used by u-forge."
        }
        "trellis" => "Generates 3D models from prompts or reference images.",
        _ => "A model runtime reported by Lemonade Server.",
    }
}

fn friendly_device(device: &str) -> String {
    match device {
        "amd_igpu" => "AMD iGPU".to_string(),
        "amd_dgpu" => "AMD GPU".to_string(),
        "amd_npu" => "AMD NPU".to_string(),
        "nvidia_gpu" => "NVIDIA GPU".to_string(),
        "apple_gpu" => "Apple GPU".to_string(),
        "cpu" => "CPU".to_string(),
        device => device.replace('_', " "),
    }
}

fn backend_display_name(backend: &str) -> String {
    match backend {
        "rocm" => "ROCm".to_string(),
        "vulkan" => "Vulkan".to_string(),
        "cuda" => "CUDA".to_string(),
        "metal" => "Metal".to_string(),
        "npu" => "NPU".to_string(),
        "cpu" => "CPU".to_string(),
        backend => backend.to_string(),
    }
}

fn backend_tags(backend: &BackendRow) -> Vec<BackendTag> {
    let mut tags = vec![BackendTag {
        label: match backend.state.as_str() {
            "installed" => "Installed".to_string(),
            "installable" => "Available".to_string(),
            "update_required" => "Update required".to_string(),
            "" => "State unavailable".to_string(),
            state => state.replace('_', " "),
        },
        tone: match backend.state.as_str() {
            "installed" => BackendTagTone::Success,
            "update_required" => BackendTagTone::Warning,
            "installable" => BackendTagTone::Info,
            _ => BackendTagTone::Neutral,
        },
    }];
    tags.push(BackendTag {
        label: match backend.backend.as_str() {
            "rocm" => "AMD GPU optimized".to_string(),
            "vulkan" => "Portable GPU".to_string(),
            "cuda" => "NVIDIA GPU optimized".to_string(),
            "metal" => "Apple GPU optimized".to_string(),
            "npu" => "Low-power accelerator".to_string(),
            "cpu" => "Universal CPU fallback".to_string(),
            backend => backend.to_uppercase(),
        },
        tone: BackendTagTone::Neutral,
    });
    for device in &backend.devices {
        let label = friendly_device(device);
        if !tags.iter().any(|tag| tag.label == label) {
            tags.push(BackendTag {
                label,
                tone: BackendTagTone::Neutral,
            });
        }
    }
    if let Some(version) = &backend.version {
        tags.push(BackendTag {
            label: version.clone(),
            tone: BackendTagTone::Neutral,
        });
    }
    tags
}

fn component_rows(
    catalog: &LemonadeServerCatalog,
    include_hq: bool,
    include_npu: bool,
) -> Vec<ComponentRow> {
    initial_setup_components()
        .into_iter()
        .filter(|component| {
            component.required
                || (component.role == u_forge_core::lemonade::SetupRole::HighQualityEmbedding
                    && include_hq)
                || (component.role == u_forge_core::lemonade::SetupRole::NpuEmbedding
                    && include_npu)
        })
        .map(|component| {
            let state = component_state(catalog, &component);
            let recipe = component.recipe.or_else(|| {
                catalog
                    .models
                    .iter()
                    .find(|model| component.matches_model_id(&model.id))
                    .map(|model| model.recipe.as_str())
            });
            let backend_ready = recipe.is_none_or(|recipe| {
                recipe.is_empty() || recipe_has_installed_backend(catalog, recipe)
            });
            let status = if !backend_ready {
                recipe
                    .map(|recipe| backend_requirement_text(catalog, recipe))
                    .unwrap_or_else(|| state_text(&state))
            } else {
                state_text(&state)
            };
            ComponentRow {
                label: format!("{:?}: {}", component.role, component.model_id),
                state,
                status,
                backend_ready,
            }
        })
        .collect()
}

fn recipe_has_installed_backend(catalog: &LemonadeServerCatalog, recipe: &str) -> bool {
    catalog
        .backends
        .iter()
        .any(|backend| backend.recipe == recipe && backend.state == "installed")
}

fn backend_requirement_text(catalog: &LemonadeServerCatalog, recipe: &str) -> String {
    if catalog.backends.iter().any(|backend| {
        backend.recipe == recipe
            && matches!(backend.state.as_str(), "installable" | "update_required")
    }) {
        format!("{recipe} backend install required")
    } else if catalog
        .backends
        .iter()
        .any(|backend| backend.recipe == recipe)
    {
        format!("{recipe} backend unavailable")
    } else {
        format!("{recipe} backend not reported")
    }
}

fn parse_download_jobs(value: &serde_json::Value) -> Vec<DownloadJob> {
    let entries = value
        .as_array()
        .or_else(|| value.get("downloads").and_then(serde_json::Value::as_array))
        .or_else(|| value.get("jobs").and_then(serde_json::Value::as_array));
    entries
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let id = ["job_id", "id", "download_id"]
                .iter()
                .find_map(|key| entry.get(*key).and_then(serde_json::Value::as_str))?
                .to_string();
            let label = ["model_name", "model", "name", "file"]
                .iter()
                .find_map(|key| entry.get(*key).and_then(serde_json::Value::as_str))
                .unwrap_or(&id)
                .to_string();
            let status = ["status", "state", "action"]
                .iter()
                .find_map(|key| entry.get(*key).and_then(serde_json::Value::as_str))
                .unwrap_or("active")
                .to_string();
            let progress = entry
                .get("percent")
                .or_else(|| entry.get("progress"))
                .and_then(serde_json::Value::as_f64);
            let file = entry
                .get("file")
                .and_then(serde_json::Value::as_str)
                .filter(|file| !file.is_empty())
                .map(ToString::to_string);
            let bytes_downloaded = entry
                .get("bytes_downloaded")
                .and_then(serde_json::Value::as_u64);
            let bytes_total = entry.get("bytes_total").and_then(serde_json::Value::as_u64);
            let error = entry
                .get("error")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string);
            Some(DownloadJob {
                id,
                label,
                status,
                progress,
                file,
                bytes_downloaded,
                bytes_total,
                error,
            })
        })
        .collect()
}

fn state_text(state: &SetupComponentState) -> String {
    match state {
        SetupComponentState::Ready => "ready".to_string(),
        SetupComponentState::Missing => "model registration required".to_string(),
        SetupComponentState::NeedsDownload => "download required".to_string(),
        SetupComponentState::Conflict(message) => format!("blocked: {message}"),
    }
}

fn device_text(device: &ChatDevice) -> &'static str {
    match device {
        ChatDevice::Auto => "Auto",
        ChatDevice::Gpu => "GPU",
        ChatDevice::Npu => "NPU",
        ChatDevice::Cpu => "CPU",
    }
}

fn chat_choice_label(model: &ChatChoice) -> String {
    format!(
        "{} · {} · {}{}{}",
        model.id,
        model.recipe,
        if model.downloaded {
            "ready"
        } else {
            "download"
        },
        if model.tools { " · tools" } else { "" },
        if model.reasoning { " · thinking" } else { "" },
    )
}

impl Render for SetupPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *UiTheme::get(cx);
        let startup = self.startup.clone();
        let mut components = div().flex().flex_col().gap(px(3.0));
        for (index, row) in self.component_rows.iter().enumerate() {
            let ready = row.state == SetupComponentState::Ready && row.backend_ready;
            components = components.child(
                div()
                    .id(format!("setup-component-{index}"))
                    .flex()
                    .flex_row()
                    .justify_between()
                    .text_xs()
                    .text_color(if ready {
                        rgba(0xa6e3a1ff)
                    } else {
                        rgba(0xf9e2afff)
                    })
                    .child(row.label.clone())
                    .child(row.status.clone()),
            );
        }

        let visible_backend_groups = backend_groups(&self.backend_rows, self.show_all_backends);
        let has_visible_backends = !visible_backend_groups.is_empty();
        let mut backends = div().flex().flex_col().gap(px(16.0));
        for (group_index, group) in visible_backend_groups.into_iter().enumerate() {
            let recipe = group.recipe;
            let mut capability_tags = div().flex().flex_row().flex_wrap().gap(px(4.0));
            for capability in recipe_capabilities(&recipe) {
                capability_tags = capability_tags.child(
                    div()
                        .h(px(20.0))
                        .px_2()
                        .flex()
                        .items_center()
                        .rounded(px(10.0))
                        .bg(rgba(0x89b4fa20))
                        .text_xs()
                        .text_color(rgba(0x89b4faff))
                        .child(capability.label()),
                );
            }
            let mut group_rows = div().flex().flex_col().gap(px(6.0));
            for (row_index, row) in group.rows.into_iter().enumerate() {
                let can_install = row.can_install();
                let action = if row.action.is_empty() {
                    if row.state == "update_required" {
                        "Update".to_string()
                    } else {
                        "Install".to_string()
                    }
                } else {
                    row.action.clone()
                };
                let control_label = if can_install {
                    action
                } else if row.state == "installed" {
                    "Installed".to_string()
                } else {
                    "Unavailable".to_string()
                };
                let mut tags = div().flex().flex_row().flex_wrap().gap(px(4.0));
                for tag in backend_tags(&row) {
                    let (background, foreground) = match tag.tone {
                        BackendTagTone::Success => (rgba(0xa6e3a126), rgba(0xa6e3a1ff)),
                        BackendTagTone::Info => (rgba(0x89b4fa26), rgba(0x89b4faff)),
                        BackendTagTone::Warning => (rgba(0xf9e2af26), rgba(0xf9e2afff)),
                        BackendTagTone::Neutral => (rgba(0x45475a80), rgba(0xbac2deff)),
                    };
                    tags = tags.child(
                        div()
                            .h(px(20.0))
                            .px_2()
                            .flex()
                            .items_center()
                            .rounded(px(10.0))
                            .bg(background)
                            .text_xs()
                            .text_color(foreground)
                            .child(tag.label),
                    );
                }
                let install_row = row.clone();
                group_rows = group_rows.child(
                    div()
                        .id(format!("setup-backend-{group_index}-{row_index}"))
                        .p_3()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap(px(12.0))
                        .rounded(px(4.0))
                        .bg(rgba(0x1e1e2e80))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(5.0))
                                .child(backend_display_name(&row.backend))
                                .child(tags)
                                .when(!row.message.is_empty(), |details| {
                                    details.child(
                                        div()
                                            .text_xs()
                                            .text_color(rgba(0xa6adc8ff))
                                            .child(row.message.clone()),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .id(format!("setup-backend-install-{group_index}-{row_index}"))
                                .h(px(26.0))
                                .px_3()
                                .flex()
                                .items_center()
                                .bg(if can_install {
                                    rgb(0x89b4fa)
                                } else {
                                    rgb(0x45475a)
                                })
                                .text_color(if can_install {
                                    rgba(0x1e1e2eff)
                                } else {
                                    rgba(0xa6adc8ff)
                                })
                                .when(can_install, |button| {
                                    button.cursor_pointer().on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            this.request_backend_install(install_row.clone(), cx)
                                        }),
                                    )
                                })
                                .child(control_label),
                        ),
                );
            }
            backends = backends.child(
                div()
                    .id(format!("setup-backend-group-{group_index}"))
                    .flex()
                    .flex_col()
                    .gap(px(7.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .child(recipe_display_name(&recipe))
                            .when(!recipe_capabilities(&recipe).is_empty(), |header| {
                                header.child(capability_tags)
                            })
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgba(0xa6adc8ff))
                                    .child(recipe_description(&recipe)),
                            ),
                    )
                    .child(group_rows),
            );
        }

        let chat_label = self
            .chat_models
            .get(self.selected_chat)
            .map(chat_choice_label)
            .unwrap_or_else(|| "No compatible chat model".to_string());
        let visible_chat_models = self
            .chat_models
            .iter()
            .enumerate()
            .filter(|(_, model)| self.show_advanced_models || model.is_recommended())
            .map(|(index, model)| (index, chat_choice_label(model)))
            .collect::<Vec<_>>();
        let mut chat_options = div()
            .id("setup-chat-options")
            .w_full()
            .max_h(px(220.0))
            .overflow_y_scroll()
            .border_1()
            .border_color(rgb(0x585b70))
            .rounded(px(4.0))
            .bg(rgb(0x1e1e2e));
        for (index, label) in visible_chat_models {
            let selected = index == self.selected_chat;
            chat_options = chat_options.child(
                div()
                    .id(format!("setup-chat-option-{index}"))
                    .h(px(30.0))
                    .px_3()
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .bg(if selected {
                        rgba(0x89b4fa30)
                    } else {
                        rgba(0x00000000)
                    })
                    .hover(|style| style.bg(rgba(0x45475aaa)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.selected_chat = index;
                            this.chat_dropdown_open = false;
                            this.external_confirmation_armed = false;
                            cx.notify();
                        }),
                    )
                    .child(label),
            );
        }

        let mut downloads = div().flex().flex_col().gap(px(3.0));
        for (index, job) in self.downloads.iter().enumerate() {
            let job_id = job.id.clone();
            let operation = if matches!(
                job.status.as_str(),
                "error" | "failed" | "cancelled" | "paused"
            ) {
                SetupDownloadOperation::Retry
            } else if job.status == "completed" {
                SetupDownloadOperation::Control(DownloadAction::Remove)
            } else {
                SetupDownloadOperation::Control(DownloadAction::Pause)
            };
            let active = matches!(job.status.as_str(), "downloading" | "active");
            let model_name = job.label.clone();
            let label = match job.progress {
                Some(progress) => format!("{} — {} ({progress:.1}%)", job.label, job.status),
                None => format!("{} — {}", job.label, job.status),
            };
            let details = match (job.bytes_downloaded, job.bytes_total) {
                (Some(done), Some(total)) if total > 0 => {
                    format!(" · {done}/{total} bytes")
                }
                _ => String::new(),
            };
            let label = format!(
                "{label}{}{}{}",
                job.file
                    .as_deref()
                    .map(|file| format!(" · {file}"))
                    .unwrap_or_default(),
                details,
                job.error
                    .as_deref()
                    .map(|error| format!(" · {error}"))
                    .unwrap_or_default()
            );
            let primary_job_id = job_id.clone();
            let primary_model_name = model_name.clone();
            let operation_content = match operation {
                SetupDownloadOperation::Retry => {
                    Icon::new(IconName::Refresh, IconSize::Medium, rgba(0xcdd6f4ff))
                        .into_any_element()
                }
                SetupDownloadOperation::Control(DownloadAction::Remove) => {
                    Icon::new(IconName::MinusCircle, IconSize::Medium, rgba(0xf38ba8ff))
                        .into_any_element()
                }
                _ => div().child(format!("{operation:?}")).into_any_element(),
            };
            let operation_tooltip = match operation {
                SetupDownloadOperation::Retry => "Retry download",
                SetupDownloadOperation::Control(DownloadAction::Remove) => {
                    "Remove completed download from this list"
                }
                SetupDownloadOperation::Control(DownloadAction::Pause) => "Pause download",
                SetupDownloadOperation::Control(DownloadAction::Cancel) => "Cancel download",
            };
            let mut controls = div().flex().flex_row().gap(px(4.0)).child(
                div()
                    .id(format!("setup-download-action-{index}"))
                    .px_2()
                    .h(px(24.0))
                    .flex()
                    .items_center()
                    .bg(rgb(0x45475a))
                    .cursor_pointer()
                    .tooltip(Tooltip::text(operation_tooltip))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                            this.request_download_operation(
                                primary_job_id.clone(),
                                primary_model_name.clone(),
                                operation,
                                cx,
                            );
                        }),
                    )
                    .child(operation_content),
            );
            if active {
                let cancel_job_id = job_id;
                let cancel_model_name = model_name;
                controls = controls.child(
                    div()
                        .id(format!("setup-download-cancel-{index}"))
                        .px_2()
                        .h(px(24.0))
                        .flex()
                        .items_center()
                        .bg(rgb(0x45475a))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                                this.request_download_operation(
                                    cancel_job_id.clone(),
                                    cancel_model_name.clone(),
                                    SetupDownloadOperation::Control(DownloadAction::Cancel),
                                    cx,
                                );
                            }),
                        )
                        .child("Cancel"),
                );
            }
            downloads = downloads.child(
                div()
                    .id(format!("setup-download-{index}"))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(label)
                    .child(controls),
            );
        }

        let provision_label = if self.busy {
            "Working…"
        } else if self.page == SetupPage::Backends {
            "Continue to configuration"
        } else if self.ownership == LemonadeOwnership::External && self.external_confirmation_armed
        {
            "Confirm external provisioning"
        } else {
            "Save and provision"
        };

        let backend_content = div()
            .id("setup-backend-page")
            .p_4()
            .flex_1()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .min_h_0()
            .gap(px(12.0))
            .child("1 of 2 · Lemonade backends")
            .child(
                div()
                    .text_xs()
                    .text_color(rgba(0xa6adc8ff))
                    .child(
                        "Backends are Lemonade's hardware runtimes. Install the ones you want before downloading or registering models.",
                    ),
            )
            .child(
                div()
                    .id("setup-show-all-backends")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.show_all_backends = !this.show_all_backends;
                            this.external_confirmation_armed = false;
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .w(px(16.0))
                            .h(px(16.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .border_1()
                            .border_color(rgb(0x89b4fa))
                            .bg(if self.show_all_backends {
                                rgb(0x89b4fa)
                            } else {
                                rgb(0x313244)
                            })
                            .text_color(rgba(0x1e1e2eff))
                            .when(self.show_all_backends, |checkbox| {
                                checkbox.child(Icon::new(
                                    IconName::Check,
                                    IconSize::Small,
                                    rgba(0x1e1e2eff),
                                ))
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child("Show all backends")
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgba(0xa6adc8ff))
                                    .child(
                                        "Includes speech, music, image, sound-effect, routing, and 3D runtimes.",
                                    ),
                            ),
                    ),
            )
            .child(if !has_visible_backends {
                div()
                    .text_color(rgba(0xf9e2afff))
                    .child(if self.backend_rows.is_empty() {
                        "No backend information was reported by Lemonade."
                    } else {
                        "No chat, embedding, or reranking backends were reported. Enable Show all backends to inspect the remaining runtimes."
                    })
            } else {
                backends
            })
            .child(
                div()
                    .text_xs()
                    .text_color(rgba(0xf9e2afff))
                    .child(self.status.clone()),
            );

        let configuration_content = div()
            .id("setup-configuration-page")
            .p_4()
            .flex_1()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .min_h_0()
            .gap(px(12.0))
            .child("2 of 2 · Models and configuration")
            .child(components)
            .child(div().h(px(1.0)).bg(rgb(0x45475a)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child("Chat model")
                    .child(
                        div()
                            .id("setup-chat-dropdown")
                            .w_full()
                            .min_h(px(32.0))
                            .px_3()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .border_1()
                            .border_color(rgb(0x585b70))
                            .rounded(px(4.0))
                            .bg(rgb(0x1e1e2e))
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    this.chat_dropdown_open = !this.chat_dropdown_open;
                                    cx.notify();
                                }),
                            )
                            .child(chat_label)
                            .child(Icon::new(
                                IconName::ChevronDown,
                                IconSize::Medium,
                                rgba(0xcdd6f4ff),
                            )),
                    )
                    .when(self.chat_dropdown_open, |field| field.child(chat_options)),
            )
            .child(
                div()
                    .id("setup-advanced-models")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.show_advanced_models = !this.show_advanced_models;
                            if !this.show_advanced_models
                                && this
                                    .chat_models
                                    .get(this.selected_chat)
                                    .is_some_and(|model| !model.is_recommended())
                                && let Some(index) =
                                    this.chat_models.iter().position(ChatChoice::is_recommended)
                            {
                                this.selected_chat = index;
                            }
                            this.chat_dropdown_open = false;
                            this.external_confirmation_armed = false;
                            cx.notify();
                        }),
                    )
                    .child(if self.show_advanced_models {
                        "☑"
                    } else {
                        "☐"
                    })
                    .child("Advanced · show models without both tool use and thinking"),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .child("High-quality embedding (optional)")
                    .child(
                        div()
                            .id("setup-hq-toggle")
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    this.high_quality_embedding = !this.high_quality_embedding;
                                    this.component_rows = component_rows(
                                        &this.catalog,
                                        this.high_quality_embedding,
                                        this.npu_embedding_enabled,
                                    );
                                    this.external_confirmation_armed = false;
                                    cx.notify();
                                }),
                            )
                            .child(if self.high_quality_embedding {
                                "Enabled"
                            } else {
                                "Disabled"
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .child("Preferred device")
                    .child(
                        div()
                            .id("setup-device-cycle")
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.cycle_device(cx)),
                            )
                            .child(device_text(&self.preferred_device)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .child("Reasoning control")
                    .child(
                        div()
                            .id("setup-reasoning-toggle")
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    this.reasoning_control = match this.reasoning_control {
                                        ReasoningControl::Request => ReasoningControl::Reload,
                                        ReasoningControl::Reload => ReasoningControl::Request,
                                    };
                                    this.external_confirmation_armed = false;
                                    cx.notify();
                                }),
                            )
                            .child(match self.reasoning_control {
                                ReasoningControl::Request => "Request",
                                ReasoningControl::Reload => "Reload fallback",
                            }),
                    ),
            )
            .child(div().h(px(1.0)).bg(rgb(0x45475a)))
            .child("Server-owned downloads")
            .child(if self.downloads.is_empty() {
                div().text_color(rgba(0x6c7086ff)).child("No active jobs")
            } else {
                downloads
            })
            .child(
                div()
                    .text_xs()
                    .text_color(rgba(0xf9e2afff))
                    .child(self.status.clone()),
            );

        deferred(
            div()
                .id("setup-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme.colors.overlay)
                .child(
                    div()
                        .id("setup-dialog")
                        .w(px(720.0))
                        .max_h(px(680.0))
                        .flex()
                        .flex_col()
                        .bg(theme.colors.elevated_surface)
                        .border_1()
                        .border_color(theme.colors.border)
                        .rounded(px(theme.metrics.radius_medium))
                        .text_color(theme.colors.text)
                        .text_size(theme.typography.label)
                        .child(
                            div()
                                .h(theme.metrics.panel_header_height)
                                .px(px(theme.metrics.space_4))
                                .flex()
                                .items_center()
                                .justify_between()
                                .bg(theme.colors.title_bar_surface)
                                .child("Lemonade AI Setup")
                                .child(
                                    div()
                                        .id("setup-close")
                                        .cursor_pointer()
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(
                                                |_this, _: &MouseDownEvent, _window, cx| {
                                                    cx.emit(SetupClosed);
                                                },
                                            ),
                                        )
                                        .child("Close"),
                                ),
                        )
                        .child(if self.page == SetupPage::Backends {
                            backend_content
                        } else {
                            configuration_content
                        })
                        .child(
                            div()
                                .min_h(theme.metrics.panel_header_height)
                                .px(px(theme.metrics.space_4))
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap(px(8.0))
                                .border_t_1()
                                .border_color(theme.colors.border)
                                .when(self.page == SetupPage::Configuration, |footer| {
                                    footer.child(
                                        div()
                                            .id("setup-back")
                                            .h(px(28.0))
                                            .px_3()
                                            .flex()
                                            .items_center()
                                            .cursor_pointer()
                                            .bg(rgb(0x45475a))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|this, _, _, cx| {
                                                    this.page = SetupPage::Backends;
                                                    this.external_confirmation_armed = false;
                                                    this.status = "Review or install Lemonade backends."
                                                        .to_string();
                                                    cx.notify();
                                                }),
                                            )
                                            .child("Back"),
                                    )
                                })
                                .child(
                                    div()
                                        .id("setup-refresh")
                                        .h(px(28.0))
                                        .px_3()
                                        .flex()
                                        .items_center()
                                        .cursor_pointer()
                                        .bg(rgb(0x45475a))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|_this, _, _, cx| {
                                                cx.emit(SetupRefreshRequested)
                                            }),
                                        )
                                        .child("Refresh"),
                                )
                                .child(
                                    div()
                                        .id("setup-provision")
                                        .h(px(28.0))
                                        .px_3()
                                        .flex()
                                        .items_center()
                                        .cursor_pointer()
                                        .bg(rgb(0x89b4fa))
                                        .text_color(rgba(0x1e1e2eff))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                if this.busy {
                                                    return;
                                                }
                                                if this.page == SetupPage::Backends {
                                                    this.page = SetupPage::Configuration;
                                                    this.external_confirmation_armed = false;
                                                    this.status = "Choose models and runtime preferences, then save and provision."
                                                        .to_string();
                                                    cx.notify();
                                                } else {
                                                    this.request_setup(cx);
                                                }
                                            }),
                                        )
                                        .child(provision_label),
                                ),
                        ),
                )
                .when_some(startup, |root, startup| {
                    root.child(
                        canvas(
                            |_, _, _| {},
                            move |_, (), _, cx| {
                                if startup.milestone(StartupMilestone::SetupFirstPaint)
                                    && startup.should_exit_after(StartupMilestone::SetupFirstPaint)
                                {
                                    cx.quit();
                                }
                            },
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .w(px(1.0))
                        .h(px(1.0)),
                    )
                }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat_model(
        id: &str,
        labels: &[&str],
        downloaded: bool,
    ) -> u_forge_core::lemonade::CatalogModel {
        u_forge_core::lemonade::CatalogModel {
            id: id.to_string(),
            recipe: "llamacpp".to_string(),
            labels: labels.iter().map(|label| (*label).to_string()).collect(),
            downloaded,
            ..Default::default()
        }
    }

    #[test]
    fn basic_model_picker_defaults_to_tools_and_thinking_models() {
        let catalog = LemonadeServerCatalog {
            models: vec![
                chat_model("plain", &[], true),
                chat_model("tools-only", &["tool-calling"], true),
                chat_model("recommended", &["tool-calling", "reasoning"], false),
            ],
            ..Default::default()
        };

        let panel = SetupPanel::new(
            LemonadeOwnership::Embedded,
            &catalog,
            Some("plain"),
            false,
            false,
            ChatDevice::Auto,
            ReasoningControl::Request,
        );

        assert_eq!(panel.chat_models[panel.selected_chat].id, "recommended");
        assert!(!panel.show_advanced_models);
    }

    #[test]
    fn model_picker_falls_back_to_advanced_when_catalog_has_no_recommended_model() {
        let catalog = LemonadeServerCatalog {
            models: vec![chat_model("plain", &[], true)],
            ..Default::default()
        };
        let panel = SetupPanel::new(
            LemonadeOwnership::Embedded,
            &catalog,
            None,
            false,
            false,
            ChatDevice::Auto,
            ReasoningControl::Request,
        );

        assert!(panel.show_advanced_models);
        assert_eq!(panel.chat_models[panel.selected_chat].id, "plain");
    }

    #[test]
    fn parses_current_and_legacy_download_collections() {
        let jobs = parse_download_jobs(&serde_json::json!({
            "downloads": [{
                "job_id": "job-1",
                "model_name": "model-a",
                "status": "downloading",
                "percent": 25.0
            }]
        }));
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "job-1");
        assert_eq!(jobs[0].progress, Some(25.0));
    }

    #[test]
    fn optional_embedding_roles_follow_their_independent_settings() {
        let catalog = LemonadeServerCatalog::default();
        assert_eq!(component_rows(&catalog, true, true).len(), 4);
        assert_eq!(component_rows(&catalog, true, false).len(), 3);
        assert_eq!(component_rows(&catalog, false, true).len(), 3);
        assert_eq!(component_rows(&catalog, false, false).len(), 2);
    }

    #[test]
    fn missing_model_reports_backend_prerequisite_before_registration() {
        let mut catalog = LemonadeServerCatalog {
            backends: vec![u_forge_core::lemonade::InstalledBackend {
                recipe: "flm".to_string(),
                backend: "npu".to_string(),
                state: "installable".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let rows = component_rows(&catalog, false, true);
        let flm = rows
            .iter()
            .find(|row| row.label.contains("embed-gemma-300m-FLM"))
            .unwrap();
        assert_eq!(flm.status, "flm backend install required");
        assert!(!flm.backend_ready);

        catalog.backends[0].state = "installed".to_string();
        let rows = component_rows(&catalog, false, true);
        let flm = rows
            .iter()
            .find(|row| row.label.contains("embed-gemma-300m-FLM"))
            .unwrap();
        assert_eq!(flm.status, "model registration required");
        assert!(flm.backend_ready);
    }

    #[test]
    fn backend_page_excludes_unsupported_options() {
        let catalog = LemonadeServerCatalog {
            backends: vec![
                u_forge_core::lemonade::InstalledBackend {
                    recipe: "llamacpp".to_string(),
                    backend: "rocm".to_string(),
                    state: "unsupported".to_string(),
                    ..Default::default()
                },
                u_forge_core::lemonade::InstalledBackend {
                    recipe: "flm".to_string(),
                    backend: "npu".to_string(),
                    state: "installable".to_string(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let rows = backend_rows(&catalog);
        assert_eq!(rows.len(), 1);
        assert!(
            rows.iter()
                .any(|row| row.recipe == "flm" && row.can_install())
        );
        assert!(!rows.iter().any(|row| row.backend == "rocm"));
    }

    #[test]
    fn backend_groups_keep_recipe_variants_together_and_describe_targets() {
        let rows = vec![
            BackendRow {
                recipe: "flm".to_string(),
                backend: "npu".to_string(),
                state: "installed".to_string(),
                message: String::new(),
                action: String::new(),
                version: Some("v1".to_string()),
                devices: vec!["amd_npu".to_string()],
            },
            BackendRow {
                recipe: "llamacpp".to_string(),
                backend: "vulkan".to_string(),
                state: "installable".to_string(),
                message: String::new(),
                action: String::new(),
                version: None,
                devices: vec!["amd_igpu".to_string()],
            },
            BackendRow {
                recipe: "llamacpp".to_string(),
                backend: "cpu".to_string(),
                state: "installed".to_string(),
                message: String::new(),
                action: String::new(),
                version: None,
                devices: vec!["cpu".to_string()],
            },
        ];
        let groups = backend_groups(&rows, false);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].recipe, "llamacpp");
        assert_eq!(groups[0].rows.len(), 2);
        assert_eq!(recipe_display_name(&groups[0].recipe), "llama.cpp");

        let vulkan = groups[0]
            .rows
            .iter()
            .find(|row| row.backend == "vulkan")
            .unwrap();
        let labels: Vec<_> = backend_tags(vulkan)
            .into_iter()
            .map(|tag| tag.label)
            .collect();
        assert!(labels.contains(&"Available".to_string()));
        assert!(labels.contains(&"Portable GPU".to_string()));
        assert!(labels.contains(&"AMD iGPU".to_string()));
    }

    #[test]
    fn backend_filter_defaults_to_current_features_and_show_all_exposes_future_engines() {
        let row = |recipe: &str| BackendRow {
            recipe: recipe.to_string(),
            backend: "cpu".to_string(),
            state: "installable".to_string(),
            message: String::new(),
            action: String::new(),
            version: None,
            devices: vec!["cpu".to_string()],
        };
        let rows = [
            "llamacpp",
            "flm",
            "vllm",
            "acestep",
            "moonshine",
            "onnxruntime",
            "openmoss",
            "thinksound",
            "trellis",
        ]
        .into_iter()
        .map(row)
        .collect::<Vec<_>>();

        let relevant = backend_groups(&rows, false);
        assert_eq!(
            relevant
                .iter()
                .map(|group| group.recipe.as_str())
                .collect::<Vec<_>>(),
            vec!["llamacpp", "flm", "vllm"]
        );
        assert_eq!(backend_groups(&rows, true).len(), rows.len());
        assert_eq!(recipe_display_name("acestep"), "ACE-Step");
        assert_eq!(recipe_display_name("onnxruntime"), "ONNX Runtime");
        assert_eq!(recipe_display_name("trellis"), "TRELLIS");
        assert!(recipe_description("thinksound").contains("sound effects"));
    }
}
