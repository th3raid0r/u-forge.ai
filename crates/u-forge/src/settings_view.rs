//! Typed, editable application settings.
//!
//! The view owns a draft [`AppConfig`] and emits a complete validated snapshot
//! on save. It deliberately never renders serialized configuration text.

use std::path::PathBuf;

use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, Render, Subscription, Window, div,
    prelude::*, px,
};
use u_forge_core::{AppConfig, ChatDevice, ChatDeviceConfig, EmbeddingTarget, ReasoningControl};

use crate::text_field::{TextChanged, TextFieldView};
use crate::ui::components::{Button, ButtonStyle};
use crate::ui::theme::UiTheme;

pub(crate) struct SettingsSaveRequested(pub(crate) AppConfig);
pub(crate) struct SettingsRebuildRequested(pub(crate) EmbeddingTarget);

pub(crate) struct SettingsView {
    focus: FocusHandle,
    draft: AppConfig,
    dirty: bool,
    show_expert: bool,
    status: Option<String>,
    system_prompt: gpui::Entity<TextFieldView>,
    import_file: gpui::Entity<TextFieldView>,
    schema_dir: gpui::Entity<TextFieldView>,
    db_path: gpui::Entity<TextFieldView>,
    gpu_model: gpui::Entity<TextFieldView>,
    npu_model: gpui::Entity<TextFieldView>,
    cpu_model: gpui::Entity<TextFieldView>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SettingsSaveRequested> for SettingsView {}
impl EventEmitter<SettingsRebuildRequested> for SettingsView {}

impl Focusable for SettingsView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SettingsView {
    pub(crate) fn new(config: AppConfig, cx: &mut Context<Self>) -> Self {
        fn field(
            value: &str,
            multiline: bool,
            cx: &mut Context<SettingsView>,
        ) -> gpui::Entity<TextFieldView> {
            cx.new(|cx| {
                let mut field = TextFieldView::new(multiline, "", cx);
                field.set_content(value, cx);
                field
            })
        }

        let system_prompt = field(&config.chat.system_prompt, true, cx);
        let import_file = field(&config.data.import_file.display().to_string(), false, cx);
        let schema_dir = field(&config.data.schema_dir.display().to_string(), false, cx);
        let db_path = field(&config.storage.db_path.display().to_string(), false, cx);
        let gpu_model = field(config.chat.gpu.model.as_deref().unwrap_or(""), false, cx);
        let npu_model = field(config.chat.npu.model.as_deref().unwrap_or(""), false, cx);
        let cpu_model = field(config.chat.cpu.model.as_deref().unwrap_or(""), false, cx);

        let mut subscriptions = Vec::new();
        subscriptions.push(
            cx.subscribe(&system_prompt, |this, _, event: &TextChanged, cx| {
                this.draft.chat.system_prompt = event.0.clone();
                this.mark_dirty(cx);
            }),
        );
        subscriptions.push(
            cx.subscribe(&import_file, |this, _, event: &TextChanged, cx| {
                this.draft.data.import_file = PathBuf::from(&event.0);
                this.mark_dirty(cx);
            }),
        );
        subscriptions.push(
            cx.subscribe(&schema_dir, |this, _, event: &TextChanged, cx| {
                this.draft.data.schema_dir = PathBuf::from(&event.0);
                this.mark_dirty(cx);
            }),
        );
        subscriptions.push(cx.subscribe(&db_path, |this, _, event: &TextChanged, cx| {
            this.draft.storage.db_path = PathBuf::from(&event.0);
            this.mark_dirty(cx);
        }));
        subscriptions.push(
            cx.subscribe(&gpu_model, |this, _, event: &TextChanged, cx| {
                this.draft.chat.gpu.model = non_empty(&event.0);
                this.mark_dirty(cx);
            }),
        );
        subscriptions.push(
            cx.subscribe(&npu_model, |this, _, event: &TextChanged, cx| {
                this.draft.chat.npu.model = non_empty(&event.0);
                this.mark_dirty(cx);
            }),
        );
        subscriptions.push(
            cx.subscribe(&cpu_model, |this, _, event: &TextChanged, cx| {
                this.draft.chat.cpu.model = non_empty(&event.0);
                this.mark_dirty(cx);
            }),
        );

        Self {
            focus: cx.focus_handle(),
            draft: config,
            dirty: false,
            show_expert: false,
            status: None,
            system_prompt,
            import_file,
            schema_dir,
            db_path,
            gpu_model,
            npu_model,
            cpu_model,
            _subscriptions: subscriptions,
        }
    }

