//! Small, application-owned GPUI primitives.
//!
//! These components intentionally encode u-forge's interaction states without
//! depending on Zed's UI crate. More specialized controls compose these rather
//! than restyling raw `div` elements independently.

use std::rc::Rc;

use gpui::{
    Action, AnyElement, AnyView, App, ClickEvent, ElementId, FocusHandle, IntoElement,
    KeyDownEvent, MouseDownEvent, ParentElement, Render, RenderOnce, SharedString, Window, div,
    prelude::*, px,
};

use super::icons::{Icon, IconName, IconSize};
use super::theme::UiTheme;

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
type DismissHandler = Rc<dyn Fn(&mut Window, &mut App)>;

fn action_handler(action: impl Action) -> ClickHandler {
    let action = action.boxed_clone();
    Rc::new(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelTone {
    Primary,
    Muted,
    Disabled,
    Success,
    Warning,
    Danger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelSize {
    Small,
    Normal,
}

fn label_tone_color(tone: LabelTone, theme: UiTheme) -> gpui::Rgba {
    match tone {
        LabelTone::Primary => theme.colors.text,
        LabelTone::Muted => theme.colors.text_muted,
        LabelTone::Disabled => theme.colors.text_disabled,
        LabelTone::Success => theme.colors.success,
        LabelTone::Warning => theme.colors.warning,
        LabelTone::Danger => theme.colors.danger,
    }
}

#[derive(IntoElement)]
pub struct Label {
    text: SharedString,
    tone: LabelTone,
    size: LabelSize,
}

impl Label {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            tone: LabelTone::Primary,
            size: LabelSize::Normal,
        }
    }

    pub fn tone(mut self, tone: LabelTone) -> Self {
        self.tone = tone;
        self
    }

    pub fn size(mut self, size: LabelSize) -> Self {
        self.size = size;
        self
    }
}

impl RenderOnce for Label {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = *UiTheme::get(cx);
        div()
            .text_color(label_tone_color(self.tone, theme))
            .map(|label| match self.size {
                LabelSize::Small => label.text_size(theme.typography.caption),
                LabelSize::Normal => label.text_size(theme.typography.label),
            })
            .child(self.text)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonStyle {
    Ghost,
    Filled,
    Danger,
}

#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: SharedString,
    style: ButtonStyle,
    disabled: bool,
    selected: bool,
    tooltip: Option<SharedString>,
    focus_handle: Option<FocusHandle>,
    on_click: Option<ClickHandler>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            style: ButtonStyle::Ghost,
            disabled: false,
            selected: false,
            tooltip: None,
            focus_handle: None,
            on_click: None,
        }
    }

    pub fn style(mut self, style: ButtonStyle) -> Self {
        self.style = style;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    pub fn focus_handle(mut self, focus_handle: FocusHandle) -> Self {
        self.focus_handle = Some(focus_handle);
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }

    pub fn action(mut self, action: impl Action) -> Self {
        self.on_click = Some(action_handler(action));
        self
    }
}

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = *UiTheme::get(cx);
        let mut button = div()
            .id(self.id)
            .flex()
            .items_center()
            .justify_center()
            .h(theme.metrics.control_height)
            .px(px(theme.metrics.space_4))
            .rounded(px(theme.metrics.radius_small))
            .text_size(theme.typography.label)
            .text_color(if self.disabled {
                theme.colors.text_disabled
            } else if self.style == ButtonStyle::Filled {
                theme.colors.text_inverse
            } else if self.style == ButtonStyle::Danger {
                theme.colors.danger
            } else {
                theme.colors.text
            })
            .bg(if self.selected {
                theme.colors.selected
            } else if self.style == ButtonStyle::Filled {
                theme.colors.accent
            } else {
                theme.colors.panel_surface
            })
            .child(self.label);

        if !self.disabled
            && let Some(handler) = self.on_click
        {
            button = match self.focus_handle {
                Some(focus_handle) => button.track_focus(&focus_handle),
                None => button.tab_index(0),
            }
            .cursor_pointer()
            .hover(move |style| style.bg(theme.colors.selected))
            .active(move |style| style.bg(theme.colors.border))
            .focus_visible(move |style| style.border_1().border_color(theme.colors.focus));
            button = button.on_click(move |event, window, cx| handler(event, window, cx));
        }
        if let Some(tooltip) = self.tooltip {
            button = button.tooltip(Tooltip::text(tooltip));
        }
        button
    }
}

