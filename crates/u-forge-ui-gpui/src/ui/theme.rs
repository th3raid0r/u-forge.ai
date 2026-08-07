//! Semantic UI tokens for the desktop application.
//!
//! Graph-specific node colors deliberately remain in `u-forge-ui-traits`;
//! these tokens describe application chrome and interactive controls.

use gpui::{App, Global, Rems, Rgba, rems, rgba};

#[derive(Debug, Clone, Copy)]
pub struct UiColors {
    pub app_surface: Rgba,
    pub panel_surface: Rgba,
    pub elevated_surface: Rgba,
    pub input_surface: Rgba,
    pub overlay: Rgba,
    pub border: Rgba,
    pub border_subtle: Rgba,
    pub text: Rgba,
    pub text_muted: Rgba,
    pub text_disabled: Rgba,
    pub text_inverse: Rgba,
    pub accent: Rgba,
    pub selected: Rgba,
    pub success: Rgba,
    pub warning: Rgba,
    pub danger: Rgba,
    pub focus: Rgba,
}

#[derive(Debug, Clone, Copy)]
pub struct UiMetrics {
    pub space_1: f32,
    pub space_2: f32,
    pub space_3: f32,
    pub space_4: f32,
    pub space_6: f32,
    pub radius_small: f32,
    pub radius_medium: f32,
    pub control_height_small: Rems,
    pub control_height: Rems,
    pub menu_bar_height: Rems,
    pub panel_header_height: Rems,
    pub status_bar_height: Rems,
}

/// Relative type and icon sizes. Keeping both in rems makes the entire chrome
/// follow the user-selected root font size instead of leaving SVGs behind at a
/// fixed pixel size.
#[derive(Debug, Clone, Copy)]
pub struct UiTypography {
    pub body: Rems,
    pub label: Rems,
    pub chrome: Rems,
    pub caption: Rems,
    pub icon_small: Rems,
    pub icon_medium: Rems,
    pub icon_large: Rems,
}

#[derive(Debug, Clone, Copy)]
pub struct UiTheme {
    pub colors: UiColors,
    pub metrics: UiMetrics,
    pub typography: UiTypography,
}

impl Global for UiTheme {}

impl Default for UiTheme {
    fn default() -> Self {
        Self {
            colors: UiColors {
                app_surface: rgba(0x1e1e2eff),
                panel_surface: rgba(0x181825ff),
                elevated_surface: rgba(0x313244ff),
                input_surface: rgba(0x11111bff),
                overlay: rgba(0x0000008c),
                border: rgba(0x45475aff),
                border_subtle: rgba(0x313244ff),
                text: rgba(0xcdd6f4ff),
                text_muted: rgba(0xa6adc8ff),
                text_disabled: rgba(0x6c7086ff),
                text_inverse: rgba(0x1e1e2eff),
                accent: rgba(0x89b4faff),
                selected: rgba(0x45475aaa),
                success: rgba(0xa6e3a1ff),
                warning: rgba(0xf9e2afff),
                danger: rgba(0xf38ba8ff),
                focus: rgba(0xb4befeff),
            },
            metrics: UiMetrics {
                space_1: 2.0,
                space_2: 4.0,
                space_3: 6.0,
                space_4: 8.0,
                space_6: 12.0,
                radius_small: 3.0,
                radius_medium: 6.0,
                control_height_small: rems(1.375),
                control_height: rems(1.75),
                menu_bar_height: rems(1.75),
                panel_header_height: rems(1.75),
                status_bar_height: rems(1.5),
            },
            typography: UiTypography {
                body: rems(1.0),
                label: rems(0.875),
                // Halfway between GPUI's old 0.75-rem extra-small chrome and
                // the 0.875-rem labels, while remaining below body copy.
                chrome: rems(0.8125),
                caption: rems(0.75),
                icon_small: rems(0.75),
                icon_medium: rems(0.8125),
                icon_large: rems(1.0),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::UiTheme;

    #[test]
    fn chrome_is_larger_than_captions_but_smaller_than_body_copy() {
        let typography = UiTheme::default().typography;

        assert!(typography.chrome.0 > typography.caption.0);
        assert!(typography.chrome.0 < typography.body.0);
    }

    #[test]
    fn typography_relative_icons_scale_with_the_root_font() {
        let typography = UiTheme::default().typography;

        assert_eq!(typography.icon_medium.0 * 16.0, 13.0);
        assert_eq!(typography.icon_medium.0 * 24.0, 19.5);
    }
}

impl UiTheme {
    pub fn init(cx: &mut App) {
        cx.set_global(Self::default());
    }

    pub fn get(cx: &App) -> &Self {
        cx.global::<Self>()
    }
}