    fn mark_dirty(&mut self, cx: &mut Context<Self>) {
        self.dirty = true;
        self.status = None;
        cx.notify();
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub(crate) fn mark_saved(
        &mut self,
        config: AppConfig,
        message: String,
        cx: &mut Context<Self>,
    ) {
        self.draft = config;
        self.dirty = false;
        self.status = Some(message);
        cx.notify();
    }

    pub(crate) fn set_error(&mut self, message: String, cx: &mut Context<Self>) {
        self.status = Some(message);
        cx.notify();
    }

    pub(crate) fn draft(&self) -> AppConfig {
        self.draft.clone()
    }

    fn active_device(&self) -> &ChatDeviceConfig {
        match self.draft.chat.preferred_device {
            ChatDevice::Auto | ChatDevice::Gpu => &self.draft.chat.gpu,
            ChatDevice::Npu => &self.draft.chat.npu,
            ChatDevice::Cpu => &self.draft.chat.cpu,
        }
    }

    fn active_device_mut(&mut self) -> &mut ChatDeviceConfig {
        match self.draft.chat.preferred_device {
            ChatDevice::Auto | ChatDevice::Gpu => &mut self.draft.chat.gpu,
            ChatDevice::Npu => &mut self.draft.chat.npu,
            ChatDevice::Cpu => &mut self.draft.chat.cpu,
        }
    }
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *UiTheme::get(cx);
        let handle = cx.weak_entity();
        let active_device = self.active_device().clone();

        let row = |label: &'static str, control: gpui::AnyElement| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(theme.metrics.space_4))
                .child(div().flex_grow().child(label))
                .child(control)
        };
        let stepper = |id: &'static str,
                       value: String,
                       minus: Box<dyn Fn(&mut SettingsView)>,
                       plus: Box<dyn Fn(&mut SettingsView)>| {
            let decrease = handle.clone();
            let increase = handle.clone();
            div()
                .flex()
                .items_center()
                .gap(px(theme.metrics.space_2))
                .child(
                    Button::new(format!("{id}-minus"), "−").on_click(move |_, _, cx| {
                        decrease
                            .update(cx, |view, cx| {
                                minus(view);
                                view.mark_dirty(cx);
                            })
                            .ok();
                    }),
                )
                .child(div().min_w(px(72.0)).text_center().child(value))
                .child(
                    Button::new(format!("{id}-plus"), "+").on_click(move |_, _, cx| {
                        increase
                            .update(cx, |view, cx| {
                                plus(view);
                                view.mark_dirty(cx);
                            })
                            .ok();
                    }),
                )
                .into_any_element()
        };
        let toggle = |id: &'static str,
                      label: String,
                      selected: bool,
                      mutate: Box<dyn Fn(&mut SettingsView)>| {
            let target = handle.clone();
            Button::new(id, label)
                .selected(selected)
                .on_click(move |_, _, cx| {
                    target
                        .update(cx, |view, cx| {
                            mutate(view);
                            view.mark_dirty(cx);
                        })
                        .ok();
                })
                .into_any_element()
        };

        let appearance = div()
            .flex()
            .flex_col()
            .gap(px(theme.metrics.space_3))
            .child(div().text_size(theme.typography.body).child("Appearance"))
            .child(row(
                "Text size",
                stepper(
                    "settings-font",
                    format!("{:.0} px", self.draft.ui.font_size),
                    Box::new(|v| v.draft.ui.font_size = (v.draft.ui.font_size - 1.0).max(10.0)),
                    Box::new(|v| v.draft.ui.font_size = (v.draft.ui.font_size + 1.0).min(28.0)),
                ),
            ))
            .child(row(
                "Interface size",
                stepper(
                    "settings-interface",
                    format!("{:.0} px", self.draft.ui.interface_size),
                    Box::new(|v| {
                        v.draft.ui.interface_size = (v.draft.ui.interface_size - 1.0).max(14.0)
                    }),
                    Box::new(|v| {
                        v.draft.ui.interface_size = (v.draft.ui.interface_size + 1.0).min(32.0)
                    }),
                ),
            ))
            .child(row(
                "Window controls",
                toggle(
                    "settings-controls",
                    if self.draft.ui.window_controls_left {
                        "Left"
                    } else {
                        "Right"
                    }
                    .to_string(),
                    self.draft.ui.window_controls_left,
                    Box::new(|v| {
                        v.draft.ui.window_controls_left = !v.draft.ui.window_controls_left
                    }),
                ),
            ))
            .child(row(
                "Diagnostic controls elsewhere",
                toggle(
                    "settings-diagnostics",
                    if self.draft.ui.show_advanced_controls {
                        "Shown"
                    } else {
                        "Hidden"
                    }
                    .to_string(),
                    self.draft.ui.show_advanced_controls,
                    Box::new(|v| {
                        v.draft.ui.show_advanced_controls = !v.draft.ui.show_advanced_controls
                    }),
                ),
            ));