#[derive(IntoElement)]
pub struct IconButton {
    id: ElementId,
    icon: IconName,
    accessible_label: SharedString,
    disabled: bool,
    selected: bool,
    color: Option<gpui::Rgba>,
    rotation_degrees: f32,
    focus_handle: Option<FocusHandle>,
    on_click: Option<ClickHandler>,
}

impl IconButton {
    pub fn new(
        id: impl Into<ElementId>,
        icon: IconName,
        accessible_label: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            icon,
            accessible_label: accessible_label.into(),
            disabled: false,
            selected: false,
            color: None,
            rotation_degrees: 0.0,
            focus_handle: None,
            on_click: None,
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn color(mut self, color: gpui::Rgba) -> Self {
        self.color = Some(color);
        self
    }

    pub fn rotate_degrees(mut self, degrees: f32) -> Self {
        self.rotation_degrees = degrees;
        self
    }

    pub fn focus_handle(mut self, focus_handle: FocusHandle) -> Self {
        self.focus_handle = Some(focus_handle);
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }

    pub fn action(mut self, action: impl Action) -> Self {
        self.on_click = Some(action_handler(action));
        self
    }
}

impl RenderOnce for IconButton {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = *UiTheme::get(cx);
        let color = if self.disabled {
            theme.colors.text_disabled
        } else if self.selected {
            theme.colors.accent
        } else {
            self.color.unwrap_or(theme.colors.text_muted)
        };
        let mut button = div()
            .id(self.id)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(theme.metrics.control_height_small)
            .rounded(px(theme.metrics.radius_small))
            .when(self.selected, |button| button.bg(theme.colors.selected))
            .child(
                Icon::new(self.icon, IconSize::Medium, color).rotate_degrees(self.rotation_degrees),
            )
            .tooltip(Tooltip::text(self.accessible_label));
        if !self.disabled
            && let Some(handler) = self.on_click
        {
            button = match self.focus_handle {
                Some(focus_handle) => button.track_focus(&focus_handle),
                None => button.tab_index(0),
            }
            .cursor_pointer()
            .hover(move |style| style.bg(theme.colors.selected))
            .active(move |style| style.bg(theme.colors.border))
            .focus_visible(move |style| style.border_1().border_color(theme.colors.focus));
            button = button.on_click(move |event, window, cx| handler(event, window, cx));
        }
        button
    }
}

#[derive(IntoElement)]
pub struct Tab {
    button: Button,
    dirty: bool,
}

impl Tab {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool) -> Self {
        Self {
            button: Button::new(id, label).selected(selected),
            dirty: false,
        }
    }

    pub fn dirty(mut self, dirty: bool) -> Self {
        self.dirty = dirty;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.button = self.button.disabled(disabled);
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.button = self.button.tooltip(tooltip);
        self
    }

    pub fn focus_handle(mut self, focus_handle: FocusHandle) -> Self {
        self.button = self.button.focus_handle(focus_handle);
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.button = self.button.on_click(handler);
        self
    }

    pub fn action(mut self, action: impl Action) -> Self {
        self.button = self.button.action(action);
        self
    }
}

