use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, Render, Window, deferred, div, prelude::*,
};

use crate::ui::components::{Button, ButtonStyle, Dialog};
use crate::ui::theme::UiTheme;

pub(crate) struct ConfirmationAccepted;
pub(crate) struct ConfirmationAlternative;
pub(crate) struct ConfirmationCancelled;

/// Modal confirmation with an owned focus scope and an explicit return target.
///
/// The first rendered frame moves focus into the dialog. Every completion path
/// restores the handle that was active before the dialog opened, before the
/// owner removes this entity from the workspace tree.
pub(crate) struct ConfirmationModal {
    title: String,
    message: String,
    confirm_label: String,
    cancel_label: String,
    alternative_label: Option<String>,
    destructive: bool,
    focus: FocusHandle,
    return_focus: FocusHandle,
    focus_pending: bool,
}

impl EventEmitter<ConfirmationAccepted> for ConfirmationModal {}
impl EventEmitter<ConfirmationAlternative> for ConfirmationModal {}
impl EventEmitter<ConfirmationCancelled> for ConfirmationModal {}

impl Focusable for ConfirmationModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ConfirmationModal {
    pub(crate) fn new(
        title: String,
        message: String,
        confirm_label: String,
        return_focus: FocusHandle,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            title,
            message,
            confirm_label,
            cancel_label: "Cancel".to_string(),
            alternative_label: None,
            destructive: true,
            focus: cx.focus_handle(),
            return_focus,
            focus_pending: true,
        }
    }

    pub(crate) fn with_alternative(mut self, label: impl Into<String>) -> Self {
        self.alternative_label = Some(label.into());
        self
    }

    pub(crate) fn with_cancel_label(mut self, label: impl Into<String>) -> Self {
        self.cancel_label = label.into();
        self
    }

    pub(crate) fn non_destructive(mut self) -> Self {
        self.destructive = false;
        self
    }

    fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.return_focus.focus(window);
        cx.emit(ConfirmationAccepted);
    }

    fn choose_alternative(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.return_focus.focus(window);
        cx.emit(ConfirmationAlternative);
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.return_focus.focus(window);
        cx.emit(ConfirmationCancelled);
    }
}

impl Render for ConfirmationModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *UiTheme::get(cx);
        if self.focus_pending {
            self.focus_pending = false;
            let focus = self.focus.clone();
            window.defer(cx, move |window, _cx| focus.focus(window));
        }

        let accept = cx.weak_entity();
        let cancel = cx.weak_entity();
        let dismiss_cancel = cancel.clone();
        let mut dialog = Dialog::new(
            self.title.clone(),
            div()
                .text_size(theme.typography.label)
                .text_color(theme.colors.text_muted)
                .child(self.message.clone()),
        )
        .id("confirmation-dialog")
        .focus_handle(self.focus.clone())
        .return_focus(self.return_focus.clone())
        .on_dismiss(move |window, cx| {
            dismiss_cancel
                .update(cx, |modal, cx| modal.cancel(window, cx))
                .ok();
        });

        if let Some(label) = self.alternative_label.clone() {
            let alternative = cx.weak_entity();
            dialog = dialog.action(
                Button::new("confirmation-alternative", label)
                    .style(ButtonStyle::Danger)
                    .on_click(move |_, window, cx| {
                        alternative
                            .update(cx, |modal, cx| modal.choose_alternative(window, cx))
                            .ok();
                    }),
            );
        }

        dialog = dialog
            .action(
                Button::new("confirmation-cancel", self.cancel_label.clone()).on_click(
                    move |_, window, cx| {
                        cancel.update(cx, |modal, cx| modal.cancel(window, cx)).ok();
                    },
                ),
            )
            .action(
                Button::new("confirmation-accept", self.confirm_label.clone())
                    .style(if self.destructive {
                        ButtonStyle::Danger
                    } else {
                        ButtonStyle::Filled
                    })
                    .on_click(move |_, window, cx| {
                        accept.update(cx, |modal, cx| modal.accept(window, cx)).ok();
                    }),
            );

        deferred(
            div()
                .id("confirmation-backdrop")
                .absolute()
                .size_full()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme.colors.overlay)
                .child(dialog),
        )
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::ConfirmationModal;
    use crate::ui::theme::UiTheme;

    #[gpui::test]
    fn confirmation_claims_focus_and_escape_restores_the_caller(cx: &mut TestAppContext) {
        cx.update(UiTheme::init);
        let (modal, cx) = cx.add_window_view(|window, cx| {
            let return_focus = cx.focus_handle();
            return_focus.focus(window);
            ConfirmationModal::new(
                "Confirm".into(),
                "Continue?".into(),
                "Continue".into(),
                return_focus,
                cx,
            )
        });

        cx.update(|window, _app| window.refresh());
        cx.run_until_parked();
        cx.update(|window, app| assert!(modal.read(app).focus.is_focused(window)));

        cx.simulate_keystrokes("escape");
        cx.update(|window, app| assert!(modal.read(app).return_focus.is_focused(window)));
    }
}
