//! Reopenable Lemonade provisioning dialog.

use gpui::{
    Context, EventEmitter, MouseButton, MouseDownEvent, Render, Window, deferred, div, prelude::*,
    px, rgb, rgba,
};
use u_forge_core::{
    ChatDevice, ReasoningControl,
    lemonade::{
        DownloadAction, LemonadeOwnership, LemonadeServerCatalog, SetupComponentState,
        component_state, initial_setup_components, setup_chat_models,
    },
};

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

#[derive(Debug, Clone, Copy)]
pub(crate) enum SetupDownloadOperation {
    Control(DownloadAction),
    Retry,
}

pub(crate) struct SetupRefreshRequested;
pub(crate) struct SetupClosed;

#[derive(Debug, Clone)]
struct ChatChoice {
    id: String,
    recipe: String,
    downloaded: bool,
    tools: bool,
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
    component_rows: Vec<(String, SetupComponentState)>,
    chat_models: Vec<ChatChoice>,
    selected_chat: usize,
    high_quality_embedding: bool,
    npu_embedding_enabled: bool,
    preferred_device: ChatDevice,
    reasoning_control: ReasoningControl,
    downloads: Vec<DownloadJob>,
    status: String,
    busy: bool,
    external_confirmation_armed: bool,
}

impl EventEmitter<SetupRequested> for SetupPanel {}
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
            })
            .collect();
        let selected_chat = selected_chat_model
            .and_then(|selected| chat_models.iter().position(|model| model.id == selected))
            .or_else(|| chat_models.iter().position(|model| model.downloaded))
            .unwrap_or(0);
        let component_rows = component_rows(catalog, high_quality_embedding, npu_embedding_enabled);
        Self {
            ownership,
            catalog: catalog.clone(),
            component_rows,
            chat_models,
            selected_chat,
            high_quality_embedding,
            npu_embedding_enabled,
            preferred_device,
            reasoning_control,
            downloads: Vec::new(),
            status: "Review the selected components, then save and provision.".to_string(),
            busy: false,
            external_confirmation_armed: false,
        }
    }

    pub(crate) fn is_complete(&self) -> bool {
        !self.chat_models.is_empty()
            && self
                .component_rows
                .iter()
                .all(|(_, state)| *state == SetupComponentState::Ready)
            && self.chat_models[self.selected_chat].downloaded
    }

    pub(crate) fn refresh_catalog(&mut self, catalog: &LemonadeServerCatalog) {
        self.catalog = catalog.clone();
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
            })
            .collect();
        self.selected_chat = selected
            .as_deref()
            .and_then(|id| self.chat_models.iter().position(|model| model.id == id))
            .unwrap_or(0);
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

    fn cycle_chat(&mut self, direction: isize, cx: &mut Context<Self>) {
        if !self.chat_models.is_empty() {
            self.selected_chat = self
                .selected_chat
                .checked_add_signed(direction)
                .unwrap_or(self.chat_models.len() - 1)
                % self.chat_models.len();
            self.external_confirmation_armed = false;
            cx.notify();
        }
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

fn component_rows(
    catalog: &LemonadeServerCatalog,
    include_hq: bool,
    include_npu: bool,
) -> Vec<(String, SetupComponentState)> {
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
            (
                format!("{:?}: {}", component.role, component.model_id),
                component_state(catalog, &component),
            )
        })
        .collect()
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
        SetupComponentState::Missing => "registration required".to_string(),
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

impl Render for SetupPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut components = div().flex().flex_col().gap(px(3.0));
        for (index, (name, state)) in self.component_rows.iter().enumerate() {
            let ready = *state == SetupComponentState::Ready;
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
                    .child(name.clone())
                    .child(state_text(state)),
            );
        }

        let chat_label = self
            .chat_models
            .get(self.selected_chat)
            .map(|model| {
                format!(
                    "{} · {} · {}{}",
                    model.id,
                    model.recipe,
                    if model.downloaded {
                        "ready"
                    } else {
                        "download"
                    },
                    if model.tools {
                        " · tools"
                    } else {
                        " · direct chat"
                    }
                )
            })
            .unwrap_or_else(|| "No compatible chat model".to_string());

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
            let mut controls = div().flex().flex_row().gap(px(4.0)).child(
                div()
                    .id(format!("setup-download-action-{index}"))
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
                                primary_job_id.clone(),
                                primary_model_name.clone(),
                                operation,
                                cx,
                            );
                        }),
                    )
                    .child(format!("{operation:?}")),
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
        } else if self.ownership == LemonadeOwnership::External && self.external_confirmation_armed
        {
            "Confirm external provisioning"
        } else {
            "Save and provision"
        };

        deferred(
            div()
                .id("setup-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x000000a0))
                .child(
                    div()
                        .id("setup-dialog")
                        .w(px(720.0))
                        .max_h(px(680.0))
                        .flex()
                        .flex_col()
                        .bg(rgb(0x313244))
                        .border_1()
                        .border_color(rgb(0x585b70))
                        .rounded(px(6.0))
                        .text_color(rgba(0xcdd6f4ff))
                        .text_sm()
                        .child(
                            div()
                                .h(px(42.0))
                                .px_4()
                                .flex()
                                .items_center()
                                .justify_between()
                                .bg(rgb(0x1e1e2e))
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
                        .child(
                            div()
                                .p_4()
                                .flex()
                                .flex_col()
                                .gap(px(12.0))
                                .child("Required and optional components")
                                .child(components)
                                .child(div().h(px(1.0)).bg(rgb(0x45475a)))
                                .child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .justify_between()
                                        .child("Chat model")
                                        .child(
                                            div()
                                                .flex()
                                                .flex_row()
                                                .gap(px(6.0))
                                                .child(
                                                    div()
                                                        .id("setup-chat-prev")
                                                        .px_2()
                                                        .cursor_pointer()
                                                        .on_mouse_down(
                                                            MouseButton::Left,
                                                            cx.listener(|this, _, _, cx| {
                                                                this.cycle_chat(-1, cx)
                                                            }),
                                                        )
                                                        .child("‹"),
                                                )
                                                .child(chat_label)
                                                .child(
                                                    div()
                                                        .id("setup-chat-next")
                                                        .px_2()
                                                        .cursor_pointer()
                                                        .on_mouse_down(
                                                            MouseButton::Left,
                                                            cx.listener(|this, _, _, cx| {
                                                                this.cycle_chat(1, cx)
                                                            }),
                                                        )
                                                        .child("›"),
                                                ),
                                        ),
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
                                                        this.high_quality_embedding =
                                                            !this.high_quality_embedding;
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
                                                    cx.listener(|this, _, _, cx| {
                                                        this.cycle_device(cx)
                                                    }),
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
                                                        this.reasoning_control =
                                                            match this.reasoning_control {
                                                                ReasoningControl::Request => {
                                                                    ReasoningControl::Reload
                                                                }
                                                                ReasoningControl::Reload => {
                                                                    ReasoningControl::Request
                                                                }
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
                                ),
                        )
                        .child(
                            div()
                                .h(px(48.0))
                                .px_4()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap(px(8.0))
                                .border_t_1()
                                .border_color(rgb(0x45475a))
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
                                            cx.listener(|this, _, _, cx| this.request_setup(cx)),
                                        )
                                        .child(provision_label),
                                ),
                        ),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