        let embedding = div()
            .flex()
            .flex_col()
            .gap(px(theme.metrics.space_3))
            .child(
                div()
                    .text_size(theme.typography.body)
                    .child("Embedding and Retrieval"),
            )
            .child(
                div()
                    .text_color(theme.colors.text_muted)
                    .child(
                        "Lemonade's max_loaded_models limit is per model type. A value of 3 can keep standard, NPU, and HQ embedding models resident on the app-owned server.",
                    ),
            )
            .child(row(
                "Lemonade max_loaded_models",
                stepper(
                    "settings-max-loaded-models",
                    self.draft.lemonade.max_loaded_models.to_string(),
                    Box::new(|v| {
                        v.draft.lemonade.max_loaded_models =
                            v.draft.lemonade.max_loaded_models.saturating_sub(1).max(1)
                    }),
                    Box::new(|v| {
                        v.draft.lemonade.max_loaded_models =
                            v.draft.lemonade.max_loaded_models.saturating_add(1)
                    }),
                ),
            ))
            .child(row(
                "NPU embeddings",
                toggle(
                    "settings-npu",
                    if self.draft.embedding.npu_enabled {
                        "Enabled"
                    } else {
                        "Disabled"
                    }
                    .to_string(),
                    self.draft.embedding.npu_enabled,
                    Box::new(|v| v.draft.embedding.npu_enabled = !v.draft.embedding.npu_enabled),
                ),
            ))
            .child(row(
                "GPU embeddings",
                toggle(
                    "settings-gpu",
                    if self.draft.embedding.gpu_enabled {
                        "Enabled"
                    } else {
                        "Disabled"
                    }
                    .to_string(),
                    self.draft.embedding.gpu_enabled,
                    Box::new(|v| v.draft.embedding.gpu_enabled = !v.draft.embedding.gpu_enabled),
                ),
            ))
            .child(row(
                "CPU embeddings",
                toggle(
                    "settings-cpu",
                    if self.draft.embedding.cpu_enabled {
                        "Enabled"
                    } else {
                        "Disabled"
                    }
                    .to_string(),
                    self.draft.embedding.cpu_enabled,
                    Box::new(|v| v.draft.embedding.cpu_enabled = !v.draft.embedding.cpu_enabled),
                ),
            ))
            .child(row(
                "High-quality lane",
                toggle(
                    "settings-hq",
                    if self.draft.embedding.high_quality_embedding {
                        "Enabled"
                    } else {
                        "Disabled"
                    }
                    .to_string(),
                    self.draft.embedding.high_quality_embedding,
                    Box::new(|v| {
                        v.draft.embedding.high_quality_embedding =
                            !v.draft.embedding.high_quality_embedding
                    }),
                ),
            ))
            .child(row(
                "NPU dispatch weight",
                stepper(
                    "settings-npu-weight",
                    self.draft.embedding.npu_weight.to_string(),
                    Box::new(|v| {
                        v.draft.embedding.npu_weight =
                            v.draft.embedding.npu_weight.saturating_sub(10)
                    }),
                    Box::new(|v| {
                        v.draft.embedding.npu_weight =
                            v.draft.embedding.npu_weight.saturating_add(10)
                    }),
                ),
            ))
            .child(row(
                "GPU dispatch weight",
                stepper(
                    "settings-gpu-weight",
                    self.draft.embedding.gpu_weight.to_string(),
                    Box::new(|v| {
                        v.draft.embedding.gpu_weight =
                            v.draft.embedding.gpu_weight.saturating_sub(10)
                    }),
                    Box::new(|v| {
                        v.draft.embedding.gpu_weight =
                            v.draft.embedding.gpu_weight.saturating_add(10)
                    }),
                ),
            ))
            .child(row(
                "CPU dispatch weight",
                stepper(
                    "settings-cpu-weight",
                    self.draft.embedding.cpu_weight.to_string(),
                    Box::new(|v| {
                        v.draft.embedding.cpu_weight =
                            v.draft.embedding.cpu_weight.saturating_sub(10)
                    }),
                    Box::new(|v| {
                        v.draft.embedding.cpu_weight =
                            v.draft.embedding.cpu_weight.saturating_add(10)
                    }),
                ),
            ))
            .child(row(
                "FTS candidates",
                stepper(
                    "settings-fts",
                    self.draft.chat.fts_limit.to_string(),
                    Box::new(|v| {
                        v.draft.chat.fts_limit = v.draft.chat.fts_limit.saturating_sub(5).max(1)
                    }),
                    Box::new(|v| v.draft.chat.fts_limit = (v.draft.chat.fts_limit + 5).min(1_000)),
                ),
            ))
            .child(row(
                "Semantic candidates",
                stepper(
                    "settings-semantic",
                    self.draft.chat.semantic_limit.to_string(),
                    Box::new(|v| {
                        v.draft.chat.semantic_limit =
                            v.draft.chat.semantic_limit.saturating_sub(5).max(1)
                    }),
                    Box::new(|v| {
                        v.draft.chat.semantic_limit = (v.draft.chat.semantic_limit + 5).min(1_000)
                    }),
                ),
            ))
            .child(row(
                "Returned nodes",
                stepper(
                    "settings-search-limit",
                    self.draft.chat.search_limit.to_string(),
                    Box::new(|v| {
                        v.draft.chat.search_limit =
                            v.draft.chat.search_limit.saturating_sub(1).max(1)
                    }),
                    Box::new(|v| {
                        v.draft.chat.search_limit = (v.draft.chat.search_limit + 1).min(100)
                    }),
                ),
            ))
            .child(row(
                "Hybrid semantic weight",
                stepper(
                    "settings-alpha",
                    format!("{:.1}", self.draft.chat.alpha),
                    Box::new(|v| v.draft.chat.alpha = (v.draft.chat.alpha - 0.1).max(0.0)),
                    Box::new(|v| v.draft.chat.alpha = (v.draft.chat.alpha + 0.1).min(1.0)),
                ),
            ))
            .child(row(
                "HQ semantic boost",
                stepper(
                    "settings-hq-boost",
                    format!("{:.1}", self.draft.chat.hq_semantic_boost),
                    Box::new(|v| {
                        v.draft.chat.hq_semantic_boost =
                            (v.draft.chat.hq_semantic_boost - 0.25).max(0.0)
                    }),
                    Box::new(|v| {
                        v.draft.chat.hq_semantic_boost =
                            (v.draft.chat.hq_semantic_boost + 0.25).min(20.0)
                    }),
                ),
            ))
            .child(row(
                "Reranking",
                toggle(
                    "settings-rerank",
                    if self.draft.chat.rerank {
                        "Enabled"
                    } else {
                        "Disabled"
                    }
                    .to_string(),
                    self.draft.chat.rerank,
                    Box::new(|v| v.draft.chat.rerank = !v.draft.chat.rerank),
                ),
            ));