impl RenderOnce for Tab {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = *UiTheme::get(cx);
        div()
            .flex()
            .items_center()
            .child(self.button)
            .when(self.dirty, |tab| {
                tab.child(
                    div()
                        .size(px(theme.metrics.space_2))
                        .rounded_full()
                        .bg(theme.colors.accent),
                )
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusTone {
    Muted,
    Normal,
    Success,
    Warning,
    Danger,
}

#[derive(IntoElement)]
pub struct StatusItem {
    id: ElementId,
    label: SharedString,
    icon: Option<IconName>,
    show_label: bool,
    active: bool,
    tone: StatusTone,
    tooltip: Option<SharedString>,
    focus_handle: Option<FocusHandle>,
    on_click: Option<ClickHandler>,
}

impl StatusItem {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: None,
            show_label: true,
            active: false,
            tone: StatusTone::Normal,
            tooltip: None,
            focus_handle: None,
            on_click: None,
        }
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn icon_only(mut self) -> Self {
        self.show_label = false;
        self
    }

    pub fn tone(mut self, tone: StatusTone) -> Self {
        self.tone = tone;
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    pub fn focus_handle(mut self, focus_handle: FocusHandle) -> Self {
        self.focus_handle = Some(focus_handle);
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }

    pub fn action(mut self, action: impl Action) -> Self {
        self.on_click = Some(action_handler(action));
        self
    }
}

impl RenderOnce for StatusItem {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = *UiTheme::get(cx);
        let color = match self.tone {
            StatusTone::Muted => theme.colors.text_muted,
            StatusTone::Normal => theme.colors.text,
            StatusTone::Success => theme.colors.success,
            StatusTone::Warning => theme.colors.warning,
            StatusTone::Danger => theme.colors.danger,
        };
        let mut item = div()
            .id(self.id)
            .flex()
            .items_center()
            .gap(px(theme.metrics.space_2))
            .h(theme.metrics.control_height_small)
            .max_w(px(360.0))
            .px(px(theme.metrics.space_2))
            .rounded(px(theme.metrics.radius_small))
            .truncate()
            .text_size(theme.typography.chrome)
            .text_color(color)
            .when(self.active, |item| item.bg(theme.colors.selected))
            .children(
                self.icon
                    .map(|icon| Icon::new(icon, IconSize::Small, color)),
            )
            .when(self.show_label, |item| item.child(self.label));
        if let Some(handler) = self.on_click {
            item = match self.focus_handle {
                Some(focus_handle) => item.track_focus(&focus_handle),
                None => item.tab_index(0),
            }
            .cursor_pointer()
            .hover(move |style| style.bg(theme.colors.selected))
            .active(move |style| style.bg(theme.colors.border))
            .focus_visible(move |style| style.border_1().border_color(theme.colors.focus))
            .on_click(move |event, window, cx| handler(event, window, cx));
        }
        if let Some(tooltip) = self.tooltip {
            item = item.tooltip(Tooltip::text(tooltip));
        }
        item
    }
}

pub struct Tooltip {
    text: SharedString,
}

impl Tooltip {
    pub fn text(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView {
        let text = text.into();
        move |_window, cx| cx.new(|_| Self { text: text.clone() }).into()
    }
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = *UiTheme::get(cx);
        div().pl_2().pt_2().child(
            div()
                .max_w(px(320.0))
                .px_2()
                .py_1()
                .rounded(px(theme.metrics.radius_small))
                .bg(theme.colors.elevated_surface)
                .border_1()
                .border_color(theme.colors.border)
                .text_size(theme.typography.caption)
                .text_color(theme.colors.text)
                .child(self.text.clone()),
        )
    }
}

#[derive(IntoElement)]
pub struct Popover {
    id: ElementId,
    child: AnyElement,
    on_dismiss: Option<DismissHandler>,
}

impl Popover {
    pub fn new(id: impl Into<ElementId>, child: impl IntoElement) -> Self {
        Self {
            id: id.into(),
            child: child.into_any_element(),
            on_dismiss: None,
        }
    }

    pub fn on_dismiss(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Popover {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = *UiTheme::get(cx);
        let mut popover = div()
            .id(self.id)
            .focusable()
            .bg(theme.colors.elevated_surface)
            .border_1()
            .border_color(theme.colors.border)
            .rounded(px(theme.metrics.radius_medium))
            .overflow_hidden()
            .focus_visible(move |style| style.border_color(theme.colors.focus))
            .child(self.child);

        if let Some(on_dismiss) = self.on_dismiss {
            let dismiss_on_click_out = on_dismiss.clone();
            popover = popover
                .on_mouse_down_out(move |_: &MouseDownEvent, window, cx| {
                    dismiss_on_click_out(window, cx);
                })
                .on_key_down(move |event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        on_dismiss(window, cx);
                        cx.stop_propagation();
                    }
                });
        }

        popover
    }
}

#[derive(IntoElement)]
pub struct Menu {
    popover: Popover,
}

impl Menu {
    pub fn new(id: impl Into<ElementId>, child: impl IntoElement) -> Self {
        Self {
            popover: Popover::new(id, child),
        }
    }

    pub fn on_dismiss(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.popover = self.popover.on_dismiss(handler);
        self
    }
}

impl RenderOnce for Menu {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        self.popover
    }
}

#[derive(IntoElement)]
pub struct ContextMenu {
    menu: Menu,
}

impl ContextMenu {
    pub fn new(id: impl Into<ElementId>, child: impl IntoElement) -> Self {
        Self {
            menu: Menu::new(id, child),
        }
    }

