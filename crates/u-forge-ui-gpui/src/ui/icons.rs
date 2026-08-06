//! Monochrome application icons rendered through GPUI's SVG mask pipeline.

use gpui::{App, IntoElement, RenderOnce, Rgba, Window, prelude::*, px, svg};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconName {
    TabClose,
    TabPinOutline,
    TabPinFilled,
}

impl IconName {
    const fn path(self) -> &'static str {
        match self {
            Self::TabClose => "icons/tab-close.svg",
            Self::TabPinOutline => "icons/tab-pin-outline.svg",
            Self::TabPinFilled => "icons/tab-pin-filled.svg",
        }
    }
}

#[derive(IntoElement)]
pub struct Icon {
    name: IconName,
    size: f32,
    color: Rgba,
}

impl Icon {
    pub fn new(name: IconName, size: f32, color: Rgba) -> Self {
        Self { name, size, color }
    }
}

impl RenderOnce for Icon {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        svg()
            .path(self.name.path())
            .size(px(self.size))
            .flex_none()
            .text_color(self.color)
    }
}