        let assistant = div()
            .flex()
            .flex_col()
            .gap(px(theme.metrics.space_3))
            .child(div().text_size(theme.typography.body).child("Assistant"))
            .child(
                div()
                    .text_color(theme.colors.text_muted)
                    .child("System prompt"),
            )
            .child(self.system_prompt.clone())
            .child(row(
                "Preferred device",
                toggle(
                    "settings-device",
                    format!("{:?}", self.draft.chat.preferred_device),
                    true,
                    Box::new(|v| {
                        v.draft.chat.preferred_device = match v.draft.chat.preferred_device {
                            ChatDevice::Auto => ChatDevice::Gpu,
                            ChatDevice::Gpu => ChatDevice::Npu,
                            ChatDevice::Npu => ChatDevice::Cpu,
                            ChatDevice::Cpu => ChatDevice::Auto,
                        }
                    }),
                ),
            ))
            .child(row(
                "Reasoning control",
                toggle(
                    "settings-reasoning",
                    format!("{:?}", self.draft.chat.reasoning_control),
                    true,
                    Box::new(|v| {
                        v.draft.chat.reasoning_control = match v.draft.chat.reasoning_control {
                            ReasoningControl::Request => ReasoningControl::Reload,
                            ReasoningControl::Reload => ReasoningControl::Request,
                        }
                    }),
                ),
            ))
            .child(row(
                "Context window",
                stepper(
                    "settings-context",
                    self.draft.chat.max_context_tokens.to_string(),
                    Box::new(|v| {
                        v.draft.chat.max_context_tokens = v
                            .draft
                            .chat
                            .max_context_tokens
                            .saturating_sub(1_024)
                            .max(2_048)
                    }),
                    Box::new(|v| {
                        v.draft.chat.max_context_tokens =
                            v.draft.chat.max_context_tokens.saturating_add(1_024)
                    }),
                ),
            ))
            .child(row(
                "Response reserve",
                stepper(
                    "settings-reserve",
                    self.draft.chat.response_reserve.to_string(),
                    Box::new(|v| {
                        v.draft.chat.response_reserve =
                            v.draft.chat.response_reserve.saturating_sub(256).max(256)
                    }),
                    Box::new(|v| {
                        v.draft.chat.response_reserve = (v.draft.chat.response_reserve + 256)
                            .min(v.draft.chat.max_context_tokens.saturating_sub(1))
                    }),
                ),
            ))
            .child(row(
                "History turns",
                stepper(
                    "settings-history",
                    self.draft.chat.max_history_turns.to_string(),
                    Box::new(|v| {
                        v.draft.chat.max_history_turns =
                            v.draft.chat.max_history_turns.saturating_sub(1)
                    }),
                    Box::new(|v| {
                        v.draft.chat.max_history_turns =
                            (v.draft.chat.max_history_turns + 1).min(100)
                    }),
                ),
            ))
            .child(row(
                "Tool turns",
                stepper(
                    "settings-tool-turns",
                    self.draft.chat.max_tool_turns.to_string(),
                    Box::new(|v| {
                        v.draft.chat.max_tool_turns =
                            v.draft.chat.max_tool_turns.saturating_sub(1).max(1)
                    }),
                    Box::new(|v| {
                        v.draft.chat.max_tool_turns = (v.draft.chat.max_tool_turns + 1).min(32)
                    }),
                ),
            ))
            .child(row(
                "Schema context maximum",
                stepper(
                    "settings-schema-budget",
                    self.draft.chat.agent.schema_summary_tokens.to_string(),
                    Box::new(|v| {
                        v.draft.chat.agent.schema_summary_tokens = v
                            .draft
                            .chat
                            .agent
                            .schema_summary_tokens
                            .saturating_sub(256)
                            .max(32)
                    }),
                    Box::new(|v| {
                        v.draft.chat.agent.schema_summary_tokens =
                            v.draft.chat.agent.schema_summary_tokens.saturating_add(256)
                    }),
                ),
            ))
            .child(row(
                "Unchanged-call repeats",
                stepper(
                    "settings-repeat",
                    self.draft.chat.agent.repeated_call_limit.to_string(),
                    Box::new(|v| {
                        v.draft.chat.agent.repeated_call_limit =
                            v.draft.chat.agent.repeated_call_limit.saturating_sub(1)
                    }),
                    Box::new(|v| {
                        v.draft.chat.agent.repeated_call_limit =
                            (v.draft.chat.agent.repeated_call_limit + 1).min(10)
                    }),
                ),
            ));