    pub fn on_dismiss(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.menu = self.menu.on_dismiss(handler);
        self
    }
}

impl RenderOnce for ContextMenu {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        self.menu
    }
}

#[derive(IntoElement)]
pub struct MenuItem {
    id: ElementId,
    label: SharedString,
    shortcut: Option<SharedString>,
    selected: bool,
    disabled: bool,
    tone: LabelTone,
    tooltip: Option<SharedString>,
    focus_handle: Option<FocusHandle>,
    on_click: Option<ClickHandler>,
}

impl MenuItem {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            shortcut: None,
            selected: false,
            disabled: false,
            tone: LabelTone::Primary,
            tooltip: None,
            focus_handle: None,
            on_click: None,
        }
    }

    pub fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn tone(mut self, tone: LabelTone) -> Self {
        self.tone = tone;
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    pub fn focus_handle(mut self, focus_handle: FocusHandle) -> Self {
        self.focus_handle = Some(focus_handle);
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }

    pub fn action(mut self, action: impl Action) -> Self {
        self.on_click = Some(action_handler(action));
        self
    }
}

impl RenderOnce for MenuItem {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = *UiTheme::get(cx);
        let mut item = div()
            .id(self.id)
            .flex()
            .items_center()
            .justify_between()
            .gap(px(theme.metrics.space_6))
            .h(theme.metrics.control_height)
            .px(px(theme.metrics.space_4))
            .text_size(theme.typography.label)
            .text_color(if self.disabled {
                theme.colors.text_disabled
            } else {
                label_tone_color(self.tone, theme)
            })
            .when(self.selected, |item| item.bg(theme.colors.selected))
            .child(self.label)
            .when_some(self.shortcut, |item, shortcut| {
                item.child(
                    div()
                        .text_size(theme.typography.caption)
                        .text_color(if self.disabled {
                            theme.colors.text_disabled
                        } else {
                            theme.colors.text_muted
                        })
                        .child(shortcut),
                )
            });

        if !self.disabled
            && let Some(handler) = self.on_click
        {
            item = match self.focus_handle {
                Some(focus_handle) => item.track_focus(&focus_handle),
                None => item.tab_index(0),
            }
            .cursor_pointer()
            .hover(move |style| style.bg(theme.colors.selected))
            .active(move |style| style.bg(theme.colors.border))
            .focus_visible(move |style| style.border_1().border_color(theme.colors.focus))
            .on_click(move |event, window, cx| handler(event, window, cx));
        }
        if let Some(tooltip) = self.tooltip {
            item = item.tooltip(Tooltip::text(tooltip));
        }
        item
    }
}

#[derive(IntoElement)]
pub struct Dialog {
    id: ElementId,
    title: SharedString,
    body: AnyElement,
    actions: Vec<AnyElement>,
    on_dismiss: Option<DismissHandler>,
}

