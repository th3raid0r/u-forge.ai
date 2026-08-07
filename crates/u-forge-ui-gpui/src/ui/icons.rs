//! Monochrome application icons rendered through GPUI's SVG mask pipeline.

use gpui::{App, IntoElement, RenderOnce, Rgba, Transformation, Window, prelude::*, radians, svg};

use super::theme::UiTheme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconName {
    Bot,
    ChevronDown,
    ChevronRight,
    Close,
    CloseCircle,
    Copy,
    Edit,
    FloppyDisc,
    Maximize,
    MoreHorizontal,
    Minus,
    MinusCircle,
    Plus,
    PlusCircle,
    Refresh,
    SaveAll,
    Search,
    Send,
    TabClose,
    TabPinOutline,
    TabPinFilled,
    Thinking,
    Trash,
    User,
    WarningTriangle,
    World,
    ZoomIn,
    ZoomOut,
}

/// Semantic icon sizes tied to the root font size by [`UiTheme`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconSize {
    Small,
    Medium,
    Large,
}

impl IconName {
    const fn path(self) -> &'static str {
        match self {
            Self::Bot => "icons/bot.svg",
            Self::ChevronDown => "icons/chevron-down.svg",
            Self::ChevronRight => "icons/chevron-right.svg",
            Self::Close => "icons/close.svg",
            Self::CloseCircle => "icons/close-circle.svg",
            Self::Copy => "icons/copy.svg",
            Self::Edit => "icons/edit.svg",
            Self::FloppyDisc => "icons/floppy-disc.svg",
            Self::Maximize => "icons/maximize.svg",
            Self::MoreHorizontal => "icons/more-horizontal.svg",
            Self::Minus => "icons/minus.svg",
            Self::MinusCircle => "icons/minus-circle.svg",
            Self::Plus => "icons/plus.svg",
            Self::PlusCircle => "icons/plus-circle.svg",
            Self::Refresh => "icons/refresh.svg",
            Self::SaveAll => "icons/save-all.svg",
            Self::Search => "icons/search.svg",
            Self::Send => "icons/send.svg",
            Self::TabClose => "icons/tab-close.svg",
            Self::TabPinOutline => "icons/tab-pin-outline.svg",
            Self::TabPinFilled => "icons/tab-pin-filled.svg",
            Self::Thinking => "icons/thinking.svg",
            Self::Trash => "icons/trash.svg",
            Self::User => "icons/user.svg",
            Self::WarningTriangle => "icons/warning-triangle.svg",
            Self::World => "icons/world.svg",
            Self::ZoomIn => "icons/zoom-in.svg",
            Self::ZoomOut => "icons/zoom-out.svg",
        }
    }
}

#[derive(IntoElement)]
pub struct Icon {
    name: IconName,
    size: IconSize,
    color: Rgba,
    rotation_degrees: f32,
}

impl Icon {
    pub fn new(name: IconName, size: IconSize, color: Rgba) -> Self {
        Self {
            name,
            size,
            color,
            rotation_degrees: 0.0,
        }
    }

    pub fn rotate_degrees(mut self, degrees: f32) -> Self {
        self.rotation_degrees = degrees;
        self
    }
}

impl RenderOnce for Icon {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let typography = UiTheme::get(cx).typography;
        let size = match self.size {
            IconSize::Small => typography.icon_small,
            IconSize::Medium => typography.icon_medium,
            IconSize::Large => typography.icon_large,
        };
        svg()
            .path(self.name.path())
            .with_transformation(Transformation::rotate(radians(
                self.rotation_degrees.to_radians(),
            )))
            .size(size)
            .flex_none()
            .text_color(self.color)
    }
}
