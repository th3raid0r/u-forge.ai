use std::path::PathBuf;

use gpui::{
    deferred, div, prelude::*, px, rgb, rgba, App, Context, Entity, EventEmitter, FocusHandle,
    Focusable, MouseButton, MouseDownEvent, Window,
};

use crate::text_field::TextFieldView;

// ── Picker mode & kind ────────────────────────────────────────────────────────

pub(crate) enum PickerMode {
    File,
    Directory,
}

/// Which AppView operation the picker is driving.
pub(crate) enum PathPickerKind {
    DataFile,
    SchemaDir,
    ExportDir,
}

// ── Events ────────────────────────────────────────────────────────────────────

pub(crate) struct PathConfirmed(pub(crate) PathBuf);
pub(crate) struct PathCancelled;

// ── PathPickerModal ───────────────────────────────────────────────────────────

pub(crate) struct PathPickerModal {
    mode: PickerMode,
    title: String,
    confirm_label: String,
    pub(crate) path_field: Entity<TextFieldView>,
    focus: FocusHandle,
}

impl EventEmitter<PathConfirmed> for PathPickerModal {}
impl EventEmitter<PathCancelled> for PathPickerModal {}

impl Focusable for PathPickerModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl PathPickerModal {
    pub(crate) fn new(
        mode: PickerMode,
        title: &str,
        confirm_label: &str,
        initial_path: &str,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial = initial_path.to_string();
        let path_field = cx.new(|cx| {
            let mut field = TextFieldView::new(false, "", cx);
            field.set_content(&initial, cx);
            field
        });
        Self {
            mode,
            title: title.to_string(),
            confirm_label: confirm_label.to_string(),
            path_field,
            focus: cx.focus_handle(),
        }
    }

    fn browse(&mut self, cx: &mut Context<Self>) {
        let (files, directories) = match self.mode {
            PickerMode::File => (true, false),
            PickerMode::Directory => (false, true),
        };
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files,
            directories,
            multiple: false,
            prompt: None,
        });
        let field = self.path_field.clone();
        cx.spawn(async move |_this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let path_str = path.to_string_lossy().into_owned();
            field
                .update(cx, |f, cx| {
                    f.set_content(&path_str, cx);
                })
                .ok();
        })
        .detach();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        let path = PathBuf::from(self.path_field.read(cx).content.clone());
        cx.emit(PathConfirmed(path));
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(PathCancelled);
    }
}

// ── Render ────────────────────────────────────────────────────────────────────

impl Render for PathPickerModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self.title.clone();
        let confirm_label = self.confirm_label.clone();
        let path_field = self.path_field.clone();

        // Full-screen semi-transparent backdrop rendered last (via deferred) so
        // it sits above all other content.
        deferred(
            div()
                .id("path-picker-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .w_full()
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x0000008c))
                .child(
                    div()
                        .id("path-picker-dialog")
                        .w(px(520.0))
                        .bg(rgb(0x313244))
                        .border_1()
                        .border_color(rgb(0x45475a))
                        .rounded(px(6.0))
                        // ── title bar ────────────────────────────────────────
                        .child(
                            div()
                                .px_3()
                                .h(px(36.0))
                                .flex()
                                .items_center()
                                .bg(rgb(0x1e1e2e))
                                .border_b_1()
                                .border_color(rgb(0x45475a))
                                .text_color(rgba(0xcdd6f4ff))
                                .text_sm()
                                .child(title),
                        )
                        // ── path row ──────────────────────────────────────────
                        .child(
                            div()
                                .px_3()
                                .h(px(52.0))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.0))
                                .child(
                                    // text field takes all but the browse button
                                    div()
                                        .w(px(428.0))
                                        .border_1()
                                        .border_color(rgb(0x45475a))
                                        .rounded(px(4.0))
                                        .child(path_field),
                                )
                                .child(
                                    div()
                                        .id("path-picker-browse")
                                        .h(px(28.0))
                                        .w(px(36.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .bg(rgb(0x45475a))
                                        .border_1()
                                        .border_color(rgb(0x585b70))
                                        .rounded(px(4.0))
                                        .text_color(rgba(0xcdd6f4ff))
                                        .text_sm()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(rgb(0x585b70)))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(
                                                |this, _: &MouseDownEvent, _window, cx| {
                                                    this.browse(cx);
                                                },
                                            ),
                                        )
                                        .child("…"),
                                ),
                        )
                        // ── footer ────────────────────────────────────────────
                        .child(
                            div()
                                .px_3()
                                .h(px(48.0))
                                .flex()
                                .flex_row()
                                .items_center()
                                .justify_end()
                                .gap(px(8.0))
                                .border_t_1()
                                .border_color(rgb(0x45475a))
                                .child(
                                    div()
                                        .id("path-picker-cancel")
                                        .h(px(28.0))
                                        .px_3()
                                        .flex()
                                        .items_center()
                                        .bg(rgb(0x313244))
                                        .border_1()
                                        .border_color(rgb(0x45475a))
                                        .rounded(px(4.0))
                                        .text_color(rgba(0xcdd6f4ff))
                                        .text_sm()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(rgb(0x45475a)))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(
                                                |this, _: &MouseDownEvent, _window, cx| {
                                                    this.cancel(cx);
                                                },
                                            ),
                                        )
                                        .child("Cancel"),
                                )
                                .child(
                                    div()
                                        .id("path-picker-confirm")
                                        .h(px(28.0))
                                        .px_3()
                                        .flex()
                                        .items_center()
                                        .bg(rgb(0x89b4fa))
                                        .rounded(px(4.0))
                                        .text_color(rgba(0x1e1e2eff))
                                        .text_sm()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(rgb(0xa6d0fd)))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(
                                                |this, _: &MouseDownEvent, _window, cx| {
                                                    this.confirm(cx);
                                                },
                                            ),
                                        )
                                        .child(confirm_label),
                                ),
                        ),
                ),
        )
    }
}
