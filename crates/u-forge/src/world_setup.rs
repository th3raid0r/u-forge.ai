//! Fresh-world schema and optional data selection shown while AI prerequisites
//! continue provisioning in the background.

use std::path::PathBuf;

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, MouseButton, MouseDownEvent,
    Render, Subscription, Task, Window, deferred, div, prelude::*, px, rgb, rgba,
};
use u_forge_core::SchemaIngestion;

use crate::text_field::{TextChanged, TextFieldView};
use crate::ui::components::Tooltip;
use crate::ui::icons::{Icon, IconName, IconSize};
use crate::ui::theme::UiTheme;

#[derive(Debug, Clone)]
pub(crate) struct WorldCreateRequested {
    pub schema_dir: PathBuf,
    pub data_file: Option<PathBuf>,
}

pub(crate) struct WorldSetupClosed;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorldSetupPage {
    Schema,
    Data,
}

pub(crate) struct WorldSetupModal {
    page: WorldSetupPage,
    schema_dir: Entity<TextFieldView>,
    data_file: Entity<TextFieldView>,
    focus: FocusHandle,
    status: String,
    embedding_ready: bool,
    busy: bool,
    browse_generation: u64,
    browse_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<WorldCreateRequested> for WorldSetupModal {}
impl EventEmitter<WorldSetupClosed> for WorldSetupModal {}

impl Focusable for WorldSetupModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl WorldSetupModal {
    pub(crate) fn new(
        schema_dir: &str,
        data_file: &str,
        embedding_ready: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let schema_dir = cx.new(|cx| {
            let mut field = TextFieldView::new(false, "", cx);
            field.set_content(schema_dir, cx);
            field
        });
        let data_file = cx.new(|cx| {
            let mut field = TextFieldView::new(false, "", cx);
            field.set_content(data_file, cx);
            field
        });
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe(&schema_dir, |_this, _, _: &TextChanged, cx| {
            cx.notify();
        }));
        subscriptions.push(cx.subscribe(&data_file, |_this, _, _: &TextChanged, cx| {
            cx.notify();
        }));
        Self {
            page: WorldSetupPage::Schema,
            schema_dir,
            data_file,
            focus: cx.focus_handle(),
            status: "Choose the schema that defines your world.".to_string(),
            embedding_ready,
            busy: false,
            browse_generation: 0,
            browse_task: None,
            _subscriptions: subscriptions,
        }
    }

    pub(crate) fn set_embedding_ready(&mut self, ready: bool, cx: &mut Context<Self>) {
        self.embedding_ready = ready;
        cx.notify();
    }

    pub(crate) fn set_busy(
        &mut self,
        busy: bool,
        status: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        self.busy = busy;
        self.status = status.into();
        cx.notify();
    }

    fn browse(&mut self, cx: &mut Context<Self>) {
        let schema = self.page == WorldSetupPage::Schema;
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: !schema,
            directories: schema,
            multiple: false,
            prompt: None,
        });
        self.browse_generation = self.browse_generation.wrapping_add(1);
        let generation = self.browse_generation;
        let field = if schema {
            self.schema_dir.clone()
        } else {
            self.data_file.clone()
        };
        self.browse_task = Some(cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let Some(this) = this.upgrade() else { return };
            this.update(cx, |modal, cx| {
                if modal.browse_generation != generation {
                    return;
                }
                field.update(cx, |field, cx| {
                    field.set_content(&path.to_string_lossy(), cx);
                });
            })
            .ok();
        }));
    }

    fn next(&mut self, cx: &mut Context<Self>) {
        let path = PathBuf::from(self.schema_dir.read(cx).content.trim());
        match SchemaIngestion::load_schemas_from_directory(&path, "onboarding", "1.0.0") {
            Ok(_) => {
                self.page = WorldSetupPage::Data;
                self.status =
                    "Optionally choose initial JSONL data, or start with the schema alone."
                        .to_string();
            }
            Err(error) => self.status = format!("Schema validation failed: {error}"),
        }
        cx.notify();
    }

    fn create(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let schema_dir = PathBuf::from(self.schema_dir.read(cx).content.trim());
        let data_text = self.data_file.read(cx).content.trim().to_string();
        let data_file = (!data_text.is_empty()).then(|| PathBuf::from(data_text));
        if let Some(path) = &data_file {
            if !path.is_file() {
                self.status = format!("Initial data file does not exist: {}", path.display());
                cx.notify();
                return;
            }
            if !self.embedding_ready {
                self.status = "Downloading embedding prerequisites…".to_string();
                cx.notify();
                return;
            }
        }
        cx.emit(WorldCreateRequested {
            schema_dir,
            data_file,
        });
    }
}