        let expert_toggle = {
            let target = handle.clone();
            Button::new(
                "settings-expert",
                if self.show_expert {
                    "Hide expert settings"
                } else {
                    "Show expert settings"
                },
            )
            .selected(self.show_expert)
            .on_click(move |_, _, cx| {
                target
                    .update(cx, |view, cx| {
                        view.show_expert = !view.show_expert;
                        cx.notify();
                    })
                    .ok();
            })
        };
        let save = {
            let target = handle.clone();
            Button::new(
                "settings-save",
                if self.dirty { "Save Settings" } else { "Saved" },
            )
            .style(ButtonStyle::Filled)
            .disabled(!self.dirty)
            .on_click(move |_, _, cx| {
                target
                    .update(cx, |view, cx| cx.emit(SettingsSaveRequested(view.draft())))
                    .ok();
            })
        };
        let rebuild_standard = {
            let target = handle.clone();
            Button::new(
                "settings-rebuild-standard",
                "Rebuild standard semantic index",
            )
            .on_click(move |_, _, cx| {
                target
                    .update(cx, |_view, cx| {
                        cx.emit(SettingsRebuildRequested(EmbeddingTarget::Standard))
                    })
                    .ok();
            })
        };
        let rebuild_hq = {
            let target = handle.clone();
            Button::new("settings-rebuild-hq", "Rebuild HQ semantic index").on_click(
                move |_, _, cx| {
                    target
                        .update(cx, |_view, cx| {
                            cx.emit(SettingsRebuildRequested(EmbeddingTarget::HighQuality))
                        })
                        .ok();
                },
            )
        };

