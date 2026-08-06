//! Small, application-owned GPUI primitives.
//!
//! These components intentionally encode u-forge's interaction states without
//! depending on Zed's UI crate. More specialized controls compose these rather
//! than restyling raw `div` elements independently.

use std::rc::Rc;

use gpui::{
    AnyElement, AnyView, App, ClickEvent, ElementId, IntoElement, ParentElement, Render,
    RenderOnce, SharedString, Window, div, prelude::*, px,
};

use super::icons::{Icon, IconName};
use super::theme::UiTheme;

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

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
        let colors = UiTheme::get(cx).colors;
        div()
            .text_color(match self.tone {
                LabelTone::Primary => colors.text,
                LabelTone::Muted => colors.text_muted,
                LabelTone::Disabled => colors.text_disabled,
                LabelTone::Success => colors.success,
                LabelTone::Warning => colors.warning,
                LabelTone::Danger => colors.danger,
            })
            .map(|label| match self.size {
                LabelSize::Small => label.text_xs(),
                LabelSize::Normal => label.text_sm(),
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

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
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
            .h(px(theme.metrics.control_height))
            .px(px(theme.metrics.space_4))
            .rounded(px(theme.metrics.radius_small))
            .text_sm()
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

        if !self.disabled {
            button = button
                .cursor_pointer()
                .hover(move |style| style.bg(theme.colors.selected));
            if let Some(handler) = self.on_click {
                button = button.on_click(move |event, window, cx| handler(event, window, cx));
            }
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

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
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
            .size(px(theme.metrics.control_height_small))
            .rounded(px(theme.metrics.radius_small))
            .when(self.selected, |button| button.bg(theme.colors.selected))
            .child(Icon::new(self.icon, 13.0, color).rotate_degrees(self.rotation_degrees))
            .tooltip(Tooltip::text(self.accessible_label));
        if !self.disabled {
            button = button
                .cursor_pointer()
                .hover(move |style| style.bg(theme.colors.selected));
            if let Some(handler) = self.on_click {
                button = button.on_click(move |event, window, cx| handler(event, window, cx));
            }
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

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.button = self.button.on_click(handler);
        self
    }
}

impl RenderOnce for Tab {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .child(self.button)
            .when(self.dirty, |tab| tab.child("•"))
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

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
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
            .h(px(theme.metrics.control_height_small))
            .max_w(px(360.0))
            .px(px(theme.metrics.space_2))
            .rounded(px(theme.metrics.radius_small))
            .truncate()
            .text_xs()
            .text_color(color)
            .when(self.active, |item| item.bg(theme.colors.selected))
            .children(self.icon.map(|icon| Icon::new(icon, 12.0, color)))
            .when(self.show_label, |item| item.child(self.label));
        if let Some(handler) = self.on_click {
            item = item
                .cursor_pointer()
                .hover(move |style| style.bg(theme.colors.selected))
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
                .text_xs()
                .text_color(theme.colors.text)
                .child(self.text.clone()),
        )
    }
}

#[derive(IntoElement)]
pub struct Popover {
    id: ElementId,
    child: AnyElement,
}

impl Popover {
    pub fn new(id: impl Into<ElementId>, child: impl IntoElement) -> Self {
        Self {
            id: id.into(),
            child: child.into_any_element(),
        }
    }
}

impl RenderOnce for Popover {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = *UiTheme::get(cx);
        div()
            .id(self.id)
            .bg(theme.colors.elevated_surface)
            .border_1()
            .border_color(theme.colors.border)
            .rounded(px(theme.metrics.radius_medium))
            .overflow_hidden()
            .child(self.child)
    }
}

#[derive(IntoElement)]
pub struct Dialog {
    title: SharedString,
    body: AnyElement,
    actions: Vec<AnyElement>,
}

impl Dialog {
    pub fn new(title: impl Into<SharedString>, body: impl IntoElement) -> Self {
        Self {
            title: title.into(),
            body: body.into_any_element(),
            actions: Vec::new(),
        }
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }
}

impl RenderOnce for Dialog {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = *UiTheme::get(cx);
        div()
            .flex()
            .flex_col()
            .w(px(440.0))
            .bg(theme.colors.elevated_surface)
            .border_1()
            .border_color(theme.colors.border)
            .rounded(px(theme.metrics.radius_medium))
            .child(
                div()
                    .h(px(theme.metrics.panel_header_height))
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
            )
    }
}

pub type Menu = Popover;
pub type ContextMenu = Popover;
