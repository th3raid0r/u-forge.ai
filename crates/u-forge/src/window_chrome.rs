//! Cross-platform policy and rendering primitives for application-owned window chrome.
//!
//! The compositor remains authoritative: callers request client decorations where
//! supported, then select this chrome only from GPUI's negotiated [`Decorations`]
//! value. Desktop-name environment variables are deliberately absent.

use std::rc::Rc;

use gpui::{
    AnyElement, App, BoxShadow, CursorStyle, Decorations, FocusHandle, HitboxBehavior, Hsla,
    IntoElement, KeyDownEvent, MouseButton, Pixels, Point, RenderOnce, ResizeEdge, Size, Tiling,
    Window, WindowControlArea, WindowControls, canvas, div, point, prelude::*, px,
};

use crate::ui::{
    components::Tooltip,
    icons::{Icon, IconName, IconSize},
    theme::UiTheme,
};

pub const APPLICATION_NAME: &str = "u-forge.ai";
pub const APPLICATION_ID: &str = "ai.u-forge.u-forge";

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WindowChromeAction {
    Minimize,
    ToggleMaximize,
    Close,
    Move,
    DoubleClick,
    ShowMenu(Point<Pixels>),
}

pub type WindowChromeActionHandler =
    Rc<dyn Fn(WindowChromeAction, &mut Window, &mut App) + 'static>;

#[derive(Clone)]
pub struct WindowControlFocusHandles {
    pub minimize: FocusHandle,
    pub maximize: FocusHandle,
    pub close: FocusHandle,
}

#[derive(IntoElement)]
pub struct WindowTitleBar {
    side: WindowControlSide,
    controls: WindowControls,
    maximized: bool,
    active: bool,
    focus: WindowControlFocusHandles,
    on_action: WindowChromeActionHandler,
}

impl WindowTitleBar {
    pub fn new(
        side: WindowControlSide,
        controls: WindowControls,
        maximized: bool,
        active: bool,
        focus: WindowControlFocusHandles,
        on_action: WindowChromeActionHandler,
    ) -> Self {
        Self {
            side,
            controls,
            maximized,
            active,
            focus,
            on_action,
        }
    }
}

struct WindowControlButtonSpec {
    id: &'static str,
    icon: IconName,
    label: &'static str,
    area: WindowControlArea,
    focus: FocusHandle,
    action: WindowChromeAction,
}

fn window_control_button(
    spec: WindowControlButtonSpec,
    active: bool,
    handler: WindowChromeActionHandler,
    theme: UiTheme,
) -> impl IntoElement {
    let focus_on_press = spec.focus.clone();
    let click_handler = handler.clone();
    let key_handler = handler;
    let color = if active {
        theme.colors.text
    } else {
        theme.colors.text_muted
    };

    div()
        .id(spec.id)
        .debug_selector(move || spec.id.to_owned())
        .window_control_area(spec.area)
        .track_focus(&spec.focus)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(theme.metrics.title_bar_height)
        .cursor_pointer()
        .text_color(color)
        .hover(move |style| style.bg(theme.colors.selected))
        .active(move |style| style.bg(theme.colors.border))
        .focus_visible(move |style| style.border_1().border_color(theme.colors.focus))
        .tooltip(Tooltip::text(spec.label))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            focus_on_press.focus(window);
            cx.stop_propagation();
        })
        .on_click(move |event, window, cx| {
            if !event.is_right_click() {
                click_handler(spec.action, window, cx);
            }
            cx.stop_propagation();
        })
        .on_key_down(move |event: &KeyDownEvent, window, cx| {
            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                key_handler(spec.action, window, cx);
                cx.stop_propagation();
            }
        })
        .child(Icon::new(spec.icon, IconSize::Medium, color))
}