        div()
            .id("settings-view")
            .track_focus(&self.focus)
            .size_full()
            .p_4()
            .flex().flex_col().gap(px(theme.metrics.space_4))
            .overflow_y_scroll()
            .bg(theme.colors.app_surface)
            .text_size(theme.typography.label)
            .text_color(theme.colors.text)
            .child(div().flex().items_center().justify_between().child(div().text_size(theme.typography.body).child("Application Settings")).child(expert_toggle))
            .child(appearance)
            .child(embedding)
            .child(assistant)
            .when(self.show_expert, |settings| settings.child(
                div().flex().flex_col().gap(px(theme.metrics.space_3))
                    .child(div().text_size(theme.typography.body).child("Expert Paths and Models"))
                    .child(div().text_color(theme.colors.warning).child("Database path and vector dimensions take effect after restart. Changing dimensions requires rebuilding the semantic index."))
                    .child(div().child("Default import file")).child(self.import_file.clone())
                    .child(div().child("Schema directory")).child(self.schema_dir.clone())
                    .child(div().child("Database directory")).child(self.db_path.clone())
                    .child(row("Standard vector dimensions", stepper("settings-standard-dims", self.draft.storage.embedding_dimensions.to_string(), Box::new(|v| v.draft.storage.embedding_dimensions = v.draft.storage.embedding_dimensions.saturating_sub(128).max(128)), Box::new(|v| v.draft.storage.embedding_dimensions = v.draft.storage.embedding_dimensions.saturating_add(128)))))
                    .child(row("HQ vector dimensions", stepper("settings-hq-dims", self.draft.storage.high_quality_embedding_dimensions.to_string(), Box::new(|v| v.draft.storage.high_quality_embedding_dimensions = v.draft.storage.high_quality_embedding_dimensions.saturating_sub(512).max(512)), Box::new(|v| v.draft.storage.high_quality_embedding_dimensions = v.draft.storage.high_quality_embedding_dimensions.saturating_add(512)))))
                    .child(div().child("GPU model (blank = automatic)")).child(self.gpu_model.clone())
                    .child(div().child("NPU model (blank = automatic)")).child(self.npu_model.clone())
                    .child(div().child("CPU model (blank = automatic)")).child(self.cpu_model.clone())
                    .child(row(
                        "GPU temperature",
                        stepper(
                            "settings-gpu-temp",
                            format!("{:.1}", self.draft.chat.gpu.temperature.unwrap_or(0.3)),
                            Box::new(|v| v.draft.chat.gpu.temperature = Some((v.draft.chat.gpu.temperature.unwrap_or(0.3) - 0.1).max(0.0))),
                            Box::new(|v| v.draft.chat.gpu.temperature = Some((v.draft.chat.gpu.temperature.unwrap_or(0.3) + 0.1).min(2.0))),
                        ),
                    ))
                    .child(row(
                        "NPU temperature",
                        stepper(
                            "settings-npu-temp",
                            format!("{:.1}", self.draft.chat.npu.temperature.unwrap_or(0.3)),
                            Box::new(|v| v.draft.chat.npu.temperature = Some((v.draft.chat.npu.temperature.unwrap_or(0.3) - 0.1).max(0.0))),
                            Box::new(|v| v.draft.chat.npu.temperature = Some((v.draft.chat.npu.temperature.unwrap_or(0.3) + 0.1).min(2.0))),
                        ),
                    ))
                    .child(row(
                        "CPU temperature",
                        stepper(
                            "settings-cpu-temp",
                            format!("{:.1}", self.draft.chat.cpu.temperature.unwrap_or(0.3)),
                            Box::new(|v| v.draft.chat.cpu.temperature = Some((v.draft.chat.cpu.temperature.unwrap_or(0.3) - 0.1).max(0.0))),
                            Box::new(|v| v.draft.chat.cpu.temperature = Some((v.draft.chat.cpu.temperature.unwrap_or(0.3) + 0.1).min(2.0))),
                        ),
                    ))
                    .child(div().text_size(theme.typography.body).child("Active-device sampling"))
                    .child(row(
                        "Generation maximum",
                        stepper(
                            "settings-active-max-tokens",
                            active_device.max_tokens.map_or_else(|| "Auto".to_string(), |value| value.to_string()),
                            Box::new(|v| {
                                let device = v.active_device_mut();
                                device.max_tokens = device.max_tokens.and_then(|value| value.checked_sub(256)).filter(|value| *value > 0);
                            }),
                            Box::new(|v| {
                                let device = v.active_device_mut();
                                device.max_tokens = Some(device.max_tokens.unwrap_or(768).saturating_add(256));
                            }),
                        ),
                    ))
                    .child(row(
                        "Top-p",
                        stepper(
                            "settings-active-top-p",
                            format!("{:.2}", active_device.top_p.unwrap_or(0.9)),
                            Box::new(|v| { let device = v.active_device_mut(); device.top_p = Some((device.top_p.unwrap_or(0.9) - 0.05).max(0.0)); }),
                            Box::new(|v| { let device = v.active_device_mut(); device.top_p = Some((device.top_p.unwrap_or(0.9) + 0.05).min(1.0)); }),
                        ),
                    ))
                    .child(row(
                        "Top-k",
                        stepper(
                            "settings-active-top-k",
                            active_device.top_k.unwrap_or(0).to_string(),
                            Box::new(|v| { let device = v.active_device_mut(); device.top_k = Some(device.top_k.unwrap_or(0).saturating_sub(5)); }),
                            Box::new(|v| { let device = v.active_device_mut(); device.top_k = Some(device.top_k.unwrap_or(0).saturating_add(5)); }),
                        ),
                    ))
                    .child(row(
                        "Min-p",
                        stepper(
                            "settings-active-min-p",
                            format!("{:.2}", active_device.min_p.unwrap_or(0.05)),
                            Box::new(|v| { let device = v.active_device_mut(); device.min_p = Some((device.min_p.unwrap_or(0.05) - 0.01).max(0.0)); }),
                            Box::new(|v| { let device = v.active_device_mut(); device.min_p = Some((device.min_p.unwrap_or(0.05) + 0.01).min(1.0)); }),
                        ),
                    ))
                    .child(row(
                        "Frequency penalty",
                        stepper(
                            "settings-active-frequency",
                            format!("{:.1}", active_device.frequency_penalty.unwrap_or(0.0)),
                            Box::new(|v| { let device = v.active_device_mut(); device.frequency_penalty = Some((device.frequency_penalty.unwrap_or(0.0) - 0.1).max(-2.0)); }),
                            Box::new(|v| { let device = v.active_device_mut(); device.frequency_penalty = Some((device.frequency_penalty.unwrap_or(0.0) + 0.1).min(2.0)); }),
                        ),
                    ))
                    .child(row(
                        "Presence penalty",
                        stepper(
                            "settings-active-presence",
                            format!("{:.1}", active_device.presence_penalty.unwrap_or(0.0)),
                            Box::new(|v| { let device = v.active_device_mut(); device.presence_penalty = Some((device.presence_penalty.unwrap_or(0.0) - 0.1).max(-2.0)); }),
                            Box::new(|v| { let device = v.active_device_mut(); device.presence_penalty = Some((device.presence_penalty.unwrap_or(0.0) + 0.1).min(2.0)); }),
                        ),
                    ))
                    .child(row(
                        "Repetition penalty",
                        stepper(
                            "settings-active-repetition",
                            format!("{:.1}", active_device.repetition_penalty.unwrap_or(1.0)),
                            Box::new(|v| { let device = v.active_device_mut(); device.repetition_penalty = Some((device.repetition_penalty.unwrap_or(1.0) - 0.1).max(0.0)); }),
                            Box::new(|v| { let device = v.active_device_mut(); device.repetition_penalty = Some((device.repetition_penalty.unwrap_or(1.0) + 0.1).min(3.0)); }),
                        ),
                    ))
                    .child(div().flex().gap(px(theme.metrics.space_3)).child(rebuild_standard).child(rebuild_hq))
            ))
            .when_some(self.status.clone(), |settings, status| settings.child(div().text_color(theme.colors.text_muted).child(status)))
            .child(div().flex().justify_end().child(save))
    }
}