impl Render for WorldSetupModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *UiTheme::get(cx);
        let schema_page = self.page == WorldSetupPage::Schema;
        let field = if schema_page {
            self.schema_dir.clone()
        } else {
            self.data_file.clone()
        };
        let has_data = !self.data_file.read(cx).content.trim().is_empty();
        let create_blocked = has_data && !self.embedding_ready;
        let primary_label = if self.busy {
            "Creating…"
        } else if schema_page {
            "Next"
        } else if create_blocked {
            "Downloading prerequisites…"
        } else {
            "Create"
        };
        let title = if schema_page {
            "Create your world · 1 of 2"
        } else {
            "Create your world · 2 of 2"
        };

        deferred(
            div()
                .id("world-setup-backdrop")
                .track_focus(&self.focus)
                .absolute()
                .inset_0()
                .w_full()
                .h_full()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x0000008c))
                .child(
                    div()
                        .id("world-setup-dialog")
                        .w(px(600.0))
                        .bg(rgb(0x313244))
                        .border_1()
                        .border_color(rgb(0x45475a))
                        .rounded(px(6.0))
                        .child(
                            div()
                                .h(px(42.0))
                                .px_4()
                                .flex()
                                .items_center()
                                .justify_between()
                                .bg(rgb(0x1e1e2e))
                                .child(title)
                                .child(
                                    div()
                                        .id("world-setup-close")
                                        .cursor_pointer()
                                        .tooltip(Tooltip::text(
                                            "Close for now; a schema is still required",
                                        ))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|_, _: &MouseDownEvent, _, cx| {
                                                cx.emit(WorldSetupClosed)
                                            }),
                                        )
                                        .child(Icon::new(
                                            IconName::Close,
                                            IconSize::Medium,
                                            rgba(0xcdd6f4ff),
                                        )),
                                ),
                        )
                        .child(
                            div()
                                .p_4()
                                .flex()
                                .flex_col()
                                .gap(px(12.0))
                                .child(if schema_page {
                                    "Schema directory"
                                } else {
                                    "Initial data file (optional)"
                                })
                                .child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .gap(px(8.0))
                                        .child(
                                            div()
                                                .flex_1()
                                                .border_1()
                                                .border_color(rgb(0x45475a))
                                                .rounded(px(4.0))
                                                .child(field),
                                        )
                                        .child(
                                            div()
                                                .id("world-setup-browse")
                                                .h(theme.metrics.control_height)
                                                .w(px(38.0))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .bg(rgb(0x45475a))
                                                .rounded(px(4.0))
                                                .cursor_pointer()
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(|this, _, _, cx| this.browse(cx)),
                                                )
                                                .child(Icon::new(
                                                    IconName::FolderOpen,
                                                    IconSize::Medium,
                                                    rgba(0xcdd6f4ff),
                                                )),
                                        ),
                                )
                                .when(!schema_page, |content| {
                                    content.child(
                                        div()
                                            .id("world-setup-skip-data")
                                            .text_sm()
                                            .text_color(rgba(0x89b4faff))
                                            .cursor_pointer()
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|this, _, _, cx| {
                                                    this.data_file.update(cx, |field, cx| {
                                                        field.set_content("", cx)
                                                    });
                                                }),
                                            )
                                            .child("Start without initial data"),
                                    )
                                })
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(if create_blocked {
                                            rgba(0xf9e2afff)
                                        } else {
                                            rgba(0xa6adc8ff)
                                        })
                                        .child(self.status.clone()),
                                ),
                        )
                        .child(
                            div()
                                .h(px(50.0))
                                .px_4()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap(px(8.0))
                                .border_t_1()
                                .border_color(rgb(0x45475a))
                                .when(!schema_page, |footer| {
                                    footer.child(
                                        div()
                                            .id("world-setup-back")
                                            .h(theme.metrics.control_height)
                                            .px_3()
                                            .flex()
                                            .items_center()
                                            .bg(rgb(0x45475a))
                                            .rounded(px(4.0))
                                            .cursor_pointer()
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|this, _, _, cx| {
                                                    this.page = WorldSetupPage::Schema;
                                                    this.status = "Choose the schema that defines your world."
                                                        .to_string();
                                                    cx.notify();
                                                }),
                                            )
                                            .child("Back"),
                                    )
                                })
                                .child(
                                    div()
                                        .id("world-setup-primary")
                                        .h(theme.metrics.control_height)
                                        .px_3()
                                        .flex()
                                        .items_center()
                                        .bg(if create_blocked {
                                            rgb(0x585b70)
                                        } else {
                                            rgb(0x89b4fa)
                                        })
                                        .text_color(rgba(0x1e1e2eff))
                                        .rounded(px(4.0))
                                        .cursor_pointer()
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |this, _, _, cx| {
                                                if schema_page {
                                                    this.next(cx);
                                                } else {
                                                    this.create(cx);
                                                }
                                            }),
                                        )
                                        .child(primary_label),
                                ),
                        ),
                ),
        )
    }
}