impl RenderOnce for WindowTitleBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = *UiTheme::get(cx);
        let on_action = self.on_action.clone();
        let show_window_menu = self.controls.window_menu;
        let control_count =
            usize::from(self.controls.minimize) + usize::from(self.controls.maximize) + 1;
        let side_width = theme.metrics.title_bar_height * control_count as f32;
        let mut controls = div()
            .id("window-controls")
            .flex()
            .when(self.side == WindowControlSide::Left, |controls| {
                controls.flex_row_reverse()
            })
            .flex_none()
            .h_full()
            .w(side_width);

        if self.controls.minimize {
            controls = controls.child(window_control_button(
                WindowControlButtonSpec {
                    id: "window-minimize",
                    icon: IconName::WindowMinimize,
                    label: "Minimize window",
                    area: WindowControlArea::Min,
                    focus: self.focus.minimize,
                    action: WindowChromeAction::Minimize,
                },
                self.active,
                self.on_action.clone(),
                theme,
            ));
        }
        if self.controls.maximize {
            controls = controls.child(window_control_button(
                WindowControlButtonSpec {
                    id: "window-maximize",
                    icon: if self.maximized {
                        IconName::WindowRestore
                    } else {
                        IconName::WindowMaximize
                    },
                    label: if self.maximized {
                        "Restore window"
                    } else {
                        "Maximize window"
                    },
                    area: WindowControlArea::Max,
                    focus: self.focus.maximize,
                    action: WindowChromeAction::ToggleMaximize,
                },
                self.active,
                self.on_action.clone(),
                theme,
            ));
        }
        controls = controls.child(window_control_button(
            WindowControlButtonSpec {
                id: "window-close",
                icon: IconName::WindowClose,
                label: "Close window",
                area: WindowControlArea::Close,
                focus: self.focus.close,
                action: WindowChromeAction::Close,
            },
            self.active,
            self.on_action.clone(),
            theme,
        ));

        let (left_side, right_side) = match self.side {
            WindowControlSide::Left => (
                controls.into_any_element(),
                div().flex_none().h_full().w(side_width).into_any_element(),
            ),
            WindowControlSide::Right => (
                div().flex_none().h_full().w(side_width).into_any_element(),
                controls.into_any_element(),
            ),
        };
        let background = if self.active {
            theme.colors.title_bar_surface
        } else {
            theme.colors.title_bar_surface_inactive
        };

        div()
            .id("window-title-bar")
            .debug_selector(|| "window-title-bar".to_owned())
            .window_control_area(WindowControlArea::Drag)
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .w_full()
            .h(theme.metrics.title_bar_height)
            .bg(background)
            .border_b_1()
            .border_color(theme.colors.border_subtle)
            .text_size(theme.typography.chrome)
            .text_color(if self.active {
                theme.colors.text
            } else {
                theme.colors.text_muted
            })
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                on_action(
                    if event.click_count >= 2 {
                        WindowChromeAction::DoubleClick
                    } else {
                        WindowChromeAction::Move
                    },
                    window,
                    cx,
                );
            })
            .when(show_window_menu, |title_bar| {
                title_bar.on_mouse_down(MouseButton::Right, {
                    let on_action = self.on_action.clone();
                    move |event, window, cx| {
                        on_action(WindowChromeAction::ShowMenu(event.position), window, cx);
                    }
                })
            })
            .child(left_side)
            .child(
                div()
                    .id("window-title")
                    .debug_selector(|| "window-title".to_owned())
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(APPLICATION_NAME),
            )
            .child(right_side)
    }
}

#[derive(IntoElement)]
pub struct ClientWindowFrame {
    content: AnyElement,
    geometry: FrameGeometry,
    title_bar: Option<WindowTitleBar>,
}

impl ClientWindowFrame {
    pub fn new(
        content: AnyElement,
        geometry: FrameGeometry,
        title_bar: Option<WindowTitleBar>,
    ) -> Self {
        Self {
            content,
            geometry,
            title_bar,
        }
    }
}

impl RenderOnce for ClientWindowFrame {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = *UiTheme::get(cx);
        let geometry = self.geometry;
        let frame = div()
            .id("client-window-frame")
            .debug_selector(|| "client-window-frame".to_owned())
            .relative()
            .size_full()
            .bg(gpui::transparent_black())
            .pt(px(geometry.inset.top))
            .pr(px(geometry.inset.right))
            .pb(px(geometry.inset.bottom))
            .pl(px(geometry.inset.left))
            .on_mouse_move(|_, window, _| window.refresh())
            .on_mouse_down(MouseButton::Left, move |event, window, _| {
                if let Some(edge) = geometry.resize_edge(event.position, window.viewport_size()) {
                    window.start_window_resize(edge);
                }
            })
            .child(
                canvas(
                    |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
                    move |_, hitbox, window, _| {
                        let Some(edge) =
                            geometry.resize_edge(window.mouse_position(), window.viewport_size())
                        else {
                            return;
                        };
                        let cursor = match edge {
                            ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
                            ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
                            ResizeEdge::TopLeft | ResizeEdge::BottomRight => {
                                CursorStyle::ResizeUpLeftDownRight
                            }
                            ResizeEdge::TopRight | ResizeEdge::BottomLeft => {
                                CursorStyle::ResizeUpRightDownLeft
                            }
                        };
                        window.set_cursor_style(cursor, &hitbox);
                    },
                )
                .absolute()
                .size_full(),
            );