impl Dialog {
    pub fn new(title: impl Into<SharedString>, body: impl IntoElement) -> Self {
        Self {
            id: "dialog".into(),
            title: title.into(),
            body: body.into_any_element(),
            actions: Vec::new(),
            on_dismiss: None,
        }
    }

    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = id.into();
        self
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }

    pub fn on_dismiss(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Dialog {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = *UiTheme::get(cx);
        let mut dialog = div()
            .id(self.id)
            .focusable()
            .flex()
            .flex_col()
            .w(px(440.0))
            .bg(theme.colors.elevated_surface)
            .border_1()
            .border_color(theme.colors.border)
            .rounded(px(theme.metrics.radius_medium))
            .focus_visible(move |style| style.border_color(theme.colors.focus))
            .child(
                div()
                    .h(theme.metrics.panel_header_height)
                    .px_3()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.colors.border_subtle)
                    .child(Label::new(self.title)),
            )
            .child(div().p_3().child(self.body))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(theme.metrics.space_2))
                    .p_3()
                    .children(self.actions),
            );

        if let Some(on_dismiss) = self.on_dismiss {
            dialog = dialog.on_key_down(move |event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    on_dismiss(window, cx);
                    cx.stop_propagation();
                }
            });
        }

        dialog
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use gpui::{
        Context, FocusHandle, KeyUpEvent, Keystroke, Modifiers, Render, TestAppContext, Window,
        actions, div, point, prelude::*, px,
    };

    use super::{Button, Dialog, Menu, MenuItem};
    use crate::ui::theme::UiTheme;

    actions!(component_tests, [Activate]);

    struct PrimitiveHarness {
        focus: FocusHandle,
        action_focus: FocusHandle,
        menu_focus: FocusHandle,
        dialog_focus: FocusHandle,
        activations: Rc<Cell<usize>>,
        menu_dismissals: Rc<Cell<usize>>,
        dialog_dismissals: Rc<Cell<usize>>,
    }

    impl Render for PrimitiveHarness {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let menu_dismissals = self.menu_dismissals.clone();
            let dialog_dismissals = self.dialog_dismissals.clone();
            div()
                .id("primitive-harness")
                .track_focus(&self.focus)
                .flex()
                .flex_col()
                .w(px(320.0))
                .h(px(240.0))
                .on_action(cx.listener(|this, _: &Activate, _window, _cx| {
                    this.activations.set(this.activations.get() + 1);
                }))
                .child(Button::new("disabled-button", "Disabled").disabled(true))
                .child(
                    Button::new("action-button", "Action")
                        .focus_handle(self.action_focus.clone())
                        .action(Activate),
                )
                .child(
                    Menu::new(
                        "test-menu",
                        MenuItem::new("menu-item", "Menu item")
                            .focus_handle(self.menu_focus.clone())
                            .on_click(|_, _, _| {}),
                    )
                    .on_dismiss(move |_, _| menu_dismissals.set(menu_dismissals.get() + 1)),
                )
                .child(
                    Dialog::new("Test dialog", div().child("Body"))
                        .action(
                            Button::new("dialog-action", "Dialog action")
                                .focus_handle(self.dialog_focus.clone())
                                .on_click(|_, _, _| {}),
                        )
                        .on_dismiss(move |_, _| dialog_dismissals.set(dialog_dismissals.get() + 1)),
                )
        }
    }

    #[gpui::test]
    fn controls_are_keyboard_activatable_and_menus_share_dismissal(cx: &mut TestAppContext) {
        cx.update(UiTheme::init);
        let activations = Rc::new(Cell::new(0));
        let menu_dismissals = Rc::new(Cell::new(0));
        let dialog_dismissals = Rc::new(Cell::new(0));
        let harness_activations = activations.clone();
        let harness_menu_dismissals = menu_dismissals.clone();
        let harness_dialog_dismissals = dialog_dismissals.clone();
        let (harness, cx) = cx.add_window_view(|_window, cx| PrimitiveHarness {
            focus: cx.focus_handle(),
            action_focus: cx.focus_handle().tab_index(0).tab_stop(true),
            menu_focus: cx.focus_handle().tab_index(1).tab_stop(true),
            dialog_focus: cx.focus_handle().tab_index(2).tab_stop(true),
            activations: harness_activations,
            menu_dismissals: harness_menu_dismissals,
            dialog_dismissals: harness_dialog_dismissals,
        });

        cx.update(|window, app| {
            harness.read(app).focus.focus(window);
            window.refresh();
        });
        cx.run_until_parked();

        // Disabled controls are omitted from tab order, so the first tab stop
        // is the enabled action-backed button.
        cx.update(|window, app| {
            harness.read(app).action_focus.focus(window);
            window.refresh();
        });
        cx.run_until_parked();
        cx.simulate_event(KeyUpEvent {
            keystroke: Keystroke::parse("enter").unwrap(),
        });
        assert_eq!(activations.get(), 1);

        // The next tab stop is the menu item. Escape bubbles through the item
        // to the menu's shared dismissal handler.
        cx.update(|window, app| {
            harness.read(app).menu_focus.focus(window);
            window.refresh();
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        assert_eq!(menu_dismissals.get(), 1);
        assert_eq!(dialog_dismissals.get(), 0);

        // Dialogs use the same Escape contract but do not dismiss on outside
        // presses, leaving modal backdrop policy to their owner.
        cx.update(|window, app| {
            harness.read(app).dialog_focus.focus(window);
            window.refresh();
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        assert_eq!(dialog_dismissals.get(), 1);

        // Pointer presses outside the popover use the same dismissal path.
        cx.simulate_mouse_down(
            point(px(700.0), px(500.0)),
            gpui::MouseButton::Left,
            Modifiers::none(),
        );
        assert_eq!(menu_dismissals.get(), 2);
        assert_eq!(dialog_dismissals.get(), 1);
    }
}