        let mut body = div()
            .id("client-window-body")
            .debug_selector(|| "client-window-body".to_owned())
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden()
            .bg(theme.colors.app_surface)
            .border_t(px(geometry.border.top))
            .border_r(px(geometry.border.right))
            .border_b(px(geometry.border.bottom))
            .border_l(px(geometry.border.left))
            .border_color(theme.colors.border)
            .rounded_tl(px(geometry.corners.top_left))
            .rounded_tr(px(geometry.corners.top_right))
            .rounded_br(px(geometry.corners.bottom_right))
            .rounded_bl(px(geometry.corners.bottom_left));
        if geometry.draw_shadow {
            body = body.shadow(vec![BoxShadow {
                color: Hsla {
                    h: 0.0,
                    s: 0.0,
                    l: 0.0,
                    a: 0.45,
                },
                blur_radius: px(geometry.inset.top / 2.0),
                spread_radius: px(0.0),
                offset: point(px(0.0), px(0.0)),
            }]);
        }
        if let Some(title_bar) = self.title_bar {
            body = body.child(title_bar);
        }
        let mut content = div()
            .id("client-window-content")
            .debug_selector(|| "client-window-content".to_owned())
            .flex()
            .flex_col()
            .min_h_0()
            .child(self.content);
        content.style().flex_grow = Some(1.0);
        content.style().flex_shrink = Some(1.0);
        frame.child(body.child(content))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecorationMode {
    Server,
    Client,
}

impl DecorationMode {
    pub fn negotiated(decorations: Decorations) -> Self {
        match decorations {
            Decorations::Server => Self::Server,
            Decorations::Client { .. } => Self::Client,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowControlSide {
    Left,
    Right,
}

impl WindowControlSide {
    pub fn from_left_preference(left: bool) -> Self {
        if left { Self::Left } else { Self::Right }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EdgeValues {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CornerValues {
    pub top_left: f32,
    pub top_right: f32,
    pub bottom_right: f32,
    pub bottom_left: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameMetrics {
    pub inset: f32,
    pub border: f32,
    pub corner_radius: f32,
    pub resize_width: f32,
}

impl FrameMetrics {
    pub fn for_interface_size(interface_size: f32) -> Self {
        let scale = interface_size.clamp(14.0, 32.0) / 16.0;
        Self {
            inset: 10.0 * scale,
            border: 1.0 * scale,
            corner_radius: 8.0 * scale,
            resize_width: 8.0 * scale,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameGeometry {
    pub inset: EdgeValues,
    pub border: EdgeValues,
    pub corners: CornerValues,
    pub resize_width: f32,
    pub draw_shadow: bool,
    pub show_title_bar: bool,
    pub tiling: Tiling,
}

impl FrameGeometry {
    pub fn for_window(
        reported_tiling: Tiling,
        maximized: bool,
        fullscreen: bool,
        metrics: FrameMetrics,
    ) -> Self {
        if fullscreen {
            return Self {
                inset: EdgeValues::default(),
                border: EdgeValues::default(),
                corners: CornerValues::default(),
                resize_width: 0.0,
                draw_shadow: false,
                show_title_bar: false,
                tiling: Tiling::tiled(),
            };
        }

        let tiling = if maximized {
            Tiling::tiled()
        } else {
            reported_tiling
        };
        let free = |tiled: bool, value: f32| if tiled { 0.0 } else { value };

        Self {
            inset: EdgeValues {
                top: free(tiling.top, metrics.inset),
                right: free(tiling.right, metrics.inset),
                bottom: free(tiling.bottom, metrics.inset),
                left: free(tiling.left, metrics.inset),
            },
            border: EdgeValues {
                top: free(tiling.top, metrics.border),
                right: free(tiling.right, metrics.border),
                bottom: free(tiling.bottom, metrics.border),
                left: free(tiling.left, metrics.border),
            },
            corners: CornerValues {
                top_left: free(tiling.top || tiling.left, metrics.corner_radius),
                top_right: free(tiling.top || tiling.right, metrics.corner_radius),
                bottom_right: free(tiling.bottom || tiling.right, metrics.corner_radius),
                bottom_left: free(tiling.bottom || tiling.left, metrics.corner_radius),
            },
            resize_width: metrics.resize_width,
            draw_shadow: !tiling.is_tiled(),
            show_title_bar: true,
            tiling,
        }
    }

    pub fn resize_edge(&self, position: Point<Pixels>, size: Size<Pixels>) -> Option<ResizeEdge> {
        if self.resize_width <= 0.0 {
            return None;
        }

        let x = f32::from(position.x);
        let y = f32::from(position.y);
        let width = f32::from(size.width);
        let height = f32::from(size.height);
        let near_top = !self.tiling.top && y < self.resize_width;
        let near_right = !self.tiling.right && x > width - self.resize_width;
        let near_bottom = !self.tiling.bottom && y > height - self.resize_width;
        let near_left = !self.tiling.left && x < self.resize_width;

        match (near_top, near_right, near_bottom, near_left) {
            (true, _, _, true) => Some(ResizeEdge::TopLeft),
            (true, true, _, _) => Some(ResizeEdge::TopRight),
            (_, true, true, _) => Some(ResizeEdge::BottomRight),
            (_, _, true, true) => Some(ResizeEdge::BottomLeft),
            (true, _, _, _) => Some(ResizeEdge::Top),
            (_, true, _, _) => Some(ResizeEdge::Right),
            (_, _, true, _) => Some(ResizeEdge::Bottom),
            (_, _, _, true) => Some(ResizeEdge::Left),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui::{
        Context, Decorations, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, Render,
        TestAppContext, Tiling, Window, WindowControls, div, point, prelude::*, px, size,
    };

    use super::{
        ClientWindowFrame, DecorationMode, FrameGeometry, FrameMetrics, ResizeEdge,
        WindowChromeAction, WindowControlFocusHandles, WindowControlSide, WindowTitleBar,
    };
    use crate::UiTheme;

    struct ChromeHarness {
        side: WindowControlSide,
        controls: WindowControls,
        maximized: bool,
        focus: WindowControlFocusHandles,
        actions: Rc<RefCell<Vec<WindowChromeAction>>>,
    }

    impl ChromeHarness {
        fn new(side: WindowControlSide, controls: WindowControls, cx: &mut Context<Self>) -> Self {
            Self {
                side,
                controls,
                maximized: false,
                focus: WindowControlFocusHandles {
                    minimize: cx.focus_handle(),
                    maximize: cx.focus_handle(),
                    close: cx.focus_handle(),
                },
                actions: Rc::default(),
            }
        }
    }

    impl Render for ChromeHarness {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let actions = self.actions.clone();
            let title_bar = WindowTitleBar::new(
                self.side,
                self.controls,
                self.maximized,
                true,
                self.focus.clone(),
                Rc::new(move |action, _, _| actions.borrow_mut().push(action)),
            );
            let geometry = FrameGeometry::for_window(
                Tiling::default(),
                false,
                false,
                FrameMetrics::for_interface_size(UiTheme::get(cx).interface_size),
            );
            div()
                .id("harness-root")
                .debug_selector(|| "harness-root".to_owned())
                .w(px(800.0))
                .h(px(600.0))
                .child(ClientWindowFrame::new(
                    div()
                        .id("harness-content")
                        .debug_selector(|| "harness-content".to_owned())
                        .size_full()
                        .into_any_element(),
                    geometry,
                    Some(title_bar),
                ))
        }
    }

    fn all_controls() -> WindowControls {
        WindowControls {
            fullscreen: true,
            maximize: true,
            minimize: true,
            window_menu: true,
        }
    }

    #[test]
    fn decoration_mode_uses_the_negotiated_gpui_value() {
        assert_eq!(
            DecorationMode::negotiated(Decorations::Server),
            DecorationMode::Server
        );
        assert_eq!(
            DecorationMode::negotiated(Decorations::Client {
                tiling: Tiling::default()
            }),
            DecorationMode::Client
        );
    }

    #[test]
    fn window_control_preference_defaults_to_right_semantics() {
        assert_eq!(
            WindowControlSide::from_left_preference(false),
            WindowControlSide::Right
        );
        assert_eq!(
            WindowControlSide::from_left_preference(true),
            WindowControlSide::Left
        );
    }

    #[test]
    fn floating_frame_has_insets_rounding_shadow_and_resize_edges() {
        let geometry = FrameGeometry::for_window(
            Tiling::default(),
            false,
            false,
            FrameMetrics::for_interface_size(16.0),
        );

        assert_eq!(geometry.inset.top, 10.0);
        assert_eq!(geometry.border.left, 1.0);
        assert_eq!(geometry.corners.top_right, 8.0);
        assert!(geometry.draw_shadow);
        assert!(geometry.show_title_bar);
        assert_eq!(
            geometry.resize_edge(point(px(2.0), px(2.0)), size(px(800.0), px(600.0))),
            Some(ResizeEdge::TopLeft)
        );
    }

    #[test]
    fn tiled_edges_remove_only_their_frame_and_resize_regions() {
        let geometry = FrameGeometry::for_window(
            Tiling {
                top: true,
                left: true,
                right: false,
                bottom: false,
            },
            false,
            false,
            FrameMetrics::for_interface_size(16.0),
        );

        assert_eq!(geometry.inset.top, 0.0);
        assert_eq!(geometry.inset.left, 0.0);
        assert_eq!(geometry.inset.right, 10.0);
        assert_eq!(geometry.corners.top_right, 0.0);
        assert_eq!(geometry.corners.bottom_right, 8.0);
        assert!(!geometry.draw_shadow);
        assert_eq!(
            geometry.resize_edge(point(px(2.0), px(300.0)), size(px(800.0), px(600.0))),
            None
        );
        assert_eq!(
            geometry.resize_edge(point(px(798.0), px(300.0)), size(px(800.0), px(600.0))),
            Some(ResizeEdge::Right)
        );
    }

    #[test]
    fn maximized_and_fullscreen_frames_cannot_resize() {
        let metrics = FrameMetrics::for_interface_size(16.0);
        let maximized = FrameGeometry::for_window(Tiling::default(), true, false, metrics);
        let fullscreen = FrameGeometry::for_window(Tiling::default(), false, true, metrics);

        assert_eq!(maximized.inset, Default::default());
        assert!(!maximized.draw_shadow);
        assert!(maximized.show_title_bar);
        assert_eq!(
            maximized.resize_edge(point(px(0.0), px(0.0)), size(px(800.0), px(600.0))),
            None
        );
        assert!(!fullscreen.show_title_bar);
        assert_eq!(fullscreen.border, Default::default());
        assert_eq!(fullscreen.corners, Default::default());
    }

    #[test]
    fn interface_scale_expands_frame_hit_targets() {
        let compact = FrameMetrics::for_interface_size(16.0);
        let large = FrameMetrics::for_interface_size(24.0);

        assert_eq!(large.inset, compact.inset * 1.5);
        assert_eq!(large.corner_radius, compact.corner_radius * 1.5);
        assert_eq!(large.resize_width, compact.resize_width * 1.5);
    }

    #[gpui::test]
    fn pointer_controls_emit_native_actions_without_triggering_drag(cx: &mut TestAppContext) {
        cx.update(UiTheme::init);
        let (harness, cx) = cx.add_window_view(|_, cx| {
            ChromeHarness::new(WindowControlSide::Right, all_controls(), cx)
        });
        cx.update(|window, app| {
            harness.update(app, |_, cx| cx.notify());
            window.refresh();
        });
        cx.run_until_parked();

        for id in ["window-minimize", "window-maximize", "window-close"] {
            let bounds = cx.debug_bounds(id).expect("control should be rendered");
            cx.simulate_click(bounds.center(), Modifiers::none());
        }

        assert_eq!(
            harness.read_with(cx, |harness, _| harness.actions.borrow().clone()),
            vec![
                WindowChromeAction::Minimize,
                WindowChromeAction::ToggleMaximize,
                WindowChromeAction::Close,
            ]
        );
    }

    #[gpui::test]
    fn title_bar_emits_drag_double_click_and_native_menu_actions(cx: &mut TestAppContext) {
        cx.update(UiTheme::init);
        let (harness, cx) = cx.add_window_view(|_, cx| {
            ChromeHarness::new(WindowControlSide::Right, all_controls(), cx)
        });
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let position = cx.debug_bounds("window-title-bar").unwrap().center();

        cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(position, MouseButton::Left, Modifiers::none());
        cx.simulate_event(MouseDownEvent {
            position,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count: 2,
            first_mouse: false,
        });
        cx.simulate_event(MouseUpEvent {
            position,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count: 2,
        });
        cx.simulate_mouse_down(position, MouseButton::Right, Modifiers::none());
        cx.simulate_mouse_up(position, MouseButton::Right, Modifiers::none());

        let actions = harness.read_with(cx, |harness, _| harness.actions.borrow().clone());
        assert_eq!(actions[0], WindowChromeAction::Move);
        assert_eq!(actions[1], WindowChromeAction::DoubleClick);
        assert!(matches!(actions[2], WindowChromeAction::ShowMenu(_)));
    }

    #[gpui::test]
    fn controls_support_keyboard_activation_but_are_not_workspace_tab_stops(
        cx: &mut TestAppContext,
    ) {
        cx.update(UiTheme::init);
        let (harness, cx) = cx.add_window_view(|_, cx| {
            ChromeHarness::new(WindowControlSide::Right, all_controls(), cx)
        });
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();

        cx.update(|window, app| {
            window.focus_next();
            assert!(window.focused(app).is_none());
            harness.read(app).focus.minimize.focus(window);
        });
        cx.simulate_keystrokes("enter");
        cx.update(|window, app| harness.read(app).focus.maximize.focus(window));
        cx.simulate_keystrokes("space");
        cx.update(|window, app| harness.read(app).focus.close.focus(window));
        cx.simulate_keystrokes("enter");

        assert_eq!(
            harness.read_with(cx, |harness, _| harness.actions.borrow().clone()),
            vec![
                WindowChromeAction::Minimize,
                WindowChromeAction::ToggleMaximize,
                WindowChromeAction::Close,
            ]
        );

        let close = cx.debug_bounds("window-close").unwrap();
        cx.simulate_click(close.center(), Modifiers::none());
        cx.update(|window, app| assert!(harness.read(app).focus.close.is_focused(window)));
    }

    #[gpui::test]
    fn control_side_capabilities_and_interface_scale_drive_layout(cx: &mut TestAppContext) {
        cx.update(|app| UiTheme::set_interface_size(app, 16.0));
        let controls = WindowControls {
            minimize: false,
            maximize: true,
            ..all_controls()
        };
        let (harness, cx) = cx.add_window_view(move |_, cx| {
            ChromeHarness::new(WindowControlSide::Left, controls, cx)
        });
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();

        assert!(cx.debug_bounds("window-minimize").is_none());
        let compact_title = cx.debug_bounds("window-title-bar").unwrap();
        let compact_close = cx.debug_bounds("window-close").unwrap();
        let compact_maximize = cx.debug_bounds("window-maximize").unwrap();
        assert!(compact_close.center().x < compact_title.center().x);
        assert!(compact_close.center().x < compact_maximize.center().x);

        cx.update(|window, app| {
            UiTheme::set_interface_size(app, 24.0);
            harness.update(app, |_, cx| cx.notify());
            window.refresh();
        });
        cx.run_until_parked();
        let large_title = cx.debug_bounds("window-title-bar").unwrap();
        assert_eq!(
            f32::from(large_title.size.height),
            f32::from(compact_title.size.height) * 1.5
        );

        harness.update(cx, |harness, cx| {
            harness.side = WindowControlSide::Right;
            harness.controls.window_menu = false;
            cx.notify();
        });
        cx.run_until_parked();
        let right_close = cx.debug_bounds("window-close").unwrap();
        let right_maximize = cx.debug_bounds("window-maximize").unwrap();
        let title = cx.debug_bounds("window-title-bar").unwrap();
        assert!(right_close.center().x > title.center().x);
        assert!(right_maximize.center().x < right_close.center().x);

        harness.update(cx, |harness, _| harness.actions.borrow_mut().clear());
        let title_position = title.center();
        cx.simulate_mouse_down(title_position, MouseButton::Right, Modifiers::none());
        cx.simulate_mouse_up(title_position, MouseButton::Right, Modifiers::none());
        assert!(harness.read_with(cx, |harness, _| harness.actions.borrow().is_empty()));
    }
}
